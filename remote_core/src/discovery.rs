use std::error::Error;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};

pub const DISCOVERY_MAGIC: &[u8; 8] = b"RPDISC1\0";
pub const DEFAULT_DISCOVERY_PORT: u16 = 38117;
pub const DEFAULT_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(2);
pub const DEFAULT_PEER_TTL: Duration = Duration::from_secs(8);
pub const MAX_DISCOVERY_PACKET_LEN: usize = 1200;
pub const DISCOVERY_RECV_BUFFER_LEN: usize = MAX_DISCOVERY_PACKET_LEN + 256;
pub const REMOTE_PLAY_DISCOVERY_ENV: &str = "REMOTE_PLAY_DISCOVERY";
pub const REMOTE_PLAY_DISCOVERY_PORT_ENV: &str = "REMOTE_PLAY_DISCOVERY_PORT";

const DISCOVERY_VERSION: u8 = 1;
const CAP_CAN_STREAM: u32 = 1 << 0;
const CAP_CAN_VIEW: u32 = 1 << 1;
const CAP_FILE_TRANSFER: u32 = 1 << 2;
const CAP_CLIPBOARD_SYNC: u32 = 1 << 3;
const CAP_TALKBACK: u32 = 1 << 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryScope {
    Lan,
    Mesh,
    Relay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiscoveryCapabilities {
    pub can_stream: bool,
    pub can_view: bool,
    pub file_transfer: bool,
    pub clipboard_sync: bool,
    pub talkback: bool,
}

impl DiscoveryCapabilities {
    pub fn all_interactive() -> Self {
        Self {
            can_stream: true,
            can_view: true,
            file_transfer: true,
            clipboard_sync: true,
            talkback: true,
        }
    }

    fn bits(self) -> u32 {
        let mut bits = 0u32;
        if self.can_stream {
            bits |= CAP_CAN_STREAM;
        }
        if self.can_view {
            bits |= CAP_CAN_VIEW;
        }
        if self.file_transfer {
            bits |= CAP_FILE_TRANSFER;
        }
        if self.clipboard_sync {
            bits |= CAP_CLIPBOARD_SYNC;
        }
        if self.talkback {
            bits |= CAP_TALKBACK;
        }
        bits
    }

    fn from_bits(bits: u32) -> Self {
        Self {
            can_stream: bits & CAP_CAN_STREAM != 0,
            can_view: bits & CAP_CAN_VIEW != 0,
            file_transfer: bits & CAP_FILE_TRANSFER != 0,
            clipboard_sync: bits & CAP_CLIPBOARD_SYNC != 0,
            talkback: bits & CAP_TALKBACK != 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryAnnouncement {
    pub network_name: String,
    pub device_id: String,
    pub display_name: String,
    pub control_port: u16,
    pub virtual_ip: Option<IpAddr>,
    pub capabilities: DiscoveryCapabilities,
    pub scope: DiscoveryScope,
    pub ttl: Duration,
}

impl DiscoveryAnnouncement {
    pub fn validate(&self) -> Result<(), DiscoveryError> {
        validate_text_field("network_name", &self.network_name, 128)?;
        validate_text_field("device_id", &self.device_id, 128)?;
        validate_text_field("display_name", &self.display_name, 128)?;
        if self.control_port == 0 && self.capabilities.can_stream {
            return Err(DiscoveryError::InvalidField("control_port"));
        }
        if self.ttl.is_zero() {
            return Err(DiscoveryError::InvalidField("ttl"));
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, DiscoveryError> {
        self.validate()?;

        let mut out = Vec::with_capacity(128);
        out.extend_from_slice(DISCOVERY_MAGIC);
        out.push(DISCOVERY_VERSION);
        out.push(match self.scope {
            DiscoveryScope::Lan => 0,
            DiscoveryScope::Mesh => 1,
            DiscoveryScope::Relay => 2,
        });
        out.extend_from_slice(&self.control_port.to_be_bytes());
        out.extend_from_slice(&self.capabilities.bits().to_be_bytes());
        out.extend_from_slice(&(self.ttl.as_millis().min(u32::MAX as u128) as u32).to_be_bytes());
        push_string(&mut out, &self.network_name)?;
        push_string(&mut out, &self.device_id)?;
        push_string(&mut out, &self.display_name)?;
        match self.virtual_ip {
            Some(IpAddr::V4(addr)) => {
                out.push(4);
                out.extend_from_slice(&addr.octets());
            }
            Some(IpAddr::V6(addr)) => {
                out.push(6);
                out.extend_from_slice(&addr.octets());
            }
            None => out.push(0),
        }

        if out.len() > MAX_DISCOVERY_PACKET_LEN {
            return Err(DiscoveryError::PacketTooLarge(out.len()));
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DiscoveryError> {
        if bytes.len() > MAX_DISCOVERY_PACKET_LEN {
            return Err(DiscoveryError::PacketTooLarge(bytes.len()));
        }

        let mut reader = DiscoveryReader::new(bytes);
        let magic = reader.read_exact(DISCOVERY_MAGIC.len())?;
        if magic != DISCOVERY_MAGIC {
            return Err(DiscoveryError::InvalidMagic);
        }
        let version = reader.read_u8()?;
        if version != DISCOVERY_VERSION {
            return Err(DiscoveryError::UnsupportedVersion(version));
        }
        let scope = match reader.read_u8()? {
            0 => DiscoveryScope::Lan,
            1 => DiscoveryScope::Mesh,
            2 => DiscoveryScope::Relay,
            _ => return Err(DiscoveryError::InvalidField("scope")),
        };
        let control_port = reader.read_u16()?;
        let capabilities = DiscoveryCapabilities::from_bits(reader.read_u32()?);
        let ttl = Duration::from_millis(u64::from(reader.read_u32()?));
        let network_name = reader.read_string()?;
        let device_id = reader.read_string()?;
        let display_name = reader.read_string()?;
        let virtual_ip = match reader.read_u8()? {
            0 => None,
            4 => Some(IpAddr::from(reader.read_array::<4>()?)),
            6 => Some(IpAddr::from(reader.read_array::<16>()?)),
            _ => return Err(DiscoveryError::InvalidField("virtual_ip")),
        };
        if !reader.is_finished() {
            return Err(DiscoveryError::TrailingBytes);
        }

        let announcement = Self {
            network_name,
            device_id,
            display_name,
            control_port,
            virtual_ip,
            capabilities,
            scope,
            ttl,
        };
        announcement.validate()?;
        Ok(announcement)
    }

    pub fn endpoint_from_source(&self, source: SocketAddr) -> SocketAddr {
        let ip = self.virtual_ip.unwrap_or_else(|| source.ip());
        SocketAddr::new(ip, self.control_port)
    }

    pub fn is_expired(&self, age: Duration) -> bool {
        age > self.ttl
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPeer {
    pub announcement: DiscoveryAnnouncement,
    pub source: SocketAddr,
    pub endpoint: SocketAddr,
    pub scope: DiscoveryScope,
    pub last_seen_ms: u64,
}

impl DiscoveredPeer {
    pub fn from_announcement(
        announcement: DiscoveryAnnouncement,
        source: SocketAddr,
        now_ms: u64,
    ) -> Self {
        let endpoint = announcement.endpoint_from_source(source);
        let scope = announcement.scope;
        Self {
            announcement,
            source,
            endpoint,
            scope,
            last_seen_ms: now_ms,
        }
    }

    pub fn from_announcement_with_route_override(
        announcement: DiscoveryAnnouncement,
        source: SocketAddr,
        now_ms: u64,
        route_override: DiscoveryRouteOverride,
    ) -> Self {
        Self {
            announcement,
            source,
            endpoint: route_override.endpoint,
            scope: route_override.scope,
            last_seen_ms: now_ms,
        }
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        let age = now_ms.saturating_sub(self.last_seen_ms);
        self.announcement.is_expired(Duration::from_millis(age))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryRouteOverride {
    pub source: SocketAddr,
    pub endpoint: SocketAddr,
    pub scope: DiscoveryScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryPeerSnapshot {
    peers: Vec<DiscoveredPeer>,
}

impl DiscoveryPeerSnapshot {
    pub fn peers(&self) -> &[DiscoveredPeer] {
        &self.peers
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryPeerCache {
    local_network_name: String,
    local_device_id: String,
    accept_any_network: bool,
    peers: Vec<DiscoveredPeer>,
}

impl DiscoveryPeerCache {
    pub fn new(local_network_name: impl Into<String>, local_device_id: impl Into<String>) -> Self {
        Self {
            local_network_name: local_network_name.into(),
            local_device_id: local_device_id.into(),
            accept_any_network: false,
            peers: Vec::new(),
        }
    }

    pub fn accept_any_network(mut self, accept: bool) -> Self {
        self.accept_any_network = accept;
        self
    }

    pub fn apply_announcement(
        &mut self,
        announcement: DiscoveryAnnouncement,
        source: SocketAddr,
        now_ms: u64,
    ) -> Option<DiscoveredPeer> {
        self.apply_announcement_with_route_override_option(announcement, source, now_ms, None)
    }

    pub fn apply_announcement_with_route_override(
        &mut self,
        announcement: DiscoveryAnnouncement,
        source: SocketAddr,
        now_ms: u64,
        route_override: DiscoveryRouteOverride,
    ) -> Option<DiscoveredPeer> {
        let route_override = (route_override.source == source).then_some(route_override);
        self.apply_announcement_with_route_override_option(
            announcement,
            source,
            now_ms,
            route_override,
        )
    }

    fn apply_announcement_with_route_override_option(
        &mut self,
        announcement: DiscoveryAnnouncement,
        source: SocketAddr,
        now_ms: u64,
        route_override: Option<DiscoveryRouteOverride>,
    ) -> Option<DiscoveredPeer> {
        if !self.accept_any_network && announcement.network_name != self.local_network_name {
            return None;
        }
        if announcement.device_id == self.local_device_id {
            return None;
        }

        let peer = if let Some(route_override) = route_override {
            DiscoveredPeer::from_announcement_with_route_override(
                announcement,
                source,
                now_ms,
                route_override,
            )
        } else {
            DiscoveredPeer::from_announcement(announcement, source, now_ms)
        };
        if let Some(existing) = self.peers.iter_mut().find(|existing| {
            existing.announcement.device_id == peer.announcement.device_id
                && existing.scope == peer.scope
        }) {
            *existing = peer.clone();
        } else {
            self.peers.push(peer.clone());
        }
        Some(peer)
    }

    pub fn prune_expired(&mut self, now_ms: u64) -> Vec<DiscoveredPeer> {
        let mut expired = Vec::new();
        let mut retained = Vec::with_capacity(self.peers.len());
        for peer in self.peers.drain(..) {
            if peer.is_expired(now_ms) {
                expired.push(peer);
            } else {
                retained.push(peer);
            }
        }
        self.peers = retained;
        expired
    }

    pub fn snapshot(&self) -> DiscoveryPeerSnapshot {
        DiscoveryPeerSnapshot {
            peers: self.peers.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveryRuntimeConfig {
    pub bind_addr: SocketAddr,
    pub announce_targets: Vec<SocketAddr>,
    pub route_overrides: Vec<DiscoveryRouteOverride>,
    pub announcement: DiscoveryAnnouncement,
    pub announce_interval: Duration,
    pub prune_interval: Duration,
    pub accept_any_network: bool,
}

impl DiscoveryRuntimeConfig {
    pub fn lan_default(announcement: DiscoveryAnnouncement) -> Self {
        Self::lan_on_port(announcement, DEFAULT_DISCOVERY_PORT)
    }

    pub fn lan_on_port(announcement: DiscoveryAnnouncement, port: u16) -> Self {
        Self {
            bind_addr: SocketAddr::from(([0, 0, 0, 0], port)),
            announce_targets: vec![SocketAddr::from(([255, 255, 255, 255], port))],
            route_overrides: Vec::new(),
            announcement,
            announce_interval: DEFAULT_ANNOUNCE_INTERVAL,
            prune_interval: Duration::from_secs(1),
            accept_any_network: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    PeerSeen(DiscoveredPeer),
    PeerExpired(DiscoveredPeer),
    Snapshot(DiscoveryPeerSnapshot),
    Error(String),
}

pub async fn run_discovery_runtime(
    config: DiscoveryRuntimeConfig,
    events_tx: tokio::sync::mpsc::UnboundedSender<DiscoveryEvent>,
    snapshot_tx: watch::Sender<DiscoveryPeerSnapshot>,
    mut cancel_rx: broadcast::Receiver<()>,
) -> Result<(), DiscoveryError> {
    config.announcement.validate()?;
    let packet = config.announcement.encode()?;
    let socket = Arc::new(bind_discovery_socket(config.bind_addr)?);
    if config
        .announce_targets
        .iter()
        .any(|target| matches!(target.ip(), IpAddr::V4(addr) if addr.is_broadcast()))
    {
        socket
            .set_broadcast(true)
            .map_err(|source| DiscoveryError::Io("set_broadcast", source.to_string()))?;
    }

    let cache = Arc::new(Mutex::new(
        DiscoveryPeerCache::new(
            config.announcement.network_name.clone(),
            config.announcement.device_id.clone(),
        )
        .accept_any_network(config.accept_any_network),
    ));
    let route_overrides = config.route_overrides.clone();

    let send_socket = socket.clone();
    let send_packet = packet.clone();
    let send_targets = config.announce_targets.clone();
    let sender_events_tx = events_tx.clone();
    let mut send_cancel_rx = cancel_rx.resubscribe();
    let sender_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(config.announce_interval);
        loop {
            tokio::select! {
                _ = send_cancel_rx.recv() => break,
                _ = interval.tick() => {
                    for target in &send_targets {
                        if let Err(err) = send_socket.send_to(&send_packet, target).await {
                            let _ = sender_events_tx.send(DiscoveryEvent::Error(format!(
                                "discovery announce to {target} failed: {err}"
                            )));
                        }
                    }
                }
            }
        }
    });

    let recv_socket = socket.clone();
    let recv_cache = cache.clone();
    let recv_snapshot_tx = snapshot_tx.clone();
    let receiver_events_tx = events_tx.clone();
    let mut recv_cancel_rx = cancel_rx.resubscribe();
    let receiver_task = tokio::spawn(async move {
        let mut buffer = [0u8; DISCOVERY_RECV_BUFFER_LEN];
        loop {
            tokio::select! {
                _ = recv_cancel_rx.recv() => break,
                result = recv_socket.recv_from(&mut buffer) => {
                    match result {
                        Ok((len, source)) => {
                            match DiscoveryAnnouncement::decode(&buffer[..len]) {
                                Ok(announcement) => {
                                    let now_ms = unix_now_ms();
                                    let route_override = route_overrides
                                        .iter()
                                        .copied()
                                        .find(|route_override| route_override.source == source);
                                    let (peer, snapshot) = {
                                        let mut cache = recv_cache.lock().expect("discovery cache lock");
                                        let peer = cache.apply_announcement_with_route_override_option(
                                            announcement,
                                            source,
                                            now_ms,
                                            route_override,
                                        );
                                        (peer, cache.snapshot())
                                    };
                                    if let Some(peer) = peer {
                                        let _ = recv_snapshot_tx.send(snapshot.clone());
                                        let _ = receiver_events_tx.send(DiscoveryEvent::PeerSeen(peer));
                                        let _ = receiver_events_tx.send(DiscoveryEvent::Snapshot(snapshot));
                                    }
                                }
                                Err(err) => {
                                    let _ = receiver_events_tx.send(DiscoveryEvent::Error(format!(
                                        "discovery packet from {source} ignored: {err}"
                                    )));
                                }
                            }
                        }
                        Err(err) => {
                            let _ = receiver_events_tx.send(DiscoveryEvent::Error(format!(
                                "discovery receive failed: {err}"
                            )));
                        }
                    }
                }
            }
        }
    });

    let prune_cache = cache.clone();
    let prune_snapshot_tx = snapshot_tx.clone();
    let pruner_events_tx = events_tx.clone();
    let mut prune_cancel_rx = cancel_rx.resubscribe();
    let pruner_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(config.prune_interval);
        loop {
            tokio::select! {
                _ = prune_cancel_rx.recv() => break,
                _ = interval.tick() => {
                    let now_ms = unix_now_ms();
                    let (expired, snapshot) = {
                        let mut cache = prune_cache.lock().expect("discovery cache lock");
                        let expired = cache.prune_expired(now_ms);
                        (expired, cache.snapshot())
                    };
                    if !expired.is_empty() {
                        let _ = prune_snapshot_tx.send(snapshot.clone());
                        for peer in expired {
                            let _ = pruner_events_tx.send(DiscoveryEvent::PeerExpired(peer));
                        }
                        let _ = pruner_events_tx.send(DiscoveryEvent::Snapshot(snapshot));
                    }
                }
            }
        }
    });

    let _ = snapshot_tx.send(cache.lock().expect("discovery cache lock").snapshot());
    let _ = cancel_rx.recv().await;
    let _ = sender_task.await;
    let _ = receiver_task.await;
    let _ = pruner_task.await;
    Ok(())
}

pub fn discovery_port_from_env() -> Result<u16, DiscoveryError> {
    parse_discovery_port(
        std::env::var(REMOTE_PLAY_DISCOVERY_PORT_ENV)
            .ok()
            .as_deref(),
    )
}

pub fn parse_discovery_port(value: Option<&str>) -> Result<u16, DiscoveryError> {
    let Some(value) = value else {
        return Ok(DEFAULT_DISCOVERY_PORT);
    };
    let value = value.trim();
    let port = value
        .parse::<u16>()
        .map_err(|_| DiscoveryError::InvalidPort(value.to_string()))?;
    if port == 0 {
        return Err(DiscoveryError::InvalidPort(value.to_string()));
    }
    Ok(port)
}

fn bind_discovery_socket(bind_addr: SocketAddr) -> Result<UdpSocket, DiscoveryError> {
    let socket = socket2::Socket::new(
        if bind_addr.is_ipv4() {
            socket2::Domain::IPV4
        } else {
            socket2::Domain::IPV6
        },
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .map_err(|source| DiscoveryError::Io("socket", source.to_string()))?;
    socket
        .set_reuse_address(true)
        .map_err(|source| DiscoveryError::Io("set_reuse_address", source.to_string()))?;
    #[cfg(unix)]
    socket
        .set_reuse_port(true)
        .map_err(|source| DiscoveryError::Io("set_reuse_port", source.to_string()))?;
    socket
        .set_nonblocking(true)
        .map_err(|source| DiscoveryError::Io("set_nonblocking", source.to_string()))?;
    socket
        .bind(&bind_addr.into())
        .map_err(|source| DiscoveryError::Io("bind", source.to_string()))?;
    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket)
        .map_err(|source| DiscoveryError::Io("from_std_socket", source.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryError {
    InvalidMagic,
    UnsupportedVersion(u8),
    InvalidField(&'static str),
    PacketTooLarge(usize),
    UnexpectedEof,
    InvalidUtf8,
    TrailingBytes,
    InvalidPort(String),
    Io(&'static str, String),
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => f.write_str("discovery packet has invalid magic"),
            Self::UnsupportedVersion(version) => {
                write!(f, "discovery packet version {version} is unsupported")
            }
            Self::InvalidField(field) => write!(f, "discovery packet field is invalid: {field}"),
            Self::PacketTooLarge(len) => write!(f, "discovery packet is too large: {len} bytes"),
            Self::UnexpectedEof => f.write_str("discovery packet ended unexpectedly"),
            Self::InvalidUtf8 => f.write_str("discovery packet contains invalid utf-8"),
            Self::TrailingBytes => f.write_str("discovery packet has trailing bytes"),
            Self::InvalidPort(value) => {
                write!(f, "discovery port must be a non-zero u16, got {value:?}")
            }
            Self::Io(context, source) => write!(f, "discovery {context} failed: {source}"),
        }
    }
}

impl Error for DiscoveryError {}

fn validate_text_field(
    field: &'static str,
    value: &str,
    max_len: usize,
) -> Result<(), DiscoveryError> {
    if value.trim().is_empty() {
        return Err(DiscoveryError::InvalidField(field));
    }
    if value.len() > max_len {
        return Err(DiscoveryError::InvalidField(field));
    }
    Ok(())
}

fn push_string(out: &mut Vec<u8>, value: &str) -> Result<(), DiscoveryError> {
    let len = value.len();
    if len > u8::MAX as usize {
        return Err(DiscoveryError::InvalidField("string"));
    }
    out.push(len as u8);
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

struct DiscoveryReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> DiscoveryReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, DiscoveryError> {
        let Some(value) = self.bytes.get(self.pos) else {
            return Err(DiscoveryError::UnexpectedEof);
        };
        self.pos += 1;
        Ok(*value)
    }

    fn read_u16(&mut self) -> Result<u16, DiscoveryError> {
        Ok(u16::from_be_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, DiscoveryError> {
        Ok(u32::from_be_bytes(self.read_array()?))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], DiscoveryError> {
        let bytes = self.read_exact(N)?;
        bytes.try_into().map_err(|_| DiscoveryError::UnexpectedEof)
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], DiscoveryError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(DiscoveryError::UnexpectedEof)?;
        if end > self.bytes.len() {
            return Err(DiscoveryError::UnexpectedEof);
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn read_string(&mut self) -> Result<String, DiscoveryError> {
        let len = self.read_u8()? as usize;
        let bytes = self.read_exact(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DiscoveryError::InvalidUtf8)
    }

    fn is_finished(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::DEFAULT_CONTROL_PORT;

    fn sample_announcement() -> DiscoveryAnnouncement {
        DiscoveryAnnouncement {
            network_name: "remote-play-test".to_string(),
            device_id: "device-123".to_string(),
            display_name: "Desk".to_string(),
            control_port: DEFAULT_CONTROL_PORT,
            virtual_ip: Some("10.7.0.12".parse().unwrap()),
            capabilities: DiscoveryCapabilities::all_interactive(),
            scope: DiscoveryScope::Lan,
            ttl: DEFAULT_PEER_TTL,
        }
    }

    #[test]
    fn cache_can_accept_foreign_networks_for_mobile_viewers() {
        let mut cache = DiscoveryPeerCache::new("local-net", "android-1").accept_any_network(true);
        let mut foreign = sample_announcement();
        foreign.network_name = "other-network".to_string();
        assert!(cache
            .apply_announcement(foreign, "192.168.1.20:38117".parse().unwrap(), 1)
            .is_some());
    }

    #[test]
    fn discovery_announcement_roundtrips() {
        let announcement = sample_announcement();
        let encoded = announcement.encode().expect("encode announcement");
        assert!(encoded.len() < MAX_DISCOVERY_PACKET_LEN);
        assert!(encoded.starts_with(DISCOVERY_MAGIC));

        let decoded = DiscoveryAnnouncement::decode(&encoded).expect("decode announcement");
        assert_eq!(decoded, announcement);
    }

    #[test]
    fn relay_scoped_discovery_announcement_roundtrips() {
        let announcement = DiscoveryAnnouncement {
            scope: DiscoveryScope::Relay,
            virtual_ip: None,
            ..sample_announcement()
        };

        let decoded = DiscoveryAnnouncement::decode(
            &announcement.encode().expect("encode relay announcement"),
        )
        .expect("decode relay announcement");

        assert_eq!(decoded.scope, DiscoveryScope::Relay);
        assert_eq!(decoded, announcement);
    }

    #[test]
    fn discovery_endpoint_prefers_virtual_ip_when_present() {
        let announcement = sample_announcement();
        let source = "192.168.1.20:38117".parse().unwrap();
        assert_eq!(
            announcement.endpoint_from_source(source),
            SocketAddr::from(([10, 7, 0, 12], DEFAULT_CONTROL_PORT))
        );

        let without_virtual = DiscoveryAnnouncement {
            virtual_ip: None,
            ..announcement
        };
        assert_eq!(
            without_virtual.endpoint_from_source(source),
            SocketAddr::from(([192, 168, 1, 20], DEFAULT_CONTROL_PORT))
        );
    }

    #[test]
    fn discovery_packet_rejects_wrong_network_or_bad_shape() {
        assert_eq!(
            DiscoveryAnnouncement::decode(b"not remote play").unwrap_err(),
            DiscoveryError::InvalidMagic
        );

        let mut encoded = sample_announcement().encode().expect("encode announcement");
        encoded.push(0);
        assert_eq!(
            DiscoveryAnnouncement::decode(&encoded).unwrap_err(),
            DiscoveryError::TrailingBytes
        );
    }

    #[test]
    fn discovery_validation_rejects_empty_identity_and_zero_port() {
        let err = DiscoveryAnnouncement {
            display_name: " ".to_string(),
            ..sample_announcement()
        }
        .encode()
        .unwrap_err();
        assert_eq!(err, DiscoveryError::InvalidField("display_name"));

        let err = DiscoveryAnnouncement {
            control_port: 0,
            ..sample_announcement()
        }
        .encode()
        .unwrap_err();
        assert_eq!(err, DiscoveryError::InvalidField("control_port"));

        DiscoveryAnnouncement {
            control_port: 0,
            capabilities: DiscoveryCapabilities {
                can_view: true,
                ..DiscoveryCapabilities::default()
            },
            ..sample_announcement()
        }
        .encode()
        .expect("viewer-only announcement does not need a stream control port");
    }

    #[test]
    fn discovery_port_parsing_uses_default_and_rejects_invalid_values() {
        assert_eq!(
            parse_discovery_port(None).expect("default port"),
            DEFAULT_DISCOVERY_PORT
        );
        assert_eq!(
            parse_discovery_port(Some("38118")).expect("custom port"),
            38118
        );
        assert_eq!(
            parse_discovery_port(Some("0")).unwrap_err(),
            DiscoveryError::InvalidPort("0".to_string())
        );
        assert_eq!(
            parse_discovery_port(Some("not-a-port")).unwrap_err(),
            DiscoveryError::InvalidPort("not-a-port".to_string())
        );
    }

    #[test]
    fn discovered_peer_expiry_uses_announcement_ttl() {
        let announcement = DiscoveryAnnouncement {
            ttl: Duration::from_millis(500),
            ..sample_announcement()
        };
        let peer = DiscoveredPeer::from_announcement(
            announcement,
            "192.168.1.20:38117".parse().unwrap(),
            1_000,
        );

        assert!(!peer.is_expired(1_500));
        assert!(peer.is_expired(1_501));
    }

    #[test]
    fn peer_cache_filters_other_networks_and_self_then_updates_existing_peer() {
        let mut cache = DiscoveryPeerCache::new("remote-play-test", "local");
        let source = "127.0.0.1:38117".parse().unwrap();

        assert!(
            cache
                .apply_announcement(
                    DiscoveryAnnouncement {
                        network_name: "other-network".to_string(),
                        device_id: "peer".to_string(),
                        ..sample_announcement()
                    },
                    source,
                    1_000,
                )
                .is_none()
        );
        assert!(
            cache
                .apply_announcement(
                    DiscoveryAnnouncement {
                        device_id: "local".to_string(),
                        ..sample_announcement()
                    },
                    source,
                    1_000,
                )
                .is_none()
        );

        let first = cache
            .apply_announcement(
                DiscoveryAnnouncement {
                    device_id: "peer".to_string(),
                    display_name: "Desk".to_string(),
                    ..sample_announcement()
                },
                source,
                1_000,
            )
            .expect("peer should be accepted");
        assert_eq!(first.announcement.display_name, "Desk");

        let updated = cache
            .apply_announcement(
                DiscoveryAnnouncement {
                    device_id: "peer".to_string(),
                    display_name: "Desk Renamed".to_string(),
                    ..sample_announcement()
                },
                source,
                2_000,
            )
            .expect("peer update should be accepted");
        assert_eq!(updated.announcement.display_name, "Desk Renamed");
        let snapshot = cache.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot.peers()[0].announcement.display_name,
            "Desk Renamed"
        );
    }

    #[test]
    fn peer_cache_keeps_direct_and_relay_routes_for_same_device() {
        let mut cache = DiscoveryPeerCache::new("remote-play-test", "local");
        let source = "192.168.1.20:38117".parse().unwrap();
        let relay_source = "127.0.0.1:48117".parse().unwrap();
        let relay_endpoint = "127.0.0.1:49171".parse().unwrap();

        cache
            .apply_announcement(
                DiscoveryAnnouncement {
                    device_id: "peer".to_string(),
                    display_name: "Desk".to_string(),
                    ..sample_announcement()
                },
                source,
                1_000,
            )
            .expect("direct peer should be accepted");
        cache
            .apply_announcement_with_route_override(
                DiscoveryAnnouncement {
                    device_id: "peer".to_string(),
                    display_name: "Desk".to_string(),
                    virtual_ip: None,
                    ..sample_announcement()
                },
                source,
                1_100,
                DiscoveryRouteOverride {
                    source: relay_source,
                    endpoint: relay_endpoint,
                    scope: DiscoveryScope::Relay,
                },
            )
            .expect("non-matching override should not apply");
        cache
            .apply_announcement_with_route_override(
                DiscoveryAnnouncement {
                    device_id: "peer".to_string(),
                    display_name: "Desk".to_string(),
                    virtual_ip: None,
                    ..sample_announcement()
                },
                relay_source,
                1_200,
                DiscoveryRouteOverride {
                    source: relay_source,
                    endpoint: relay_endpoint,
                    scope: DiscoveryScope::Relay,
                },
            )
            .expect("relay peer should be accepted");

        let snapshot = cache.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert!(
            snapshot
                .peers()
                .iter()
                .any(|peer| peer.scope == DiscoveryScope::Mesh || peer.scope == DiscoveryScope::Lan)
        );
        let relay = snapshot
            .peers()
            .iter()
            .find(|peer| peer.scope == DiscoveryScope::Relay)
            .expect("relay route should be present");
        assert_eq!(relay.endpoint, relay_endpoint);
        assert_eq!(relay.source, relay_source);
    }

    #[tokio::test]
    async fn discovery_runtime_finds_peer_over_loopback_unicast() {
        let local = DiscoveryAnnouncement {
            device_id: "local".to_string(),
            display_name: "Local".to_string(),
            control_port: DEFAULT_CONTROL_PORT,
            virtual_ip: None,
            ..sample_announcement()
        };
        let peer = DiscoveryAnnouncement {
            device_id: "peer".to_string(),
            display_name: "Peer".to_string(),
            control_port: 9000,
            virtual_ip: None,
            ..sample_announcement()
        };

        let local_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let local_addr = local_socket.local_addr().unwrap();
        let peer_addr = peer_socket.local_addr().unwrap();
        drop(local_socket);
        drop(peer_socket);

        let (local_events_tx, mut local_events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (peer_events_tx, mut peer_events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (local_snapshot_tx, _local_snapshot_rx) =
            watch::channel(DiscoveryPeerSnapshot::default());
        let (peer_snapshot_tx, _peer_snapshot_rx) =
            watch::channel(DiscoveryPeerSnapshot::default());
        let (cancel_tx, _) = broadcast::channel(1);

        let local_task = tokio::spawn(run_discovery_runtime(
            DiscoveryRuntimeConfig {
                bind_addr: local_addr,
                announce_targets: vec![peer_addr],
                route_overrides: Vec::new(),
                announcement: local,
                announce_interval: Duration::from_millis(50),
                prune_interval: Duration::from_millis(50),
                accept_any_network: false,
            },
            local_events_tx,
            local_snapshot_tx,
            cancel_tx.subscribe(),
        ));
        let peer_task = tokio::spawn(run_discovery_runtime(
            DiscoveryRuntimeConfig {
                bind_addr: peer_addr,
                announce_targets: vec![local_addr],
                route_overrides: Vec::new(),
                announcement: peer,
                announce_interval: Duration::from_millis(50),
                prune_interval: Duration::from_millis(50),
                accept_any_network: false,
            },
            peer_events_tx,
            peer_snapshot_tx,
            cancel_tx.subscribe(),
        ));

        let local_seen = wait_for_peer(&mut local_events_rx, "peer").await;
        let peer_seen = wait_for_peer(&mut peer_events_rx, "local").await;
        let _ = cancel_tx.send(());
        local_task.await.unwrap().unwrap();
        peer_task.await.unwrap().unwrap();

        assert_eq!(local_seen.endpoint, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(
            peer_seen.endpoint,
            SocketAddr::from(([127, 0, 0, 1], DEFAULT_CONTROL_PORT))
        );
    }

    #[tokio::test]
    async fn discovery_socket_allows_shared_port_for_dual_process_dev() {
        let first_addr = "127.0.0.1:0".parse().unwrap();
        let first = bind_discovery_socket(first_addr).expect("first bind should succeed");
        let shared_addr = first.local_addr().expect("first local addr");
        let second = bind_discovery_socket(shared_addr).expect("shared bind should succeed");

        assert_eq!(second.local_addr().expect("second local addr"), shared_addr);
    }

    async fn wait_for_peer(
        events_rx: &mut tokio::sync::mpsc::UnboundedReceiver<DiscoveryEvent>,
        device_id: &str,
    ) -> DiscoveredPeer {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(DiscoveryEvent::PeerSeen(peer)) = events_rx.recv().await
                    && peer.announcement.device_id == device_id
                {
                    break peer;
                }
            }
        })
        .await
        .expect("peer should be discovered")
    }
}
