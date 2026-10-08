//! Device- and connection-addressed adapter over the existing opaque relay transport.
//! Discovery may be broadcast; authenticated control/media must never be broadcast
//! into a host or returned to whichever local client happened to send last.
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{
    collections::{HashMap, HashSet},
    fmt, io,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};

const MAX_PEERS: usize = 64;
const MAX_ROUTES: usize = 128;
const IDLE: Duration = Duration::from_secs(60);
// Discovery may retain endpoints between user actions. Reclaim only long-unused
// endpoints with no live outbound connection, never a currently used route.
const ENDPOINT_IDLE: Duration = Duration::from_secs(300);
const RECEIVE_ERROR_BACKOFF: Duration = Duration::from_millis(5);
const MAGIC: &[u8; 4] = b"RPMX";
const MAX_PAYLOAD: usize = 64000;
type Token = [u8; 16];
type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, PartialEq, Eq)]
pub struct RoutingKey([u8; 32]);
impl fmt::Debug for RoutingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoutingKey([redacted])")
    }
}
pub fn derive_routing_key(secret: &[u8]) -> RoutingKey {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(b"RemotePlay relay destination binding v1");
    RoutingKey(mac.finalize().into_bytes().into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Envelope {
    source: String,
    target: String,
    token: Token,
    response: bool,
    payload: Vec<u8>,
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
impl Envelope {
    fn encode(&self, key: &[u8; 32]) -> Option<Vec<u8>> {
        if !valid_id(&self.source) || !valid_id(&self.target) || self.payload.len() > MAX_PAYLOAD {
            return None;
        }
        let mut bytes =
            Vec::with_capacity(56 + self.source.len() + self.target.len() + self.payload.len());
        bytes.extend_from_slice(MAGIC);
        bytes.push(1);
        bytes.push(u8::from(self.response));
        bytes.extend_from_slice(&self.token);
        bytes.push(self.source.len() as u8);
        bytes.push(self.target.len() as u8);
        bytes.extend_from_slice(self.source.as_bytes());
        bytes.extend_from_slice(self.target.as_bytes());
        bytes.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&self.payload);
        let mut mac = HmacSha256::new_from_slice(key).ok()?;
        mac.update(&bytes);
        bytes.extend_from_slice(&mac.finalize().into_bytes());
        Some(bytes)
    }
    fn decode(bytes: &[u8], key: &[u8; 32]) -> Option<Self> {
        if bytes.len() < 58
            || bytes.len() > MAX_PAYLOAD + 314
            || &bytes[..4] != MAGIC
            || bytes[4] != 1
            || bytes[5] > 1
        {
            return None;
        }
        let (body, tag) = bytes.split_at(bytes.len() - 32);
        let mut mac = HmacSha256::new_from_slice(key).ok()?;
        mac.update(body);
        mac.verify_slice(tag).ok()?;
        let a = body[22] as usize;
        let b = body[23] as usize;
        let end = 24usize.checked_add(a)?.checked_add(b)?;
        let source = std::str::from_utf8(body.get(24..24 + a)?).ok()?.to_owned();
        let target = std::str::from_utf8(body.get(24 + a..end)?).ok()?.to_owned();
        let size = u16::from_be_bytes(body.get(end..end + 2)?.try_into().ok()?) as usize;
        if !valid_id(&source)
            || !valid_id(&target)
            || body.len() != end + 2 + size
            || size > MAX_PAYLOAD
        {
            return None;
        }
        Some(Self {
            source,
            target,
            token: body[6..22].try_into().ok()?,
            response: body[5] != 0,
            payload: body[end + 2..].to_vec(),
        })
    }
}

#[derive(Clone)]
pub struct PeerRelayRoutes {
    tx: mpsc::Sender<Event>,
}
impl fmt::Debug for PeerRelayRoutes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PeerRelayRoutes(device-and-connection-isolated)")
    }
}
impl PeerRelayRoutes {
    pub async fn endpoint(&self, peer: &str) -> io::Result<SocketAddr> {
        if !valid_id(peer) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid peer ID",
            ));
        }
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Event::Endpoint(peer.to_owned(), tx))
            .await
            .map_err(|_| io::Error::other("relay router stopped"))?;
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "relay endpoint allocation timed out",
                )
            })?
            .map_err(|_| io::Error::other("relay router stopped"))?
    }
}
enum Event {
    Endpoint(String, oneshot::Sender<io::Result<SocketAddr>>),
    Client {
        peer: String,
        local: SocketAddr,
        listener_endpoint: SocketAddr,
        payload: Vec<u8>,
    },
    Host {
        peer: String,
        token: Token,
        payload: Vec<u8>,
    },
}
#[derive(Clone)]
enum ListenerKind {
    Client {
        peer: String,
    },
    Host {
        peer: String,
        token: Token,
        host: SocketAddr,
    },
}

