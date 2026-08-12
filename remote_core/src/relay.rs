use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::sync::{Mutex, mpsc};

const RELAY_MAGIC: &[u8; 4] = b"RPR1";
const REGISTER_PACKET: u8 = 1;
const DATA_PACKET: u8 = 2;
const MAX_ID_LEN: usize = 255;
const MAX_PACKET_LEN: usize = 65_535;
const MAX_TCP_FRAME_LEN: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpRelayServerConfig {
    pub bind_addr: SocketAddr,
    pub peer_ttl: Duration,
    pub log_events: bool,
}

impl UdpRelayServerConfig {
    pub fn new(bind_addr: SocketAddr) -> Self {
        Self {
            bind_addr,
            peer_ttl: Duration::from_secs(15),
            log_events: false,
        }
    }

    pub fn with_event_logging(mut self, log_events: bool) -> Self {
        self.log_events = log_events;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpRelayTunnelConfig {
    pub bind_addr: SocketAddr,
    pub relay_addr: SocketAddr,
    pub group_id: String,
    pub peer_id: String,
    pub local_target_addr: Option<SocketAddr>,
    pub register_interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpRelayServerConfig {
    pub bind_addr: SocketAddr,
    pub log_events: bool,
}

impl TcpRelayServerConfig {
    pub fn new(bind_addr: SocketAddr) -> Self {
        Self {
            bind_addr,
            log_events: false,
        }
    }

    pub fn with_event_logging(mut self, log_events: bool) -> Self {
        self.log_events = log_events;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpRelayTunnelConfig {
    pub bind_addr: SocketAddr,
    pub relay_addr: SocketAddr,
    pub group_id: String,
    pub peer_id: String,
    pub local_target_addr: Option<SocketAddr>,
    pub register_interval: Duration,
    pub log_events: bool,
}

impl TcpRelayTunnelConfig {
    pub fn new(
        bind_addr: SocketAddr,
        relay_addr: SocketAddr,
        group_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> Result<Self, RelayConfigError> {
        let group_id = validate_relay_id("group_id", group_id.into())?;
        let peer_id = validate_relay_id("peer_id", peer_id.into())?;
        Ok(Self {
            bind_addr,
            relay_addr,
            group_id,
            peer_id,
            local_target_addr: None,
            register_interval: Duration::from_secs(1),
            log_events: false,
        })
    }

    pub fn with_local_target_addr(mut self, local_target_addr: SocketAddr) -> Self {
        self.local_target_addr = Some(local_target_addr);
        self
    }

    pub fn with_event_logging(mut self, log_events: bool) -> Self {
        self.log_events = log_events;
        self
    }
}

impl UdpRelayTunnelConfig {
    pub fn new(
        bind_addr: SocketAddr,
        relay_addr: SocketAddr,
        group_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> Result<Self, RelayConfigError> {
        let group_id = validate_relay_id("group_id", group_id.into())?;
        let peer_id = validate_relay_id("peer_id", peer_id.into())?;
        Ok(Self {
            bind_addr,
            relay_addr,
            group_id,
            peer_id,
            local_target_addr: None,
            register_interval: Duration::from_secs(1),
        })
    }

    pub fn with_local_target_addr(mut self, local_target_addr: SocketAddr) -> Self {
        self.local_target_addr = Some(local_target_addr);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayConfigError {
    EmptyId(&'static str),
    IdTooLong { field: &'static str, len: usize },
}

impl std::fmt::Display for RelayConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyId(field) => write!(f, "{field} is empty"),
            Self::IdTooLong { field, len } => {
                write!(f, "{field} is too long: {len} bytes, max {MAX_ID_LEN}")
            }
        }
    }
}

impl std::error::Error for RelayConfigError {}

pub struct BoundUdpRelayServer {
    socket: UdpSocket,
    config: UdpRelayServerConfig,
}

impl BoundUdpRelayServer {
    pub async fn bind(config: UdpRelayServerConfig) -> io::Result<Self> {
        let socket = UdpSocket::bind(config.bind_addr).await?;
        Ok(Self { socket, config })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub async fn run(self, mut cancel_rx: broadcast::Receiver<()>) -> io::Result<()> {
        let mut peers = RelayPeerTable::new(self.config.peer_ttl);
        let mut buf = vec![0u8; MAX_PACKET_LEN];

        loop {
            tokio::select! {
                _ = cancel_rx.recv() => return Ok(()),
                received = self.socket.recv_from(&mut buf) => {
                    let (len, addr) = received?;
                    peers.prune_expired();
                    let Some(packet) = RelayPacket::decode(&buf[..len]) else {
                        continue;
                    };
                    match packet {
                        RelayPacket::Register { group_id, peer_id } => {
                            if self.config.log_events {
                                println!("Relay register group={group_id} peer={peer_id} addr={addr}");
                            }
                            peers.register(group_id, peer_id, addr);
                        }
                        RelayPacket::Data { group_id, peer_id, payload } => {
                            peers.register(group_id.clone(), peer_id.clone(), addr);
                            let targets = peers.forward_targets(&group_id, addr);
                            if self.config.log_events {
                                println!(
                                    "Relay data group={group_id} peer={peer_id} bytes={} targets={}",
                                    payload.len(),
                                    targets.len()
                                );
                            }
                            let packet = RelayPacket::Data {
                                group_id: group_id.clone(),
                                peer_id,
                                payload,
                            };
                            let encoded = packet.encode();
                            for target in targets {
                                let _ = self.socket.send_to(&encoded, target).await;
                            }
                        }
                    }
                }
            }
        }
    }
}

pub struct BoundUdpRelayTunnel {
    socket: UdpSocket,
    config: UdpRelayTunnelConfig,
}

pub struct BoundTcpRelayServer {
    listener: TcpListener,
    config: TcpRelayServerConfig,
}

impl BoundTcpRelayServer {
    pub async fn bind(config: TcpRelayServerConfig) -> io::Result<Self> {
        let listener = TcpListener::bind(config.bind_addr).await?;
        Ok(Self { listener, config })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub async fn run(self, mut cancel_rx: broadcast::Receiver<()>) -> io::Result<()> {
        let state = Arc::new(Mutex::new(TcpRelayServerState::default()));

        loop {
            tokio::select! {
                _ = cancel_rx.recv() => return Ok(()),
                accepted = self.listener.accept() => {
                    let (stream, addr) = accepted?;
                    let state = state.clone();
                    let log_events = self.config.log_events;
                    tokio::spawn(async move {
                        if let Err(err) = run_tcp_relay_connection(stream, addr, state, log_events).await {
                            eprintln!("TCP relay connection ended: {err}");
                        }
                    });
                }
            }
        }
    }
}

pub struct BoundTcpRelayTunnel {
    socket: UdpSocket,
    config: TcpRelayTunnelConfig,
}

impl BoundTcpRelayTunnel {
    pub async fn bind(config: TcpRelayTunnelConfig) -> io::Result<Self> {
        let socket = UdpSocket::bind(config.bind_addr).await?;
        Ok(Self { socket, config })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub async fn run(self, mut cancel_rx: broadcast::Receiver<()>) -> io::Result<()> {
        loop {
            tokio::select! {
                _ = cancel_rx.recv() => return Ok(()),
                connected = TcpStream::connect(self.config.relay_addr) => {
                    match connected {
                        Ok(stream) => {
                            stream.set_nodelay(true)?;
                            if self.config.log_events {
                                println!("TCP relay tunnel connected to {}", self.config.relay_addr);
                            }
                            match self.run_connected(stream, &mut cancel_rx).await {
                                Ok(()) => return Ok(()),
                                Err(err) => {
                                    if self.config.log_events {
                                        eprintln!("TCP relay tunnel disconnected from {}: {err}", self.config.relay_addr);
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            if self.config.log_events {
                                eprintln!("TCP relay tunnel connect to {} failed: {err}", self.config.relay_addr);
                            }
                        }
                    }
                }
            }

            tokio::select! {
                _ = cancel_rx.recv() => return Ok(()),
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }

    async fn run_connected(
        &self,
        stream: TcpStream,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> io::Result<()> {
        let (mut reader, mut writer) = stream.into_split();
        let mut buf = vec![0u8; MAX_PACKET_LEN];
        let mut local_target_addr = self.config.local_target_addr;
        let register_packet = RelayPacket::Register {
            group_id: self.config.group_id.clone(),
            peer_id: self.config.peer_id.clone(),
        }
        .encode();
        write_tcp_frame(&mut writer, &register_packet).await?;
        let mut register_interval = tokio::time::interval(self.config.register_interval);

        loop {
            tokio::select! {
                _ = cancel_rx.recv() => return Ok(()),
                _ = register_interval.tick() => {
                    write_tcp_frame(&mut writer, &register_packet).await?;
                }
                frame = read_tcp_frame(&mut reader) => {
                    let frame = frame?;
                    if let Some(RelayPacket::Data { payload, .. }) = RelayPacket::decode(&frame)
                        && let Some(target) = local_target_addr
                    {
                        if self.config.log_events {
                            println!("TCP relay tunnel inbound bytes={} target={target}", payload.len());
                        }
                        let _ = self.socket.send_to(&payload, target).await;
                    }
                }
                received = self.socket.recv_from(&mut buf) => {
                    let (len, addr) = received?;
                    local_target_addr = Some(addr);
                    if self.config.log_events {
                        println!("TCP relay tunnel outbound bytes={len} source={addr}");
                    }
                    let encoded = RelayPacket::Data {
                        group_id: self.config.group_id.clone(),
                        peer_id: self.config.peer_id.clone(),
                        payload: buf[..len].to_vec(),
                    }
                    .encode();
                    write_tcp_frame(&mut writer, &encoded).await?;
                }
            }
        }
    }
}

impl BoundUdpRelayTunnel {
    pub async fn bind(config: UdpRelayTunnelConfig) -> io::Result<Self> {
        let socket = UdpSocket::bind(config.bind_addr).await?;
        Ok(Self { socket, config })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub async fn run(self, mut cancel_rx: broadcast::Receiver<()>) -> io::Result<()> {
        let mut buf = vec![0u8; MAX_PACKET_LEN];
        let mut local_target_addr = self.config.local_target_addr;
        let register_packet = RelayPacket::Register {
            group_id: self.config.group_id.clone(),
            peer_id: self.config.peer_id.clone(),
        }
        .encode();
        let mut register_interval = tokio::time::interval(self.config.register_interval);
        let _ = self
            .socket
            .send_to(&register_packet, self.config.relay_addr)
            .await;

        loop {
            tokio::select! {
                _ = cancel_rx.recv() => return Ok(()),
                _ = register_interval.tick() => {
                    let _ = self.socket.send_to(&register_packet, self.config.relay_addr).await;
                }
                received = self.socket.recv_from(&mut buf) => {
                    let (len, addr) = received?;
                    if addr == self.config.relay_addr {
                        if let Some(RelayPacket::Data { payload, .. }) = RelayPacket::decode(&buf[..len])
                            && let Some(target) = local_target_addr
                        {
                            let _ = self.socket.send_to(&payload, target).await;
                        }
                        continue;
                    }

                    local_target_addr = Some(addr);
                    let encoded = RelayPacket::Data {
                        group_id: self.config.group_id.clone(),
                        peer_id: self.config.peer_id.clone(),
                        payload: buf[..len].to_vec(),
                    }
                    .encode();
                    let _ = self.socket.send_to(&encoded, self.config.relay_addr).await;
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RelayPeerKey {
    group_id: String,
    peer_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RelayPeerState {
    addr: SocketAddr,
    last_seen: Instant,
}

struct RelayPeerTable {
    peer_ttl: Duration,
    peers: HashMap<RelayPeerKey, RelayPeerState>,
}

#[derive(Default)]
struct TcpRelayServerState {
    next_connection_id: u64,
    peers: HashMap<RelayPeerKey, TcpRelayPeerState>,
}

#[derive(Clone)]
struct TcpRelayPeerState {
    connection_id: u64,
    writer_tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl TcpRelayServerState {
    fn next_connection_id(&mut self) -> u64 {
        self.next_connection_id = self.next_connection_id.wrapping_add(1).max(1);
        self.next_connection_id
    }

    fn register(
        &mut self,
        group_id: String,
        peer_id: String,
        connection_id: u64,
        writer_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) {
        self.peers.insert(
            RelayPeerKey { group_id, peer_id },
            TcpRelayPeerState {
                connection_id,
                writer_tx,
            },
        );
    }

    fn forward_targets(
        &self,
        group_id: &str,
        source_connection_id: u64,
    ) -> Vec<mpsc::UnboundedSender<Vec<u8>>> {
        self.peers
            .iter()
            .filter(|(key, peer)| {
                key.group_id == group_id && peer.connection_id != source_connection_id
            })
            .map(|(_, peer)| peer.writer_tx.clone())
            .collect()
    }

    fn remove_connection(&mut self, connection_id: u64) {
        self.peers
            .retain(|_, peer| peer.connection_id != connection_id);
    }
}

async fn run_tcp_relay_connection(
    stream: TcpStream,
    addr: SocketAddr,
    state: Arc<Mutex<TcpRelayServerState>>,
    log_events: bool,
) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let (mut reader, writer) = stream.into_split();
    let (writer_tx, writer_rx) = mpsc::unbounded_channel();
    let connection_id = {
        let mut state = state.lock().await;
        state.next_connection_id()
    };
    let writer_task = tokio::spawn(run_tcp_relay_writer(writer, writer_rx));

    let result = async {
        loop {
            let frame = read_tcp_frame(&mut reader).await?;
            let Some(packet) = RelayPacket::decode(&frame) else {
                continue;
            };
            match packet {
                RelayPacket::Register { group_id, peer_id } => {
                    if log_events {
                        println!("TCP relay register group={group_id} peer={peer_id} addr={addr}");
                    }
                    state.lock().await.register(
                        group_id,
                        peer_id,
                        connection_id,
                        writer_tx.clone(),
                    );
                }
                RelayPacket::Data {
                    group_id,
                    peer_id,
                    payload,
                } => {
                    let encoded = RelayPacket::Data {
                        group_id: group_id.clone(),
                        peer_id: peer_id.clone(),
                        payload: payload.clone(),
                    }
                    .encode();
                    let targets = state.lock().await.forward_targets(&group_id, connection_id);
                    if log_events {
                        println!(
                            "TCP relay data group={group_id} peer={peer_id} bytes={} targets={}",
                            payload.len(),
                            targets.len()
                        );
                    }
                    for target in targets {
                        let _ = target.send(encoded.clone());
                    }
                }
            }
        }
    }
    .await;

    state.lock().await.remove_connection(connection_id);
    writer_task.abort();
    result
}

async fn run_tcp_relay_writer(
    mut writer: OwnedWriteHalf,
    mut writer_rx: mpsc::UnboundedReceiver<Vec<u8>>,
) -> io::Result<()> {
    while let Some(frame) = writer_rx.recv().await {
        write_tcp_frame(&mut writer, &frame).await?;
    }
    Ok(())
}

impl RelayPeerTable {
    fn new(peer_ttl: Duration) -> Self {
        Self {
            peer_ttl,
            peers: HashMap::new(),
        }
    }

    fn register(&mut self, group_id: String, peer_id: String, addr: SocketAddr) {
        let key = RelayPeerKey { group_id, peer_id };
        self.peers.insert(
            key,
            RelayPeerState {
                addr,
                last_seen: Instant::now(),
            },
        );
    }

    fn forward_targets(&self, group_id: &str, source_addr: SocketAddr) -> Vec<SocketAddr> {
        self.peers
            .iter()
            .filter(|(key, peer)| key.group_id == group_id && peer.addr != source_addr)
            .map(|(_, peer)| peer.addr)
            .collect()
    }

    fn prune_expired(&mut self) {
        let now = Instant::now();
        let peer_ttl = self.peer_ttl;
        self.peers
            .retain(|_, peer| now.duration_since(peer.last_seen) <= peer_ttl);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RelayPacket {
    Register {
        group_id: String,
        peer_id: String,
    },
    Data {
        group_id: String,
        peer_id: String,
        payload: Vec<u8>,
    },
}

impl RelayPacket {
    fn encode(&self) -> Vec<u8> {
        let (packet_type, group_id, peer_id, payload) = match self {
            Self::Register { group_id, peer_id } => (
                REGISTER_PACKET,
                group_id.as_str(),
                peer_id.as_str(),
                &[][..],
            ),
            Self::Data {
                group_id,
                peer_id,
                payload,
            } => (
                DATA_PACKET,
                group_id.as_str(),
                peer_id.as_str(),
                payload.as_slice(),
            ),
        };

        let group = group_id.as_bytes();
        let peer = peer_id.as_bytes();
        let mut out =
            Vec::with_capacity(RELAY_MAGIC.len() + 5 + group.len() + peer.len() + payload.len());
        out.extend_from_slice(RELAY_MAGIC);
        out.push(packet_type);
        out.push(group.len() as u8);
        out.push(peer.len() as u8);
        out.extend_from_slice(group);
        out.extend_from_slice(peer);
        out.extend_from_slice(payload);
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < RELAY_MAGIC.len() + 3 || &bytes[..RELAY_MAGIC.len()] != RELAY_MAGIC {
            return None;
        }
        let packet_type = bytes[4];
        let group_len = bytes[5] as usize;
        let peer_len = bytes[6] as usize;
        let header_len = RELAY_MAGIC.len() + 3;
        let group_start = header_len;
        let peer_start = group_start.checked_add(group_len)?;
        let payload_start = peer_start.checked_add(peer_len)?;
        if payload_start > bytes.len() {
            return None;
        }

        let group_id = std::str::from_utf8(&bytes[group_start..peer_start])
            .ok()?
            .to_string();
        let peer_id = std::str::from_utf8(&bytes[peer_start..payload_start])
            .ok()?
            .to_string();
        if group_id.is_empty() || peer_id.is_empty() {
            return None;
        }

        match packet_type {
            REGISTER_PACKET => {
                if payload_start != bytes.len() {
                    return None;
                }
                Some(Self::Register { group_id, peer_id })
            }
            DATA_PACKET => Some(Self::Data {
                group_id,
                peer_id,
                payload: bytes[payload_start..].to_vec(),
            }),
            _ => None,
        }
    }
}

async fn read_tcp_frame(reader: &mut OwnedReadHalf) -> io::Result<Vec<u8>> {
    let len = reader.read_u32().await? as usize;
    if len > MAX_TCP_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("relay frame too large: {len} bytes"),
        ));
    }
    let mut frame = vec![0u8; len];
    reader.read_exact(&mut frame).await?;
    Ok(frame)
}

async fn write_tcp_frame(writer: &mut OwnedWriteHalf, frame: &[u8]) -> io::Result<()> {
    if frame.len() > MAX_TCP_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("relay frame too large: {} bytes", frame.len()),
        ));
    }
    writer.write_u32(frame.len() as u32).await?;
    writer.write_all(frame).await?;
    writer.flush().await
}

fn validate_relay_id(field: &'static str, value: String) -> Result<String, RelayConfigError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(RelayConfigError::EmptyId(field));
    }
    let len = value.len();
    if len > MAX_ID_LEN {
        return Err(RelayConfigError::IdTooLong { field, len });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{MultiplexedPacket, UdpMultiplexer};
    use protocol::ControlMessage;
    use tokio::time::timeout;

    #[test]
    fn relay_packet_roundtrips() {
        let packet = RelayPacket::Data {
            group_id: "group".to_string(),
            peer_id: "peer-a".to_string(),
            payload: vec![1, 2, 3, 4],
        };

        let encoded = packet.encode();

        assert_eq!(RelayPacket::decode(&encoded), Some(packet));
        assert_eq!(RelayPacket::decode(b"not-relay"), None);
    }

    #[tokio::test]
    async fn relay_tunnels_forward_existing_udp_protocol_both_ways() {
        let (cancel_tx, _) = broadcast::channel(1);

        let server =
            BoundUdpRelayServer::bind(UdpRelayServerConfig::new("127.0.0.1:0".parse().unwrap()))
                .await
                .expect("server should bind");
        let relay_addr = server.local_addr().expect("server addr");
        let server_task = tokio::spawn(server.run(cancel_tx.subscribe()));

        let host_app = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("host app should bind");
        let host_app_addr = host_app.local_addr().expect("host app addr");
        let (host_sender, host_receiver) = host_app.split();

        let client_app = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("client app should bind");
        let (client_sender, client_receiver) = client_app.split();

        let host_tunnel = BoundUdpRelayTunnel::bind(
            UdpRelayTunnelConfig::new("127.0.0.1:0".parse().unwrap(), relay_addr, "group", "host")
                .unwrap()
                .with_local_target_addr(host_app_addr),
        )
        .await
        .expect("host tunnel should bind");
        let host_tunnel_addr = host_tunnel.local_addr().expect("host tunnel addr");
        let host_tunnel_task = tokio::spawn(host_tunnel.run(cancel_tx.subscribe()));

        let client_tunnel = BoundUdpRelayTunnel::bind(
            UdpRelayTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                relay_addr,
                "group",
                "client",
            )
            .unwrap(),
        )
        .await
        .expect("client tunnel should bind");
        let client_tunnel_addr = client_tunnel.local_addr().expect("client tunnel addr");
        let client_tunnel_task = tokio::spawn(client_tunnel.run(cancel_tx.subscribe()));

        tokio::time::sleep(Duration::from_millis(50)).await;
        client_sender
            .send_control(&ControlMessage::Heartbeat, client_tunnel_addr)
            .await
            .expect("client send should succeed");

        let received = timeout(Duration::from_secs(2), host_receiver.recv())
            .await
            .expect("host should receive through relay")
            .expect("host packet should decode");
        let MultiplexedPacket::Control(ControlMessage::Heartbeat, host_seen_addr) = received else {
            panic!("host received unexpected packet: {received:?}");
        };
        assert_eq!(host_seen_addr, host_tunnel_addr);

        host_sender
            .send_control(&ControlMessage::StopStream, host_seen_addr)
            .await
            .expect("host reply should send");

        let received = timeout(Duration::from_secs(2), client_receiver.recv())
            .await
            .expect("client should receive through relay")
            .expect("client packet should decode");
        let MultiplexedPacket::Control(ControlMessage::StopStream, _) = received else {
            panic!("client received unexpected packet: {received:?}");
        };

        let _ = cancel_tx.send(());
        server_task.await.expect("server task should join").unwrap();
        host_tunnel_task
            .await
            .expect("host tunnel task should join")
            .unwrap();
        client_tunnel_task
            .await
            .expect("client tunnel task should join")
            .unwrap();
    }

    #[tokio::test]
    async fn tcp_relay_tunnels_forward_existing_udp_protocol_both_ways() {
        let (cancel_tx, _) = broadcast::channel(1);

        let server =
            BoundTcpRelayServer::bind(TcpRelayServerConfig::new("127.0.0.1:0".parse().unwrap()))
                .await
                .expect("server should bind");
        let relay_addr = server.local_addr().expect("server addr");
        let server_task = tokio::spawn(server.run(cancel_tx.subscribe()));

        let host_app = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("host app should bind");
        let host_app_addr = host_app.local_addr().expect("host app addr");
        let (host_sender, host_receiver) = host_app.split();

        let client_app = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("client app should bind");
        let (client_sender, client_receiver) = client_app.split();

        let host_tunnel = BoundTcpRelayTunnel::bind(
            TcpRelayTunnelConfig::new("127.0.0.1:0".parse().unwrap(), relay_addr, "group", "host")
                .unwrap()
                .with_local_target_addr(host_app_addr),
        )
        .await
        .expect("host tunnel should bind");
        let host_tunnel_addr = host_tunnel.local_addr().expect("host tunnel addr");
        let host_tunnel_task = tokio::spawn(host_tunnel.run(cancel_tx.subscribe()));

        let client_tunnel = BoundTcpRelayTunnel::bind(
            TcpRelayTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                relay_addr,
                "group",
                "client",
            )
            .unwrap(),
        )
        .await
        .expect("client tunnel should bind");
        let client_tunnel_addr = client_tunnel.local_addr().expect("client tunnel addr");
        let client_tunnel_task = tokio::spawn(client_tunnel.run(cancel_tx.subscribe()));

        tokio::time::sleep(Duration::from_millis(50)).await;
        client_sender
            .send_control(&ControlMessage::Heartbeat, client_tunnel_addr)
            .await
            .expect("client send should succeed");

        let received = timeout(Duration::from_secs(2), host_receiver.recv())
            .await
            .expect("host should receive through relay")
            .expect("host packet should decode");
        let MultiplexedPacket::Control(ControlMessage::Heartbeat, host_seen_addr) = received else {
            panic!("host received unexpected packet: {received:?}");
        };
        assert_eq!(host_seen_addr, host_tunnel_addr);

        host_sender
            .send_control(&ControlMessage::StopStream, host_seen_addr)
            .await
            .expect("host reply should send");

        let received = timeout(Duration::from_secs(2), client_receiver.recv())
            .await
            .expect("client should receive through relay")
            .expect("client packet should decode");
        let MultiplexedPacket::Control(ControlMessage::StopStream, _) = received else {
            panic!("client received unexpected packet: {received:?}");
        };

        let _ = cancel_tx.send(());
        server_task.await.expect("server task should join").unwrap();
        host_tunnel_task
            .await
            .expect("host tunnel task should join")
            .unwrap();
        client_tunnel_task
            .await
            .expect("client tunnel task should join")
            .unwrap();
    }
}
