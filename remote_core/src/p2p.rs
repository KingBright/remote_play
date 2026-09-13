use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

const P2P_MAGIC: &[u8; 8] = b"RPP2P001";
const P2P_VERSION: u8 = 1;
const PACKET_REGISTER: u8 = 1;
const PACKET_CANDIDATE: u8 = 2;
const PACKET_PUNCH: u8 = 3;
const PACKET_PUNCH_ACK: u8 = 4;
const MAX_ID_LEN: usize = 128;
const MAX_ANNOUNCEMENT_LEN: usize = 1200;
const MAX_PACKET_LEN: usize = 1536;
const ROUTE_QUEUE_CAPACITY: usize = 256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum P2pPacket {
    Register {
        group_id: String,
        peer_id: String,
        announcement: Vec<u8>,
    },
    Candidate {
        group_id: String,
        peer_id: String,
        endpoint: SocketAddr,
        announcement: Vec<u8>,
    },
    Punch {
        group_id: String,
        peer_id: String,
    },
    PunchAck {
        group_id: String,
        peer_id: String,
    },
}

impl P2pPacket {
    pub fn encode(&self) -> Result<Vec<u8>, P2pError> {
        let mut out = Vec::with_capacity(256);
        out.extend_from_slice(P2P_MAGIC);
        out.push(P2P_VERSION);
        match self {
            Self::Register {
                group_id,
                peer_id,
                announcement,
            } => {
                out.push(PACKET_REGISTER);
                push_id(&mut out, group_id)?;
                push_id(&mut out, peer_id)?;
                push_blob(&mut out, announcement)?;
            }
            Self::Candidate {
                group_id,
                peer_id,
                endpoint,
                announcement,
            } => {
                out.push(PACKET_CANDIDATE);
                push_id(&mut out, group_id)?;
                push_id(&mut out, peer_id)?;
                push_socket_addr(&mut out, *endpoint);
                push_blob(&mut out, announcement)?;
            }
            Self::Punch { group_id, peer_id } => {
                out.push(PACKET_PUNCH);
                push_id(&mut out, group_id)?;
                push_id(&mut out, peer_id)?;
            }
            Self::PunchAck { group_id, peer_id } => {
                out.push(PACKET_PUNCH_ACK);
                push_id(&mut out, group_id)?;
                push_id(&mut out, peer_id)?;
            }
        }
        if out.len() > MAX_PACKET_LEN {
            return Err(P2pError::PacketTooLarge(out.len()));
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, P2pError> {
        if bytes.len() > MAX_PACKET_LEN {
            return Err(P2pError::PacketTooLarge(bytes.len()));
        }
        if bytes.len() < P2P_MAGIC.len() + 2 || &bytes[..P2P_MAGIC.len()] != P2P_MAGIC {
            return Err(P2pError::InvalidPacket);
        }
        let mut reader = Reader::new(&bytes[P2P_MAGIC.len()..]);
        let version = reader.u8()?;
        if version != P2P_VERSION {
            return Err(P2pError::UnsupportedVersion(version));
        }
        let kind = reader.u8()?;
        let group_id = reader.id()?;
        let peer_id = reader.id()?;
        let packet = match kind {
            PACKET_REGISTER => Self::Register {
                group_id,
                peer_id,
                announcement: reader.blob()?,
            },
            PACKET_CANDIDATE => Self::Candidate {
                group_id,
                peer_id,
                endpoint: reader.socket_addr()?,
                announcement: reader.blob()?,
            },
            PACKET_PUNCH => Self::Punch { group_id, peer_id },
            PACKET_PUNCH_ACK => Self::PunchAck { group_id, peer_id },
            _ => return Err(P2pError::InvalidPacket),
        };
        if !reader.finished() {
            return Err(P2pError::InvalidPacket);
        }
        Ok(packet)
    }

    pub fn is_wire_packet(bytes: &[u8]) -> bool {
        bytes.len() >= P2P_MAGIC.len() && &bytes[..P2P_MAGIC.len()] == P2P_MAGIC
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum P2pError {
    InvalidPacket,
    UnsupportedVersion(u8),
    InvalidId,
    EmptySecret,
    IdTooLong(usize),
    AnnouncementTooLarge(usize),
    PacketTooLarge(usize),
}

impl std::fmt::Display for P2pError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPacket => f.write_str("invalid P2P packet"),
            Self::UnsupportedVersion(version) => write!(f, "unsupported P2P version {version}"),
            Self::InvalidId => f.write_str("invalid P2P group/peer id"),
            Self::EmptySecret => f.write_str("P2P group secret must not be empty"),
            Self::IdTooLong(len) => write!(f, "P2P id is too long: {len}"),
            Self::AnnouncementTooLarge(len) => write!(f, "P2P announcement is too large: {len}"),
            Self::PacketTooLarge(len) => write!(f, "P2P packet is too large: {len}"),
        }
    }
}

impl std::error::Error for P2pError {}

/// Derive an opaque rendezvous capability from the private device-group secret.
/// The secret itself never leaves the device.
pub fn derive_p2p_group_id(network_name: &str, network_secret: &str) -> Result<String, P2pError> {
    if network_name.trim().is_empty() {
        return Err(P2pError::InvalidId);
    }
    if network_secret.trim().is_empty() {
        return Err(P2pError::EmptySecret);
    }
    let mut mac =
        HmacSha256::new_from_slice(network_secret.as_bytes()).map_err(|_| P2pError::EmptySecret)?;
    mac.update(b"remote-play-p2p-rendezvous-v1\0");
    mac.update(network_name.as_bytes());
    let digest = mac.finalize().into_bytes();
    let mut id = String::from("rp-");
    for byte in digest.iter().take(16) {
        use std::fmt::Write as _;
        let _ = write!(&mut id, "{byte:02x}");
    }
    validate_id(id)
}

#[derive(Debug, Clone)]
pub struct P2pRendezvousServerConfig {
    pub bind_addr: SocketAddr,
    pub peer_ttl: Duration,
    pub max_peers_per_group: usize,
    pub max_total_peers: usize,
    pub log_events: bool,
}

impl P2pRendezvousServerConfig {
    pub fn new(bind_addr: SocketAddr) -> Self {
        Self {
            bind_addr,
            peer_ttl: Duration::from_secs(15),
            max_peers_per_group: 16,
            max_total_peers: 4096,
            log_events: false,
        }
    }