fn recoverable_datagram_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
    )
}

struct Listener {
    socket: Arc<UdpSocket>,
    task: JoinHandle<()>,
    kind: ListenerKind,
    touched: Instant,
    failure: Arc<Mutex<Option<io::ErrorKind>>>,
}
impl Listener {
    fn new(
        socket: Arc<UdpSocket>,
        kind: ListenerKind,
        tx: mpsc::Sender<Event>,
    ) -> io::Result<Self> {
        let failure = Arc::new(Mutex::new(None));
        let task = Self::spawn_reader(socket.clone(), kind.clone(), tx, failure.clone())?;
        Ok(Self {
            socket,
            task,
            kind,
            touched: Instant::now(),
            failure,
        })
    }
    fn spawn_reader(
        socket: Arc<UdpSocket>,
        kind: ListenerKind,
        tx: mpsc::Sender<Event>,
        failure: Arc<Mutex<Option<io::ErrorKind>>>,
    ) -> io::Result<JoinHandle<()>> {
        let listener_endpoint = socket.local_addr()?;
        Ok(tokio::spawn(async move {
            let mut buf = vec![0; MAX_PAYLOAD + 1];
            loop {
                let (n, from) = match socket.recv_from(&mut buf).await {
                    Ok(packet) => packet,
                    Err(error) if recoverable_datagram_error(&error) => {
                        // UDP ICMP feedback is about an earlier peer datagram,
                        // not closure of this listener. Yield even on repeated errors.
                        tokio::time::sleep(RECEIVE_ERROR_BACKOFF).await;
                        continue;
                    }
                    Err(error) => {
                        *failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.kind());
                        eprintln!("Relay local listener stopped: {error}");
                        return;
                    }
                };
                if n > MAX_PAYLOAD {
                    continue;
                }
                let event = match &kind {
                    ListenerKind::Client { peer } if from.ip().is_loopback() => Event::Client {
                        peer: peer.clone(),
                        local: from,
                        listener_endpoint,
                        payload: buf[..n].to_vec(),
                    },
                    ListenerKind::Host { peer, token, host } if from == *host => Event::Host {
                        peer: peer.clone(),
                        token: *token,
                        payload: buf[..n].to_vec(),
                    },
                    _ => continue,
                };
                if tx.send(event).await.is_err() {
                    return;
                }
            }
        }))
    }
    fn ensure_reader(&mut self, tx: &mpsc::Sender<Event>) -> io::Result<()> {
        // The healthy video path performs only the task-state atomic check.
        // Never lock error metadata for each media datagram.
        if !self.task.is_finished() {
            return Ok(());
        }
        if let Some(kind) = *self.failure.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(io::Error::new(
                kind,
                "relay listener encountered a local socket error",
            ));
        }
        if self.task.is_finished() {
            if tx.is_closed() {
                return Err(io::Error::other("relay router stopped"));
            }
            // Keep socket, local port, and route token unchanged. A live peer's
            // session crypto is bound to this exact endpoint.
            self.task = Self::spawn_reader(
                self.socket.clone(),
                self.kind.clone(),
                tx.clone(),
                self.failure.clone(),
            )?;
        }
        Ok(())
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Outgoing {
    peer: String,
    local: SocketAddr,
    seen: Instant,
}
struct Incoming {
    listener: Listener,
    seen: Instant,
}

pub struct RoutedRelay {
    local_id: String,
    key: RoutingKey,
    host: Option<SocketAddr>,
    hub: UdpSocket,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    peers: HashMap<String, Listener>,
    outgoing: HashMap<Token, Outgoing>,
    clients: HashMap<(String, SocketAddr), Token>,
    incoming: HashMap<(String, Token), Incoming>,
}
impl RoutedRelay {
    pub async fn bind(
        local_id: String,
        key: RoutingKey,
        host: Option<SocketAddr>,
    ) -> io::Result<Self> {
        if !valid_id(&local_id) || host.is_some_and(|a| !a.ip().is_loopback()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "relay identity or host endpoint invalid",
            ));
        }
        let (tx, rx) = mpsc::channel(128);
        Ok(Self {
            local_id,
            key,
            host,
            hub: UdpSocket::bind("127.0.0.1:0").await?,
            tx,
            rx,
            peers: HashMap::new(),
            outgoing: HashMap::new(),
            clients: HashMap::new(),
            incoming: HashMap::new(),
        })
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.hub.local_addr()
    }
    pub fn routes(&self) -> PeerRelayRoutes {
        PeerRelayRoutes {
            tx: self.tx.clone(),
        }
    }
    async fn peer_endpoint(&mut self, peer: String) -> io::Result<SocketAddr> {
        if peer == self.local_id || !valid_id(&peer) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid remote peer",
            ));
        }
        if let Some(p) = self.peers.get_mut(&peer) {
            p.ensure_reader(&self.tx)?;
            p.touched = Instant::now();
            return p.socket.local_addr();
        }
        self.expire();
        if self.peers.len() >= MAX_PEERS {
            return Err(io::Error::other("relay peer limit reached"));
        }
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
        let addr = socket.local_addr()?;
        let listener = Listener::new(
            socket,
            ListenerKind::Client { peer: peer.clone() },
            self.tx.clone(),
        )?;
        self.peers.insert(peer, listener);
        Ok(addr)
    }
    async fn send(
        &self,
        relay: SocketAddr,
        peer: String,
        token: Token,
        response: bool,
        payload: Vec<u8>,
    ) {
        if let Some(bytes) = (Envelope {
            source: self.local_id.clone(),
            target: peer,
            token,
            response,
            payload,
        })
        .encode(&self.key.0)
        {
            let _ = self.hub.send_to(&bytes, relay).await;
        }
    }
    async fn receive(&mut self, envelope: Envelope) {
        if envelope.target != self.local_id || envelope.source == self.local_id {
            return;
        }
        if envelope.response {
            let Some(route) = self.outgoing.get_mut(&envelope.token) else {
                return;
            };
            if route.peer != envelope.source {
                return;
            }
            route.seen = Instant::now();
            if let Some(peer) = self.peers.get_mut(&route.peer) {
                peer.touched = Instant::now();
                if peer.ensure_reader(&self.tx).is_err() {
                    return;
                }
                let _ = peer.socket.send_to(&envelope.payload, route.local).await;
            }
        } else {
            let Some(host) = self.host else { return };
            let key = (envelope.source.clone(), envelope.token);
            if !self.incoming.contains_key(&key) {
                self.expire();
                if self.incoming.len() >= MAX_ROUTES {
                    return;
                }
                let Ok(socket) = UdpSocket::bind(if host.is_ipv4() {
                    "127.0.0.1:0"
                } else {
                    "[::1]:0"
                })
                .await
                else {
                    return;
                };
                let socket = Arc::new(socket);
                let kind = ListenerKind::Host {
                    peer: envelope.source.clone(),
                    token: envelope.token,
                    host,
                };
                let Ok(listener) = Listener::new(socket, kind, self.tx.clone()) else {
                    return;
                };
                self.incoming.insert(
                    key.clone(),
                    Incoming {
                        listener,
                        seen: Instant::now(),
                    },
                );
            }
            if let Some(route) = self.incoming.get_mut(&key) {
                if route.listener.ensure_reader(&self.tx).is_err() {
                    return;
                }
                route.seen = Instant::now();
                let _ = route.listener.socket.send_to(&envelope.payload, host).await;
            }
        }
    }
    fn expire(&mut self) {
        self.expire_at(Instant::now());
    }
    fn expire_at(&mut self, now: Instant) {
        self.outgoing
            .retain(|_, v| now.saturating_duration_since(v.seen) < IDLE);
        self.clients
            .retain(|_, token| self.outgoing.contains_key(token));
        self.incoming
            .retain(|_, v| now.saturating_duration_since(v.seen) < IDLE);
        let active: HashSet<_> = self.outgoing.values().map(|v| v.peer.as_str()).collect();
        self.peers.retain(|peer, listener| {
            active.contains(peer.as_str())
                || now.saturating_duration_since(listener.touched) < ENDPOINT_IDLE
        });
        // Repair unexpectedly completed tasks before another packet depends on
        // their receive half. Local permission/configuration failures stay errors.
        for listener in self.peers.values_mut() {
            let _ = listener.ensure_reader(&self.tx);
        }
        for incoming in self.incoming.values_mut() {
            let _ = incoming.listener.ensure_reader(&self.tx);
        }
    }
    pub async fn run(
        mut self,
        relay: SocketAddr,
        mut cancel: broadcast::Receiver<()>,
    ) -> io::Result<()> {
        if !relay.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "relay adapter must be loopback",
            ));
        }
        let mut buf = vec![0; 65536];
        let mut cleanup = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _=cancel.recv()=>break,
                _=cleanup.tick()=>self.expire(),
                event=self.rx.recv()=>match event{
                    None=>break,
                    Some(Event::Endpoint(peer,reply))=>{if reply.is_closed(){continue;}let result=self.peer_endpoint(peer).await;let _=reply.send(result);},
                    Some(Event::Client{peer,local,listener_endpoint,payload})=>{
                        // An event from a retired listener cannot recreate an old
                        // client route on a newly allocated endpoint for this peer.
                        let Some(listener)=self.peers.get_mut(&peer) else {continue;};
                        if listener.socket.local_addr().ok()!=Some(listener_endpoint){continue;}
                        listener.touched=Instant::now();
                        let client=(peer.clone(),local);
                        let token=if let Some(token)=self.clients.get(&client){*token}else{
                            if self.outgoing.len()>=MAX_ROUTES{self.expire();if self.outgoing.len()>=MAX_ROUTES{continue;}}
                            let mut token=rand::random::<Token>();while self.outgoing.contains_key(&token){token=rand::random();}
                            self.clients.insert(client,token);self.outgoing.insert(token,Outgoing{peer:peer.clone(),local,seen:Instant::now()});token
                        };
                        self.outgoing.get_mut(&token).unwrap().seen=Instant::now();self.send(relay,peer,token,false,payload).await;
                    },
                    Some(Event::Host{peer,token,payload})=>{
                        if let Some(route)=self.incoming.get_mut(&(peer.clone(),token)){route.seen=Instant::now();self.send(relay,peer,token,true,payload).await;}
                    },
                },
                received=self.hub.recv_from(&mut buf)=>{
                    let (n,from)=match received {
                        Ok(value)=>value,
                        Err(error) if recoverable_datagram_error(&error)=>{tokio::time::sleep(RECEIVE_ERROR_BACKOFF).await;continue;},
                        Err(error)=>return Err(error),
                    };if from!=relay{continue;}
                    if let Some(envelope)=Envelope::decode(&buf[..n],&self.key.0){self.receive(envelope).await;}
                    // Legacy broadcast datagrams are deliberately rejected, never guessed/routed.
                },
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::{
        BoundTcpRelayServer, BoundWebSocketRelayTunnel, TcpRelayServerConfig,
        WebSocketRelayTunnelConfig,
    };
    fn key() -> RoutingKey {
        derive_routing_key(b"isolated test network, not user credentials")
    }
    fn packet() -> Envelope {
        Envelope {
            source: "a".into(),
            target: "b".into(),
            token: [7; 16],
            response: false,
            payload: b"video-session-1".to_vec(),
        }
    }
    #[test]
    fn envelope_is_destination_bound() {
        let p = packet();
        let bytes = p.encode(&key().0).unwrap();
        assert_eq!(Envelope::decode(&bytes, &key().0), Some(p));
        let mut changed = bytes;
        changed[25] = b'c';
        assert!(Envelope::decode(&changed, &key().0).is_none());
    }
    #[test]
    fn wrong_network_and_legacy_packets_are_rejected() {
        let bytes = packet().encode(&key().0).unwrap();
        assert!(Envelope::decode(&bytes, &derive_routing_key(b"other").0).is_none());
        assert!(Envelope::decode(b"legacy raw screen packet", &key().0).is_none());
    }
    #[test]
    fn malformed_truncated_and_extra_bytes_are_rejected() {
        let bytes = packet().encode(&key().0).unwrap();
        for n in 0..bytes.len() {
            assert!(Envelope::decode(&bytes[..n], &key().0).is_none());
        }
        let mut extra = bytes;
        extra.push(0);
        assert!(Envelope::decode(&extra, &key().0).is_none());
    }
    #[test]
    fn oversized_payload_and_invalid_ids_are_rejected() {
        let mut p = packet();
        p.payload = vec![0; MAX_PAYLOAD + 1];
        assert!(p.encode(&key().0).is_none());
        p.payload.clear();
        p.target = "../bad".into();
        assert!(p.encode(&key().0).is_none());
    }
    #[test]
    fn debug_never_prints_authentication_key() {
        assert_eq!(format!("{:?}", key()), "RoutingKey([redacted])");
    }
    struct TestNode {
        routes: PeerRelayRoutes,
        host: Arc<UdpSocket>,
        tasks: Vec<JoinHandle<()>>,
    }
    impl Drop for TestNode {
        fn drop(&mut self) {
            for task in &self.tasks {
                task.abort();
            }
        }
    }
    async fn node(id: &str, server: SocketAddr, cancel: &broadcast::Sender<()>) -> TestNode {
        let host = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let router = RoutedRelay::bind(id.into(), key(), Some(host.local_addr().unwrap()))
            .await
            .unwrap();
        let routes = router.routes();
        let tunnel = BoundWebSocketRelayTunnel::bind(
            WebSocketRelayTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                format!("ws://{server}/relay"),
                "route-regression",
                id,
            )
            .unwrap()
            .with_local_target_addr(router.local_addr().unwrap()),
        )
        .await
        .unwrap();
        let endpoint = tunnel.local_addr().unwrap();
        let rx = cancel.subscribe();
        let rxc = cancel.subscribe();
        let tasks = vec![
            tokio::spawn(async move {
                tunnel.run(rx).await.unwrap();
            }),
            tokio::spawn(async move {
                router.run(endpoint, rxc).await.unwrap();
            }),
        ];
        TestNode {
            routes,
            host,
            tasks,
        }
    }
    async fn recv(socket: &UdpSocket) -> (Vec<u8>, SocketAddr) {
        let mut data = vec![0; 65535];
        let (n, a) = tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut data))
            .await
            .expect("routing timeout")
            .unwrap();
        data.truncate(n);
        (data, a)
    }
    #[tokio::test]
    async fn three_devices_two_clients_and_reverse_control_never_cross_route() {
        let server =
            BoundTcpRelayServer::bind(TcpRelayServerConfig::new("127.0.0.1:0".parse().unwrap()))
                .await
                .unwrap();
        let address = server.local_addr().unwrap();
        let (cancel, _) = broadcast::channel(8);
        let rx = cancel.subscribe();
        let server_task = tokio::spawn(async move {
            server.run_websocket(rx).await.unwrap();
        });
        let a = node("ho5", address, &cancel).await;
        let b = node("studio", address, &cancel).await;
        let c = node("macbook", address, &cancel).await;
        let ab = a.routes.endpoint("studio").await.unwrap();
        let ac = a.routes.endpoint("macbook").await.unwrap();
        let ba = b.routes.endpoint("ho5").await.unwrap();
        assert_ne!(ab, ac);
        let left = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let right = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let reverse = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        // Identical subscription IDs in three directions must not share routing identity.
        for i in 0..12 {
            let pa = format!("studio-click-session1-{i}").into_bytes();
            let pb = format!("macbook-key-session1-{i}").into_bytes();
            let pc = format!("ho5-reverse-session1-{i}").into_bytes();
            left.send_to(&pa, ab).await.unwrap();
            right.send_to(&pb, ac).await.unwrap();
            reverse.send_to(&pc, ba).await.unwrap();
            let (got_b, addr_b) = recv(&b.host).await;
            let (got_c, addr_c) = recv(&c.host).await;
            let (got_a, addr_a) = recv(&a.host).await;
            assert_eq!(got_b, pa);
            assert_eq!(got_c, pb);
            assert_eq!(got_a, pc);
            b.host.send_to(b"STUDIO_FRAME", addr_b).await.unwrap();
            c.host.send_to(b"MACBOOK_FRAME", addr_c).await.unwrap();
            a.host.send_to(b"HO5_FRAME", addr_a).await.unwrap();
            assert_eq!(recv(&left).await, (b"STUDIO_FRAME".to_vec(), ab));
            assert_eq!(recv(&right).await, (b"MACBOOK_FRAME".to_vec(), ac));
            assert_eq!(recv(&reverse).await, (b"HO5_FRAME".to_vec(), ba));
        }
        // A second local connection to the SAME peer has a different return socket at the host.
        right.send_to(b"second-studio-client", ab).await.unwrap();
        left.send_to(b"first-studio-client", ab).await.unwrap();
        let (one, from_one) = recv(&b.host).await;
        let (two, from_two) = recv(&b.host).await;
        assert_ne!(from_one, from_two);
        b.host.send_to(&one, from_one).await.unwrap();
        b.host.send_to(&two, from_two).await.unwrap();
        assert_eq!(recv(&left).await.0, b"first-studio-client");
        assert_eq!(recv(&right).await.0, b"second-studio-client");
        let _ = cancel.send(());
        server_task.await.unwrap();
    }
    #[tokio::test]
    async fn wrong_target_and_unknown_response_do_not_reach_a_host() {
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut router = RoutedRelay::bind("a".into(), key(), Some(host.local_addr().unwrap()))
            .await
            .unwrap();
        let mut p = packet();
        p.target = "c".into();
        router.receive(p).await;
        assert!(router.incoming.is_empty());
        let mut p = packet();
        p.target = "a".into();
        p.source = "b".into();
        p.response = true;
        router.receive(p).await;
        assert!(router.incoming.is_empty());
    }
    #[tokio::test]
    async fn expired_connections_are_removed_without_reusing_tokens() {
        let mut router = RoutedRelay::bind("a".into(), key(), None).await.unwrap();
        let local = "127.0.0.1:99".parse().unwrap();
        router.outgoing.insert(
            [1; 16],
            Outgoing {
                peer: "b".into(),
                local,
                seen: Instant::now() - IDLE - Duration::from_secs(1),
            },
        );
        router.clients.insert(("b".into(), local), [1; 16]);
        router.expire();
        assert!(router.outgoing.is_empty());
        assert!(router.clients.is_empty());
    }
}