    pub fn with_event_logging(mut self, enabled: bool) -> Self {
        self.log_events = enabled;
        self
    }

    pub fn with_limits(mut self, max_peers_per_group: usize, max_total_peers: usize) -> Self {
        self.max_peers_per_group = max_peers_per_group.max(1);
        self.max_total_peers = max_total_peers.max(self.max_peers_per_group);
        self
    }
}

pub struct BoundP2pRendezvousServer {
    socket: UdpSocket,
    config: P2pRendezvousServerConfig,
}

impl BoundP2pRendezvousServer {
    pub async fn bind(config: P2pRendezvousServerConfig) -> io::Result<Self> {
        let socket = UdpSocket::bind(config.bind_addr).await?;
        Ok(Self { socket, config })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub async fn run(self, mut cancel_rx: broadcast::Receiver<()>) -> io::Result<()> {
        let mut peers: HashMap<(String, String), RendezvousPeer> = HashMap::new();
        let mut buf = [0u8; MAX_PACKET_LEN];
        let mut prune = tokio::time::interval(Duration::from_secs(1));
        prune.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = cancel_rx.recv() => return Ok(()),
                _ = prune.tick() => {
                    let ttl = self.config.peer_ttl;
                    peers.retain(|_, peer| peer.last_seen.elapsed() <= ttl);
                }
                received = self.socket.recv_from(&mut buf) => {
                    let (len, addr) = received?;
                    let Ok(P2pPacket::Register { group_id, peer_id, announcement }) = P2pPacket::decode(&buf[..len]) else {
                        continue;
                    };
                    let same_group_count = peers.keys().filter(|(group, _)| group == &group_id).count();
                    let key = (group_id.clone(), peer_id.clone());
                    if !peers.contains_key(&key)
                        && (same_group_count >= self.config.max_peers_per_group
                            || peers.len() >= self.config.max_total_peers)
                    {
                        continue;
                    }

                    let existing = peers
                        .iter()
                        .filter(|((group, other_peer_id), _)| group == &group_id && other_peer_id != &peer_id)
                        .map(|((_, other_peer_id), peer)| (other_peer_id.clone(), peer.clone()))
                        .collect::<Vec<_>>();

                    peers.insert(key, RendezvousPeer {
                        addr,
                        announcement: announcement.clone(),
                        last_seen: Instant::now(),
                    });

                    if self.config.log_events {
                        println!("P2P rendezvous register group={group_id} peer={peer_id} addr={addr} peers={}", existing.len());
                    }

                    for (other_peer_id, other) in existing {
                        let to_new = P2pPacket::Candidate {
                            group_id: group_id.clone(),
                            peer_id: other_peer_id,
                            endpoint: other.addr,
                            announcement: other.announcement,
                        };
                        if let Ok(bytes) = to_new.encode() {
                            let _ = self.socket.send_to(&bytes, addr).await;
                        }

                        let to_existing = P2pPacket::Candidate {
                            group_id: group_id.clone(),
                            peer_id: peer_id.clone(),
                            endpoint: addr,
                            announcement: announcement.clone(),
                        };
                        if let Ok(bytes) = to_existing.encode() {
                            let _ = self.socket.send_to(&bytes, other.addr).await;
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
struct RendezvousPeer {
    addr: SocketAddr,
    announcement: Vec<u8>,
    last_seen: Instant,
}

#[derive(Debug, Clone)]
pub struct P2pTunnelConfig {
    pub bind_addr: SocketAddr,
    pub rendezvous_addr: SocketAddr,
    pub group_id: String,
    pub peer_id: String,
    pub announcement: Vec<u8>,
    pub local_target_addr: Option<SocketAddr>,
    pub discovery_target_addr: Option<SocketAddr>,
    pub register_interval: Duration,
    pub punch_interval: Duration,
    pub peer_ttl: Duration,
    pub log_events: bool,
}

impl P2pTunnelConfig {
    pub fn new(
        bind_addr: SocketAddr,
        rendezvous_addr: SocketAddr,
        group_id: impl Into<String>,
        peer_id: impl Into<String>,
        announcement: Vec<u8>,
    ) -> Result<Self, P2pError> {
        let group_id = validate_id(group_id.into())?;
        let peer_id = validate_id(peer_id.into())?;
        if announcement.len() > MAX_ANNOUNCEMENT_LEN {
            return Err(P2pError::AnnouncementTooLarge(announcement.len()));
        }
        Ok(Self {
            bind_addr,
            rendezvous_addr,
            group_id,
            peer_id,
            announcement,
            local_target_addr: None,
            discovery_target_addr: None,
            register_interval: Duration::from_secs(2),
            punch_interval: Duration::from_millis(750),
            peer_ttl: Duration::from_secs(20),
            log_events: false,
        })
    }

    pub fn with_local_target_addr(mut self, addr: SocketAddr) -> Self {
        self.local_target_addr = Some(addr);
        self
    }

    pub fn with_discovery_target_addr(mut self, addr: SocketAddr) -> Self {
        self.discovery_target_addr = Some(addr);
        self
    }

    pub fn with_event_logging(mut self, enabled: bool) -> Self {
        self.log_events = enabled;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P2pPeerSnapshot {
    pub peer_id: String,
    pub candidate: SocketAddr,
    pub local_endpoint: SocketAddr,
    pub direct_ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct P2pTunnelSnapshot {
    pub peers: Vec<P2pPeerSnapshot>,
}

impl P2pTunnelSnapshot {
    pub fn route_for_peer(&self, peer_id: &str) -> Option<SocketAddr> {
        self.peers
            .iter()
            .find(|peer| peer.peer_id == peer_id)
            .map(|peer| peer.local_endpoint)
    }

    pub fn direct_peer_count(&self) -> usize {
        self.peers.iter().filter(|peer| peer.direct_ready).count()
    }
}

pub struct BoundP2pTunnel {
    socket: UdpSocket,
    config: P2pTunnelConfig,
}

impl BoundP2pTunnel {
    pub async fn bind(config: P2pTunnelConfig) -> io::Result<Self> {
        let socket = UdpSocket::bind(config.bind_addr).await?;
        Ok(Self { socket, config })
    }

    /// Public/NAT-facing UDP socket. All peers share this socket so the observed
    /// rendezvous candidate is the same socket that carries RemotePlay traffic.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub async fn run(
        self,
        mut cancel_rx: broadcast::Receiver<()>,
        snapshot_tx: watch::Sender<P2pTunnelSnapshot>,
    ) -> io::Result<()> {
        let public_socket = Arc::new(self.socket);
        let register = P2pPacket::Register {
            group_id: self.config.group_id.clone(),
            peer_id: self.config.peer_id.clone(),
            announcement: self.config.announcement.clone(),
        }
        .encode()
        .map_err(p2p_io_error)?;
        let mut register_interval = tokio::time::interval(self.config.register_interval);
        register_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut punch_interval = tokio::time::interval(self.config.punch_interval);
        punch_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut prune_interval = tokio::time::interval(Duration::from_secs(1));
        prune_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let (route_cancel_tx, _) = broadcast::channel(1);
        let mut peers: HashMap<String, PeerRoute> = HashMap::new();
        let mut buf = vec![0u8; 65_535];
        let _ = public_socket
            .send_to(&register, self.config.rendezvous_addr)
            .await;

        loop {
            tokio::select! {
                _ = cancel_rx.recv() => {
                    let _ = route_cancel_tx.send(());
                    for (_, route) in peers.drain() {
                        route.task.abort();
                    }
                    return Ok(());
                }
                _ = register_interval.tick() => {
                    let _ = public_socket.send_to(&register, self.config.rendezvous_addr).await;
                }
                _ = punch_interval.tick() => {
                    let punch = P2pPacket::Punch {
                        group_id: self.config.group_id.clone(),
                        peer_id: self.config.peer_id.clone(),
                    }.encode().map_err(p2p_io_error)?;
                    for route in peers.values() {
                        let _ = public_socket.send_to(&punch, route.candidate).await;
                    }
                }
                _ = prune_interval.tick() => {
                    let ttl = self.config.peer_ttl;
                    let stale = peers
                        .iter()
                        .filter(|(_, route)| route.last_seen.elapsed() > ttl)
                        .map(|(peer_id, _)| peer_id.clone())
                        .collect::<Vec<_>>();
                    if !stale.is_empty() {
                        for peer_id in stale {
                            if let Some(route) = peers.remove(&peer_id) {
                                route.task.abort();
                            }
                        }
                        publish_snapshot(&snapshot_tx, &peers);
                    }
                }
                received = public_socket.recv_from(&mut buf) => {
                    let (len, addr) = received?;
                    let bytes = &buf[..len];

                    if addr == self.config.rendezvous_addr && P2pPacket::is_wire_packet(bytes) {
                        if let Ok(P2pPacket::Candidate { group_id, peer_id, endpoint, announcement }) = P2pPacket::decode(bytes)
                            && group_id == self.config.group_id
                            && peer_id != self.config.peer_id
                        {
                            ensure_peer_route(
                                &mut peers,
                                &peer_id,
                                endpoint,
                                &announcement,
                                public_socket.clone(),
                                &route_cancel_tx,
                                &self.config,
                            ).await?;
                            if let Some(route) = peers.get_mut(&peer_id) {
                                route.last_seen = Instant::now();
                                route.candidate = endpoint;
                                let _ = route.candidate_tx.send(endpoint);
                            }
                            let punch = P2pPacket::Punch {
                                group_id: self.config.group_id.clone(),
                                peer_id: self.config.peer_id.clone(),
                            }.encode().map_err(p2p_io_error)?;
                            let _ = public_socket.send_to(&punch, endpoint).await;
                            publish_snapshot(&snapshot_tx, &peers);
                            if self.config.log_events {
                                println!("P2P candidate peer={peer_id} endpoint={endpoint}");
                            }
                        }
                        continue;
                    }

                    if P2pPacket::is_wire_packet(bytes) {
                        match P2pPacket::decode(bytes) {
                            Ok(P2pPacket::Punch { group_id, peer_id })
                                if group_id == self.config.group_id && peer_id != self.config.peer_id =>
                            {
                                ensure_peer_route(
                                    &mut peers,
                                    &peer_id,
                                    addr,
                                    &[],
                                    public_socket.clone(),
                                    &route_cancel_tx,
                                    &self.config,
                                ).await?;
                                if let Some(route) = peers.get_mut(&peer_id) {
                                    route.candidate = addr;
                                    route.direct_ready = true;
                                    route.last_seen = Instant::now();
                                    let _ = route.candidate_tx.send(addr);
                                    inject_discovery_announcement(
                                        route.local_endpoint,
                                        &route.announcement,
                                        self.config.discovery_target_addr,
                                    )
                                    .await?;
                                }
                                let ack = P2pPacket::PunchAck {
                                    group_id: self.config.group_id.clone(),
                                    peer_id: self.config.peer_id.clone(),
                                }.encode().map_err(p2p_io_error)?;
                                let _ = public_socket.send_to(&ack, addr).await;
                                publish_snapshot(&snapshot_tx, &peers);
                                continue;
                            }
                            Ok(P2pPacket::PunchAck { group_id, peer_id })
                                if group_id == self.config.group_id && peer_id != self.config.peer_id =>
                            {
                                ensure_peer_route(
                                    &mut peers,
                                    &peer_id,
                                    addr,
                                    &[],
                                    public_socket.clone(),
                                    &route_cancel_tx,
                                    &self.config,
                                ).await?;
                                if let Some(route) = peers.get_mut(&peer_id) {
                                    route.candidate = addr;
                                    route.direct_ready = true;
                                    route.last_seen = Instant::now();
                                    let _ = route.candidate_tx.send(addr);
                                    inject_discovery_announcement(
                                        route.local_endpoint,
                                        &route.announcement,
                                        self.config.discovery_target_addr,
                                    )
                                    .await?;
                                }
                                publish_snapshot(&snapshot_tx, &peers);
                                continue;
                            }
                            _ => {}
                        }
                    }

                    let peer_id = peers
                        .iter()
                        .find_map(|(peer_id, route)| (route.candidate == addr).then(|| peer_id.clone()));
                    if let Some(peer_id) = peer_id
                        && let Some(route) = peers.get_mut(&peer_id)
                    {
                        let was_ready = route.direct_ready;
                        route.direct_ready = true;
                        route.last_seen = Instant::now();
                        if !was_ready {
                            inject_discovery_announcement(
                                route.local_endpoint,
                                &route.announcement,
                                self.config.discovery_target_addr,
                            )
                            .await?;
                        }
                        let _ = route.inbound_tx.try_send(bytes.to_vec());
                        publish_snapshot(&snapshot_tx, &peers);
                    }
                }
            }
        }
    }
}

struct PeerRoute {
    candidate: SocketAddr,
    local_endpoint: SocketAddr,
    announcement: Vec<u8>,
    candidate_tx: watch::Sender<SocketAddr>,
    inbound_tx: mpsc::Sender<Vec<u8>>,
    direct_ready: bool,
    last_seen: Instant,
    task: JoinHandle<()>,
}

async fn ensure_peer_route(
    peers: &mut HashMap<String, PeerRoute>,
    peer_id: &str,
    candidate: SocketAddr,
    announcement: &[u8],
    public_socket: Arc<UdpSocket>,
    route_cancel_tx: &broadcast::Sender<()>,
    config: &P2pTunnelConfig,
) -> io::Result<()> {
    if let Some(route) = peers.get_mut(peer_id) {
        route.candidate = candidate;
        route.last_seen = Instant::now();
        let _ = route.candidate_tx.send(candidate);
        if !announcement.is_empty() {
            route.announcement.clear();
            route.announcement.extend_from_slice(announcement);
            if route.direct_ready {
                inject_discovery_announcement(
                    route.local_endpoint,
                    &route.announcement,
                    config.discovery_target_addr,
                )
                .await?;
            }
        }
        return Ok(());
    }

    let route_socket = UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let local_endpoint = route_socket.local_addr()?;
    let (candidate_tx, candidate_rx) = watch::channel(candidate);
    let (inbound_tx, inbound_rx) = mpsc::channel(ROUTE_QUEUE_CAPACITY);
    let cancel_rx = route_cancel_tx.subscribe();
    let local_target_addr = config.local_target_addr;
    let task_public_socket = public_socket;
    let task = tokio::spawn(async move {
        if let Err(err) = run_peer_route(
            route_socket,
            task_public_socket,
            candidate_rx,
            inbound_rx,
            local_target_addr,
            cancel_rx,
        )
        .await
        {
            eprintln!("P2P peer route stopped: {err}");
        }
    });

    peers.insert(
        peer_id.to_string(),
        PeerRoute {
            candidate,
            local_endpoint,
            announcement: announcement.to_vec(),
            candidate_tx,
            inbound_tx,
            direct_ready: false,
            last_seen: Instant::now(),
            task,
        },
    );

    Ok(())
}

async fn run_peer_route(
    route_socket: UdpSocket,
    public_socket: Arc<UdpSocket>,
    candidate_rx: watch::Receiver<SocketAddr>,
    mut inbound_rx: mpsc::Receiver<Vec<u8>>,
    default_local_target: Option<SocketAddr>,
    mut cancel_rx: broadcast::Receiver<()>,
) -> io::Result<()> {
    let mut return_target = default_local_target;
    let mut buf = vec![0u8; 65_535];
    loop {
        tokio::select! {
            _ = cancel_rx.recv() => return Ok(()),
            inbound = inbound_rx.recv() => {
                let Some(bytes) = inbound else { return Ok(()); };
                if let Some(target) = return_target {
                    route_socket.send_to(&bytes, target).await?;
                }
            }
            received = route_socket.recv_from(&mut buf) => {
                let (len, source) = received?;
                return_target = Some(source);
                let candidate = *candidate_rx.borrow();
                public_socket.send_to(&buf[..len], candidate).await?;
            }
        }
    }
}

async fn inject_discovery_announcement(
    local_endpoint: SocketAddr,
    announcement: &[u8],
    discovery_target: Option<SocketAddr>,
) -> io::Result<()> {
    let Some(target) = discovery_target else {
        return Ok(());
    };
    let Ok(mut decoded) = crate::discovery::DiscoveryAnnouncement::decode(announcement) else {
        return Ok(());
    };
    decoded.scope = crate::discovery::DiscoveryScope::P2p;
    decoded.virtual_ip = None;
    decoded.control_port = local_endpoint.port();
    let encoded = decoded.encode().map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("P2P discovery encode: {err}"),
        )
    })?;
    let socket = UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    socket.send_to(&encoded, target).await?;
    Ok(())
}

fn publish_snapshot(
    snapshot_tx: &watch::Sender<P2pTunnelSnapshot>,
    peers: &HashMap<String, PeerRoute>,
) {
    let mut snapshot = P2pTunnelSnapshot {
        peers: peers
            .iter()
            .map(|(peer_id, route)| P2pPeerSnapshot {
                peer_id: peer_id.clone(),
                candidate: route.candidate,
                local_endpoint: route.local_endpoint,
                direct_ready: route.direct_ready,
            })
            .collect(),
    };
    snapshot.peers.sort_by(|a, b| a.peer_id.cmp(&b.peer_id));
    let _ = snapshot_tx.send(snapshot);
}

fn validate_id(value: String) -> Result<String, P2pError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.'))
    {
        return Err(P2pError::InvalidId);
    }
    if value.len() > MAX_ID_LEN {
        return Err(P2pError::IdTooLong(value.len()));
    }
    Ok(value)
}

fn push_id(out: &mut Vec<u8>, value: &str) -> Result<(), P2pError> {
    let value = validate_id(value.to_string())?;
    out.push(value.len() as u8);
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn push_blob(out: &mut Vec<u8>, value: &[u8]) -> Result<(), P2pError> {
    if value.len() > MAX_ANNOUNCEMENT_LEN {
        return Err(P2pError::AnnouncementTooLarge(value.len()));
    }
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn push_socket_addr(out: &mut Vec<u8>, addr: SocketAddr) {
    match addr.ip() {
        IpAddr::V4(ip) => {
            out.push(4);
            out.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(6);
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&addr.port().to_be_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn u8(&mut self) -> Result<u8, P2pError> {
        let Some(value) = self.bytes.get(self.pos).copied() else {
            return Err(P2pError::InvalidPacket);
        };
        self.pos += 1;
        Ok(value)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], P2pError> {
        let end = self.pos.checked_add(len).ok_or(P2pError::InvalidPacket)?;
        let Some(value) = self.bytes.get(self.pos..end) else {
            return Err(P2pError::InvalidPacket);
        };
        self.pos = end;
        Ok(value)
    }

    fn id(&mut self) -> Result<String, P2pError> {
        let len = self.u8()? as usize;
        if len == 0 || len > MAX_ID_LEN {
            return Err(P2pError::InvalidId);
        }
        let value = std::str::from_utf8(self.take(len)?).map_err(|_| P2pError::InvalidId)?;
        validate_id(value.to_string())
    }

    fn blob(&mut self) -> Result<Vec<u8>, P2pError> {
        let len = u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as usize;
        if len > MAX_ANNOUNCEMENT_LEN {
            return Err(P2pError::AnnouncementTooLarge(len));
        }
        Ok(self.take(len)?.to_vec())
    }

    fn socket_addr(&mut self) -> Result<SocketAddr, P2pError> {
        let family = self.u8()?;
        let ip = match family {
            4 => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(self.take(4)?).unwrap())),
            6 => IpAddr::V6(Ipv6Addr::from(
                <[u8; 16]>::try_from(self.take(16)?).unwrap(),
            )),
            _ => return Err(P2pError::InvalidPacket),
        };
        let port = u16::from_be_bytes(self.take(2)?.try_into().unwrap());
        Ok(SocketAddr::new(ip, port))
    }

    fn finished(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

fn p2p_io_error(err: P2pError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{
        DEFAULT_PEER_TTL, DiscoveryAnnouncement, DiscoveryCapabilities, DiscoveryScope,
    };

    fn announcement(peer: &str, port: u16) -> Vec<u8> {
        DiscoveryAnnouncement {
            network_name: "network".into(),
            device_id: peer.into(),
            display_name: peer.into(),
            control_port: port,
            virtual_ip: None,
            capabilities: DiscoveryCapabilities::all_interactive(),
            scope: DiscoveryScope::Lan,
            ttl: DEFAULT_PEER_TTL,
        }
        .encode()
        .unwrap()
    }

    #[test]
    fn p2p_group_id_is_secret_scoped_and_redacted() {
        let a = derive_p2p_group_id("network", "secret-a").unwrap();
        let repeated = derive_p2p_group_id("network", "secret-a").unwrap();
        let b = derive_p2p_group_id("network", "secret-b").unwrap();
        assert_eq!(a, repeated);
        assert_ne!(a, b);
        assert!(!a.contains("secret"));
        assert!(derive_p2p_group_id("network", "").is_err());
    }

    #[test]
    fn p2p_packets_roundtrip() {
        for packet in [
            P2pPacket::Register {
                group_id: "group-a".into(),
                peer_id: "peer-a".into(),
                announcement: vec![1, 2, 3],
            },
            P2pPacket::Candidate {
                group_id: "group-a".into(),
                peer_id: "peer-b".into(),
                endpoint: "203.0.113.8:40000".parse().unwrap(),
                announcement: vec![4, 5],
            },
            P2pPacket::Punch {
                group_id: "group-a".into(),
                peer_id: "peer-a".into(),
            },
            P2pPacket::PunchAck {
                group_id: "group-a".into(),
                peer_id: "peer-b".into(),
            },
        ] {
            assert_eq!(
                P2pPacket::decode(&packet.encode().unwrap()).unwrap(),
                packet
            );
        }
    }

    #[tokio::test]
    async fn rendezvous_exchanges_observed_udp_candidates() {
        let server = BoundP2pRendezvousServer::bind(P2pRendezvousServerConfig::new(
            "127.0.0.1:0".parse().unwrap(),
        ))
        .await
        .unwrap();
        let server_addr = server.local_addr().unwrap();
        let (cancel_tx, _) = broadcast::channel(1);
        let server_task = tokio::spawn(server.run(cancel_tx.subscribe()));

        let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        for (socket, peer) in [(&a, "a"), (&b, "b")] {
            let reg = P2pPacket::Register {
                group_id: "g".into(),
                peer_id: peer.into(),
                announcement: vec![],
            }
            .encode()
            .unwrap();
            socket.send_to(&reg, server_addr).await.unwrap();
        }

        let mut buf = [0u8; 1536];
        let (len_a, _) = tokio::time::timeout(Duration::from_secs(1), a.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(P2pPacket::decode(&buf[..len_a]).unwrap(), P2pPacket::Candidate { peer_id, endpoint, .. } if peer_id == "b" && endpoint == b.local_addr().unwrap())
        );
        let (len_b, _) = tokio::time::timeout(Duration::from_secs(1), b.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(P2pPacket::decode(&buf[..len_b]).unwrap(), P2pPacket::Candidate { peer_id, endpoint, .. } if peer_id == "a" && endpoint == a.local_addr().unwrap())
        );

        let _ = cancel_tx.send(());
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn p2p_tunnels_forward_raw_remoteplay_datagrams_both_ways() {
        let server = BoundP2pRendezvousServer::bind(P2pRendezvousServerConfig::new(
            "127.0.0.1:0".parse().unwrap(),
        ))
        .await
        .unwrap();
        let server_addr = server.local_addr().unwrap();
        let (server_cancel_tx, _) = broadcast::channel(1);
        let server_task = tokio::spawn(server.run(server_cancel_tx.subscribe()));

        let host_app = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let host_target = host_app.local_addr().unwrap();
        let host_tunnel = BoundP2pTunnel::bind(
            P2pTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                server_addr,
                "group",
                "host",
                announcement("host", host_target.port()),
            )
            .unwrap()
            .with_local_target_addr(host_target),
        )
        .await
        .unwrap();
        let viewer_tunnel = BoundP2pTunnel::bind(
            P2pTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                server_addr,
                "group",
                "viewer",
                announcement("viewer", 1),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let (host_cancel_tx, _) = broadcast::channel(1);
        let (viewer_cancel_tx, _) = broadcast::channel(1);
        let (host_snapshot_tx, mut host_snapshot_rx) = watch::channel(P2pTunnelSnapshot::default());
        let (viewer_snapshot_tx, mut viewer_snapshot_rx) =
            watch::channel(P2pTunnelSnapshot::default());
        let host_task = tokio::spawn(host_tunnel.run(host_cancel_tx.subscribe(), host_snapshot_tx));
        let viewer_task =
            tokio::spawn(viewer_tunnel.run(viewer_cancel_tx.subscribe(), viewer_snapshot_tx));

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if host_snapshot_rx.borrow().direct_peer_count() == 1
                    && viewer_snapshot_rx.borrow().direct_peer_count() == 1
                {
                    break;
                }
                tokio::select! {
                    _ = host_snapshot_rx.changed() => {},
                    _ = viewer_snapshot_rx.changed() => {},
                }
            }
        })
        .await
        .expect("P2P punch should become ready");

        let host_route = host_snapshot_rx.borrow().route_for_peer("viewer").unwrap();
        let viewer_route = viewer_snapshot_rx.borrow().route_for_peer("host").unwrap();
        let viewer_app = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        viewer_app
            .send_to(b"hello-host", viewer_route)
            .await
            .unwrap();
        let mut buf = [0u8; 64];
        let (len, _) = tokio::time::timeout(Duration::from_secs(1), host_app.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..len], b"hello-host");

        host_app.send_to(b"hello-viewer", host_route).await.unwrap();
        let (len, _) = tokio::time::timeout(Duration::from_secs(1), viewer_app.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..len], b"hello-viewer");

        let _ = host_cancel_tx.send(());
        let _ = viewer_cancel_tx.send(());
        let _ = server_cancel_tx.send(());
        host_task.await.unwrap().unwrap();
        viewer_task.await.unwrap().unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn p2p_discovery_route_is_published_only_after_direct_ready() {
        let server = BoundP2pRendezvousServer::bind(P2pRendezvousServerConfig::new(
            "127.0.0.1:0".parse().unwrap(),
        ))
        .await
        .unwrap();
        let server_addr = server.local_addr().unwrap();
        let (server_cancel_tx, _) = broadcast::channel(1);
        let server_task = tokio::spawn(server.run(server_cancel_tx.subscribe()));

        let discovery_sink = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let a = BoundP2pTunnel::bind(
            P2pTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                server_addr,
                "group",
                "a",
                announcement("a", 39271),
            )
            .unwrap()
            .with_discovery_target_addr(discovery_sink.local_addr().unwrap()),
        )
        .await
        .unwrap();
        let b = BoundP2pTunnel::bind(
            P2pTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                server_addr,
                "group",
                "b",
                announcement("b", 39271),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let (a_cancel_tx, _) = broadcast::channel(1);
        let (b_cancel_tx, _) = broadcast::channel(1);
        let (a_snapshot_tx, mut a_snapshot_rx) = watch::channel(P2pTunnelSnapshot::default());
        let (b_snapshot_tx, _b_snapshot_rx) = watch::channel(P2pTunnelSnapshot::default());
        let a_task = tokio::spawn(a.run(a_cancel_tx.subscribe(), a_snapshot_tx));
        let b_task = tokio::spawn(b.run(b_cancel_tx.subscribe(), b_snapshot_tx));

        let mut buf = [0u8; 1536];
        let (len, _) =
            tokio::time::timeout(Duration::from_secs(2), discovery_sink.recv_from(&mut buf))
                .await
                .expect("direct-ready P2P route should publish discovery")
                .unwrap();
        let routed = DiscoveryAnnouncement::decode(&buf[..len]).unwrap();
        assert_eq!(routed.device_id, "b");
        assert_eq!(routed.scope, DiscoveryScope::P2p);
        assert!(routed.virtual_ip.is_none());

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if a_snapshot_rx.borrow().direct_peer_count() == 1 {
                    break;
                }
                a_snapshot_rx.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let route = a_snapshot_rx.borrow().route_for_peer("b").unwrap();
        assert_eq!(routed.control_port, route.port());

        let _ = a_cancel_tx.send(());
        let _ = b_cancel_tx.send(());
        let _ = server_cancel_tx.send(());
        a_task.await.unwrap().unwrap();
        b_task.await.unwrap().unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn one_tunnel_keeps_independent_routes_for_three_remote_peers() {
        let server = BoundP2pRendezvousServer::bind(P2pRendezvousServerConfig::new(
            "127.0.0.1:0".parse().unwrap(),
        ))
        .await
        .unwrap();
        let server_addr = server.local_addr().unwrap();
        let (server_cancel_tx, _) = broadcast::channel(1);
        let server_task = tokio::spawn(server.run(server_cancel_tx.subscribe()));

        let (local_cancel_tx, _) = broadcast::channel(1);
        let local = BoundP2pTunnel::bind(
            P2pTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                server_addr,
                "group",
                "local",
                announcement("local", 1),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let (local_snapshot_tx, mut local_snapshot_rx) =
            watch::channel(P2pTunnelSnapshot::default());
        let local_task = tokio::spawn(local.run(local_cancel_tx.subscribe(), local_snapshot_tx));

        let mut remote_cancels = Vec::new();
        let mut remote_tasks = Vec::new();
        for peer in ["a", "b", "c"] {
            let tunnel = BoundP2pTunnel::bind(
                P2pTunnelConfig::new(
                    "127.0.0.1:0".parse().unwrap(),
                    server_addr,
                    "group",
                    peer,
                    announcement(peer, 1),
                )
                .unwrap(),
            )
            .await
            .unwrap();
            let (cancel_tx, _) = broadcast::channel(1);
            let (snapshot_tx, _snapshot_rx) = watch::channel(P2pTunnelSnapshot::default());
            remote_tasks.push(tokio::spawn(tunnel.run(cancel_tx.subscribe(), snapshot_tx)));
            remote_cancels.push(cancel_tx);
        }

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if local_snapshot_rx.borrow().direct_peer_count() == 3 {
                    break;
                }
                local_snapshot_rx.changed().await.unwrap();
            }
        })
        .await
        .expect("all three peers should become direct");

        let snapshot = local_snapshot_rx.borrow().clone();
        assert_eq!(snapshot.peers.len(), 3);
        let routes = ["a", "b", "c"]
            .into_iter()
            .map(|peer| snapshot.route_for_peer(peer).unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(routes.len(), 3, "every peer needs a distinct local route");

        let _ = local_cancel_tx.send(());
        for cancel in remote_cancels {
            let _ = cancel.send(());
        }
        local_task.await.unwrap().unwrap();
        for task in remote_tasks {
            task.await.unwrap().unwrap();
        }
        let _ = server_cancel_tx.send(());
        server_task.await.unwrap().unwrap();
    }
}