#[cfg(test)]
mod listener_lifecycle_regression {
    use super::*;
    fn test_key() -> RoutingKey {
        derive_routing_key(b"local synthetic listener lifecycle only")
    }
    async fn stopped(task: &JoinHandle<()>) {
        task.abort();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn dead_peer_reader_is_rearmed_at_the_same_endpoint() {
        let mut router = RoutedRelay::bind("local-test".into(), test_key(), None)
            .await
            .unwrap();
        let peer = "other-test".to_owned();
        let endpoint = router.peer_endpoint(peer.clone()).await.unwrap();
        let socket = router.peers[&peer].socket.clone();
        stopped(&router.peers[&peer].task).await;
        let reconnected = router.peer_endpoint(peer.clone()).await.unwrap();
        assert_eq!(
            endpoint, reconnected,
            "repair must not invalidate discovery endpoint"
        );
        assert!(
            Arc::ptr_eq(&socket, &router.peers[&peer].socket),
            "keep the same datagram socket"
        );
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(b"synthetic reconnect", endpoint)
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_millis(400), router.rx.recv())
            .await
            .expect("a cached endpoint must have a live reader")
            .unwrap();
        assert!(
            matches!(event,Event::Client{peer:actual,local,payload,..} if actual==peer && local==client.local_addr().unwrap() && payload==b"synthetic reconnect")
        );
    }
    #[tokio::test]
    async fn dead_host_reader_is_rearmed_without_changing_the_session_route() {
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = host.local_addr().unwrap();
        let mut router = RoutedRelay::bind("host-test".into(), test_key(), Some(addr))
            .await
            .unwrap();
        let packet = Envelope {
            source: "client-test".into(),
            target: "host-test".into(),
            token: [17; 16],
            response: false,
            payload: b"synthetic request".to_vec(),
        };
        router.receive(packet.clone()).await;
        let mut bytes = [0u8; 64];
        let (_, first) = host.recv_from(&mut bytes).await.unwrap();
        let key = (packet.source.clone(), packet.token);
        stopped(&router.incoming[&key].listener.task).await;
        router.receive(packet.clone()).await;
        let (_, second) = host.recv_from(&mut bytes).await.unwrap();
        assert_eq!(
            first, second,
            "host crypto remains bound to its established socket"
        );
        host.send_to(b"synthetic reply", second).await.unwrap();
        let event = tokio::time::timeout(Duration::from_millis(400), router.rx.recv())
            .await
            .expect("an existing inbound route must have a live response reader")
            .unwrap();
        assert!(
            matches!(event,Event::Host{peer,token,payload} if peer==packet.source && token==packet.token && payload==b"synthetic reply")
        );
    }
}

#[cfg(test)]
mod endpoint_cache_regression {
    use super::*;
    fn test_key() -> RoutingKey {
        derive_routing_key(b"local endpoint cache fixture")
    }
    #[test]
    fn delayed_udp_errors_are_not_listener_closure() {
        for kind in [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionRefused,
            io::ErrorKind::Interrupted,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::TimedOut,
        ] {
            assert!(recoverable_datagram_error(&io::Error::from(kind)));
        }
    }
    #[test]
    fn permission_and_configuration_failures_are_not_ignored() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::NotConnected,
            io::ErrorKind::AddrNotAvailable,
        ] {
            assert!(!recoverable_datagram_error(&io::Error::from(kind)));
        }
    }
    #[tokio::test]
    async fn failed_local_permissions_remain_an_explicit_endpoint_error() {
        let mut router = RoutedRelay::bind("a".into(), test_key(), None)
            .await
            .unwrap();
        router.peer_endpoint("b".into()).await.unwrap();
        *router.peers["b"].failure.lock().unwrap() = Some(io::ErrorKind::PermissionDenied);
        router.peers["b"].task.abort();
        while !router.peers["b"].task.is_finished() {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            router.peer_endpoint("b".into()).await.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
    #[tokio::test]
    async fn inactive_historical_peers_do_not_permanently_fill_the_cache() {
        let mut router = RoutedRelay::bind("local".into(), test_key(), None)
            .await
            .unwrap();
        for i in 0..MAX_PEERS {
            router.peer_endpoint(format!("peer-{i}")).await.unwrap();
        }
        assert!(router.peer_endpoint("over-limit".into()).await.is_err());
        let now = Instant::now();
        for listener in router.peers.values_mut() {
            listener.touched = now - ENDPOINT_IDLE - Duration::from_secs(1);
        }
        router.expire_at(now);
        assert!(router.peers.is_empty());
        assert!(router.peer_endpoint("next-peer".into()).await.is_ok());
    }
    #[tokio::test]
    async fn active_connection_and_discovery_refresh_keep_their_exact_endpoint() {
        let mut router = RoutedRelay::bind("local".into(), test_key(), None)
            .await
            .unwrap();
        let active = router.peer_endpoint("active".into()).await.unwrap();
        let fresh = router.peer_endpoint("refreshed".into()).await.unwrap();
        let now = Instant::now();
        router.peers.get_mut("active").unwrap().touched =
            now - ENDPOINT_IDLE - Duration::from_secs(1);
        let local = "127.0.0.1:32100".parse().unwrap();
        router.outgoing.insert(
            [4; 16],
            Outgoing {
                peer: "active".into(),
                local,
                seen: now,
            },
        );
        router.clients.insert(("active".into(), local), [4; 16]);
        router.expire_at(now);
        assert_eq!(router.peer_endpoint("active".into()).await.unwrap(), active);
        assert_eq!(
            router.peer_endpoint("refreshed".into()).await.unwrap(),
            fresh
        );
        assert_eq!(router.clients[&("active".into(), local)], [4; 16]);
    }
    #[tokio::test]
    async fn a_local_host_restart_does_not_kill_its_existing_reply_route() {
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = host.local_addr().unwrap();
        let mut router = RoutedRelay::bind("receiver".into(), test_key(), Some(addr))
            .await
            .unwrap();
        let packet = Envelope {
            source: "sender".into(),
            target: "receiver".into(),
            token: [29; 16],
            response: false,
            payload: b"local fixture only".to_vec(),
        };
        router.receive(packet.clone()).await;
        let mut bytes = [0u8; 64];
        let (_, route) = tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        drop(host);
        // On Windows an ICMP for this unavailable target can surface on the
        // next recv_from. Neither the established local port nor token may change.
        router.receive(packet.clone()).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let restarted = UdpSocket::bind(addr).await.unwrap();
        router.receive(packet.clone()).await;
        let (_, same_route) =
            tokio::time::timeout(Duration::from_secs(2), restarted.recv_from(&mut bytes))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(route, same_route);
        restarted
            .send_to(b"host restored", same_route)
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(2), router.rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(event,Event::Host{peer,token,payload} if peer==packet.source && token==packet.token && payload==b"host restored")
        );
    }

    #[tokio::test]
    async fn cleanup_repairs_a_finished_reader_without_rekeying() {
        let mut router = RoutedRelay::bind("local".into(), test_key(), None)
            .await
            .unwrap();
        let endpoint = router.peer_endpoint("peer".into()).await.unwrap();
        router.peers["peer"].task.abort();
        while !router.peers["peer"].task.is_finished() {
            tokio::task::yield_now().await;
        }
        router.expire();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(b"after cleanup", endpoint).await.unwrap();
        let event = tokio::time::timeout(Duration::from_millis(400), router.rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event,Event::Client{payload,..} if payload==b"after cleanup"));
    }
}
