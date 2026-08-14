use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use sha2::Sha256;
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
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{WebSocketStream, accept_async_with_config, connect_async_with_config};

const RELAY_MAGIC: &[u8; 4] = b"RPR1";
const REGISTER_PACKET: u8 = 1;
const DATA_PACKET: u8 = 2;
const MAX_ID_LEN: usize = 255;
const MAX_PACKET_LEN: usize = 65_535;
const MAX_TCP_FRAME_LEN: usize = RELAY_MAGIC.len() + 3 + (2 * MAX_ID_LEN) + MAX_PACKET_LEN;
const DEFAULT_MAX_TCP_CONNECTIONS: usize = 128;
const DEFAULT_MAX_PEERS_PER_GROUP: usize = 8;
const DEFAULT_TCP_WRITER_QUEUE_CAPACITY: usize = 16;
const DEFAULT_TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const WEBSOCKET_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_WEBSOCKET_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const DEFAULT_WEBSOCKET_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(25);

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
    pub max_connections: usize,
    pub max_peers_per_group: usize,
    pub writer_queue_capacity: usize,
    pub idle_timeout: Duration,
}

impl TcpRelayServerConfig {
    pub fn new(bind_addr: SocketAddr) -> Self {
        Self {
            bind_addr,
            log_events: false,
            max_connections: DEFAULT_MAX_TCP_CONNECTIONS,
            max_peers_per_group: DEFAULT_MAX_PEERS_PER_GROUP,
            writer_queue_capacity: DEFAULT_TCP_WRITER_QUEUE_CAPACITY,
            idle_timeout: DEFAULT_TCP_IDLE_TIMEOUT,
        }
    }

    pub fn with_event_logging(mut self, log_events: bool) -> Self {
        self.log_events = log_events;
        self
    }

    pub fn with_limits(
        mut self,
        max_connections: usize,
        max_peers_per_group: usize,
        writer_queue_capacity: usize,
    ) -> Self {
        self.max_connections = max_connections.max(1);
        self.max_peers_per_group = max_peers_per_group.max(1);
        self.writer_queue_capacity = writer_queue_capacity.max(1);
        self
    }

    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout.max(Duration::from_secs(1));
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSocketRelayTunnelConfig {
    pub bind_addr: SocketAddr,
    pub relay_url: String,
    pub group_id: String,
    pub peer_id: String,
    pub local_target_addr: Option<SocketAddr>,
    pub register_interval: Duration,
    pub heartbeat_interval: Duration,
    pub heartbeat_timeout: Duration,
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

impl WebSocketRelayTunnelConfig {
    pub fn new(
        bind_addr: SocketAddr,
        relay_url: impl Into<String>,
        group_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> Result<Self, RelayConfigError> {
        let relay_url = validate_websocket_relay_url(relay_url.into())?;
        let group_id = validate_relay_id("group_id", group_id.into())?;
        let peer_id = validate_relay_id("peer_id", peer_id.into())?;
        Ok(Self {
            bind_addr,
            relay_url,
            group_id,
            peer_id,
            local_target_addr: None,
            register_interval: Duration::from_secs(1),
            heartbeat_interval: DEFAULT_WEBSOCKET_HEARTBEAT_INTERVAL,
            heartbeat_timeout: DEFAULT_WEBSOCKET_HEARTBEAT_TIMEOUT,
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

    pub fn with_heartbeat(
        mut self,
        heartbeat_interval: Duration,
        heartbeat_timeout: Duration,
    ) -> Self {
        self.heartbeat_interval = heartbeat_interval.max(Duration::from_millis(10));
        self.heartbeat_timeout = heartbeat_timeout.max(self.heartbeat_interval);
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
    InvalidWebSocketUrl(String),
}

impl std::fmt::Display for RelayConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyId(field) => write!(f, "{field} is empty"),
            Self::IdTooLong { field, len } => {
                write!(f, "{field} is too long: {len} bytes, max {MAX_ID_LEN}")
            }
            Self::InvalidWebSocketUrl(value) => {
                write!(f, "invalid relay WebSocket URL {value:?}")
            }
        }
    }
}

impl std::error::Error for RelayConfigError {}

pub fn derive_relay_group_id(
    network_name: &str,
    network_secret: &str,
    channel: &str,
) -> Result<String, RelayConfigError> {
    let network_name = validate_relay_id("network_name", network_name.to_string())?;
    let channel = validate_relay_id("channel", channel.to_string())?;
    let network_secret = network_secret.trim();
    if network_secret.is_empty() {
        return Err(RelayConfigError::EmptyId("network_secret"));
    }

    let mut mac = Hmac::<Sha256>::new_from_slice(network_secret.as_bytes())
        .expect("HMAC accepts keys of any size");
    mac.update(b"remote-play-relay-group-v1\0");
    mac.update(network_name.as_bytes());
    mac.update(b"\0");
    mac.update(channel.as_bytes());
    let bytes = mac.finalize().into_bytes();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(encoded)
}

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
        let connection_slots = Arc::new(Semaphore::new(self.config.max_connections));

        loop {
            tokio::select! {
                _ = cancel_rx.recv() => return Ok(()),
                accepted = self.listener.accept() => {
                    let (stream, addr) = accepted?;
                    let Ok(connection_slot) = connection_slots.clone().try_acquire_owned() else {
                        if self.config.log_events {
                            eprintln!("TCP relay rejected connection from {addr}: connection limit reached");
                        }
                        drop(stream);
                        continue;
                    };
                    let state = state.clone();
                    let config = self.config.clone();
                    tokio::spawn(async move {
                        let _connection_slot = connection_slot;
                        if let Err(err) = run_tcp_relay_connection(stream, addr, state, &config).await {
                            eprintln!("TCP relay connection ended: {err}");
                        }
                    });
                }
            }
        }
    }

    pub async fn run_websocket(self, mut cancel_rx: broadcast::Receiver<()>) -> io::Result<()> {
        let state = Arc::new(Mutex::new(TcpRelayServerState::default()));
        let connection_slots = Arc::new(Semaphore::new(self.config.max_connections));

        loop {
            tokio::select! {
                _ = cancel_rx.recv() => return Ok(()),
                accepted = self.listener.accept() => {
                    let (stream, addr) = accepted?;
                    let Ok(connection_slot) = connection_slots.clone().try_acquire_owned() else {
                        if self.config.log_events {
                            eprintln!("WebSocket relay rejected connection from {addr}: connection limit reached");
                        }
                        drop(stream);
                        continue;
                    };
                    let state = state.clone();
                    let config = self.config.clone();
                    tokio::spawn(async move {
                        let _connection_slot = connection_slot;
                        let handshake = tokio::time::timeout(
                            WEBSOCKET_HANDSHAKE_TIMEOUT,
                            accept_async_with_config(stream, Some(relay_websocket_config())),
                        )
                        .await;
                        let relay_stream = match handshake {
                            Ok(Ok(stream)) => stream,
                            Ok(Err(err)) => {
                                if config.log_events {
                                    eprintln!("WebSocket relay handshake from {addr} failed: {err}");
                                }
                                return;
                            }
                            Err(_) => {
                                if config.log_events {
                                    eprintln!("WebSocket relay handshake from {addr} timed out");
                                }
                                return;
                            }
                        };
                        if let Err(err) = run_websocket_relay_connection(
                            relay_stream,
                            addr,
                            state,
                            &config,
                        )
                        .await
                            && config.log_events
                        {
                            eprintln!("WebSocket relay connection ended: {err}");
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

pub struct BoundWebSocketRelayTunnel {
    socket: UdpSocket,
    config: WebSocketRelayTunnelConfig,
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

impl BoundWebSocketRelayTunnel {
    pub async fn bind(config: WebSocketRelayTunnelConfig) -> io::Result<Self> {
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
                connected = connect_async_with_config(
                    self.config.relay_url.as_str(),
                    Some(relay_websocket_config()),
                    true,
                ) => {
                    match connected {
                        Ok((stream, _response)) => {
                            if self.config.log_events {
                                println!("WebSocket relay tunnel connected");
                            }
                            match self.run_connected(stream, &mut cancel_rx).await {
                                Ok(()) => return Ok(()),
                                Err(err) => {
                                    if self.config.log_events {
                                        eprintln!("WebSocket relay tunnel disconnected: {err}");
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            if self.config.log_events {
                                eprintln!("WebSocket relay tunnel connect failed: {err}");
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

    async fn run_connected<S>(
        &self,
        mut stream: WebSocketStream<S>,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> io::Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let mut buf = vec![0u8; MAX_PACKET_LEN];
        let mut local_target_addr = self.config.local_target_addr;
        let register_packet = RelayPacket::Register {
            group_id: self.config.group_id.clone(),
            peer_id: self.config.peer_id.clone(),
        }
        .encode();
        send_websocket_binary(&mut stream, register_packet.clone()).await?;
        let mut register_interval = tokio::time::interval(self.config.register_interval);
        let mut heartbeat_interval = tokio::time::interval(self.config.heartbeat_interval);
        heartbeat_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_server_message = Instant::now();

        loop {
            tokio::select! {
                _ = cancel_rx.recv() => return Ok(()),
                _ = register_interval.tick() => {
                    send_websocket_binary(&mut stream, register_packet.clone()).await?;
                }
                _ = heartbeat_interval.tick() => {
                    if last_server_message.elapsed() > self.config.heartbeat_timeout {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "WebSocket relay heartbeat timed out",
                        ));
                    }
                    stream
                        .send(Message::Ping(Bytes::new()))
                        .await
                        .map_err(websocket_io_error)?;
                }
                message = stream.next() => {
                    let Some(message) = message else {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "WebSocket relay closed"));
                    };
                    let message = message.map_err(websocket_io_error)?;
                    last_server_message = Instant::now();
                    match message {
                        Message::Binary(frame) => {
                            if let Some(RelayPacket::Data { payload, .. }) = RelayPacket::decode(&frame)
                                && let Some(target) = local_target_addr
                            {
                                let _ = self.socket.send_to(&payload, target).await;
                            }
                        }
                        Message::Close(_) => {
                            return Err(io::Error::new(io::ErrorKind::ConnectionReset, "WebSocket relay closed"));
                        }
                        Message::Text(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                    }
                }
                received = self.socket.recv_from(&mut buf) => {
                    let (len, addr) = received?;
                    local_target_addr = Some(addr);
                    let encoded = RelayPacket::Data {
                        group_id: self.config.group_id.clone(),
                        peer_id: self.config.peer_id.clone(),
                        payload: buf[..len].to_vec(),
                    }
                    .encode();
                    send_websocket_binary(&mut stream, encoded).await?;
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
    connection_identities: HashMap<u64, RelayPeerKey>,
}

#[derive(Clone)]
struct TcpRelayPeerState {
    connection_id: u64,
    writer_tx: mpsc::Sender<Arc<[u8]>>,
    disconnect_tx: watch::Sender<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayRegistrationError {
    ConnectionIdentityChanged,
    GroupPeerLimitReached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RelayForwardResult {
    forwarded: usize,
    disconnected_slow_peers: usize,
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
        writer_tx: mpsc::Sender<Arc<[u8]>>,
        disconnect_tx: watch::Sender<bool>,
        max_peers_per_group: usize,
    ) -> Result<(), RelayRegistrationError> {
        let key = RelayPeerKey { group_id, peer_id };
        if let Some(existing) = self.connection_identities.get(&connection_id) {
            return if existing == &key {
                Ok(())
            } else {
                Err(RelayRegistrationError::ConnectionIdentityChanged)
            };
        }

        let existing_peer_connection = self.peers.get(&key).map(|peer| peer.connection_id);
        let group_peer_count = self
            .peers
            .keys()
            .filter(|peer| peer.group_id == key.group_id)
            .count();
        if existing_peer_connection.is_none() && group_peer_count >= max_peers_per_group {
            return Err(RelayRegistrationError::GroupPeerLimitReached);
        }

        if let Some(existing) = self.peers.remove(&key) {
            let _ = existing.disconnect_tx.send(true);
            self.connection_identities.remove(&existing.connection_id);
        }
        self.connection_identities
            .insert(connection_id, key.clone());
        self.peers.insert(
            key,
            TcpRelayPeerState {
                connection_id,
                writer_tx,
                disconnect_tx,
            },
        );
        Ok(())
    }

    fn identity(&self, connection_id: u64) -> Option<&RelayPeerKey> {
        self.connection_identities.get(&connection_id)
    }

    fn forward_frame(
        &mut self,
        group_id: &str,
        source_connection_id: u64,
        frame: Arc<[u8]>,
    ) -> RelayForwardResult {
        let targets = self
            .peers
            .iter()
            .filter(|(key, peer)| {
                key.group_id == group_id && peer.connection_id != source_connection_id
            })
            .map(|(_, peer)| {
                (
                    peer.connection_id,
                    peer.writer_tx.clone(),
                    peer.disconnect_tx.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut result = RelayForwardResult {
            forwarded: 0,
            disconnected_slow_peers: 0,
        };
        let mut failed_connections = Vec::new();
        for (connection_id, writer_tx, disconnect_tx) in targets {
            match writer_tx.try_send(frame.clone()) {
                Ok(()) => result.forwarded += 1,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    result.disconnected_slow_peers += 1;
                    let _ = disconnect_tx.send(true);
                    failed_connections.push(connection_id);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    failed_connections.push(connection_id);
                }
            }
        }
        for connection_id in failed_connections {
            self.remove_connection(connection_id);
        }
        result
    }

    fn remove_connection(&mut self, connection_id: u64) {
        self.connection_identities.remove(&connection_id);
        self.peers
            .retain(|_, peer| peer.connection_id != connection_id);
    }
}

async fn run_tcp_relay_connection(
    stream: TcpStream,
    addr: SocketAddr,
    state: Arc<Mutex<TcpRelayServerState>>,
    config: &TcpRelayServerConfig,
) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let (mut reader, writer) = stream.into_split();
    let (writer_tx, writer_rx) = mpsc::channel(config.writer_queue_capacity);
    let (disconnect_tx, mut disconnect_rx) = watch::channel(false);
    let connection_id = {
        let mut state = state.lock().await;
        state.next_connection_id()
    };
    let writer_task = tokio::spawn(run_tcp_relay_writer(writer, writer_rx));

    let result = async {
        loop {
            let frame = tokio::select! {
                _ = disconnect_rx.changed() => {
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "relay peer is too slow"));
                }
                frame = tokio::time::timeout(config.idle_timeout, read_tcp_frame(&mut reader)) => {
                    frame.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "relay connection idle timeout"))??
                }
            };
            let Some(packet) = RelayPacket::decode(&frame) else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid relay packet"));
            };
            match packet {
                RelayPacket::Register { group_id, peer_id } => {
                    if config.log_events {
                        println!("TCP relay peer registered addr={addr}");
                    }
                    state
                        .lock()
                        .await
                        .register(
                            group_id,
                            peer_id,
                            connection_id,
                            writer_tx.clone(),
                            disconnect_tx.clone(),
                            config.max_peers_per_group,
                        )
                        .map_err(|err| {
                            io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                format!("relay registration rejected: {err:?}"),
                            )
                        })?;
                }
                RelayPacket::Data {
                    group_id,
                    peer_id,
                    payload,
                } => {
                    let mut state = state.lock().await;
                    let Some(identity) = state.identity(connection_id) else {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "relay data received before registration",
                        ));
                    };
                    if identity.group_id != group_id || identity.peer_id != peer_id {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "relay data identity does not match registration",
                        ));
                    }
                    let encoded = Arc::<[u8]>::from(
                        RelayPacket::Data {
                            group_id: group_id.clone(),
                            peer_id,
                            payload,
                        }
                        .encode(),
                    );
                    let forward = state.forward_frame(&group_id, connection_id, encoded);
                    drop(state);
                    if config.log_events {
                        println!(
                            "TCP relay data forwarded targets={} slow_peers={}",
                            forward.forwarded,
                            forward.disconnected_slow_peers,
                        );
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
    mut writer_rx: mpsc::Receiver<Arc<[u8]>>,
) -> io::Result<()> {
    while let Some(frame) = writer_rx.recv().await {
        write_tcp_frame(&mut writer, &frame).await?;
    }
    Ok(())
}

async fn run_websocket_relay_connection<S>(
    stream: WebSocketStream<S>,
    addr: SocketAddr,
    state: Arc<Mutex<TcpRelayServerState>>,
    config: &TcpRelayServerConfig,
) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut writer, mut reader) = stream.split();
    let (writer_tx, mut writer_rx) = mpsc::channel::<Arc<[u8]>>(config.writer_queue_capacity);
    let (disconnect_tx, mut disconnect_rx) = watch::channel(false);
    let connection_id = state.lock().await.next_connection_id();

    let result = async {
        loop {
            tokio::select! {
                _ = disconnect_rx.changed() => {
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "relay peer is too slow"));
                }
                outbound = writer_rx.recv() => {
                    let Some(frame) = outbound else {
                        return Ok(());
                    };
                    writer
                        .send(Message::Binary(Bytes::from_owner(frame)))
                        .await
                        .map_err(websocket_io_error)?;
                }
                inbound = tokio::time::timeout(config.idle_timeout, reader.next()) => {
                    let inbound = inbound
                        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "relay connection idle timeout"))?;
                    let Some(message) = inbound else {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "WebSocket relay closed"));
                    };
                    match message.map_err(websocket_io_error)? {
                        Message::Binary(frame) => {
                            handle_tcp_relay_packet(
                                &frame,
                                addr,
                                connection_id,
                                &writer_tx,
                                &disconnect_tx,
                                &state,
                                config,
                            )
                            .await?;
                        }
                        Message::Close(_) => return Ok(()),
                        Message::Text(_) => {
                            return Err(io::Error::new(io::ErrorKind::InvalidData, "relay accepts binary WebSocket messages only"));
                        }
                        Message::Ping(payload) => {
                            writer.send(Message::Pong(payload)).await.map_err(websocket_io_error)?;
                        }
                        Message::Pong(_) | Message::Frame(_) => {}
                    }
                }
            }
        }
    }
    .await;

    state.lock().await.remove_connection(connection_id);
    result
}

async fn handle_tcp_relay_packet(
    frame: &[u8],
    addr: SocketAddr,
    connection_id: u64,
    writer_tx: &mpsc::Sender<Arc<[u8]>>,
    disconnect_tx: &watch::Sender<bool>,
    state: &Arc<Mutex<TcpRelayServerState>>,
    config: &TcpRelayServerConfig,
) -> io::Result<()> {
    let Some(packet) = RelayPacket::decode(frame) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid relay packet",
        ));
    };
    match packet {
        RelayPacket::Register { group_id, peer_id } => {
            if config.log_events {
                println!("WebSocket relay peer registered addr={addr}");
            }
            state
                .lock()
                .await
                .register(
                    group_id,
                    peer_id,
                    connection_id,
                    writer_tx.clone(),
                    disconnect_tx.clone(),
                    config.max_peers_per_group,
                )
                .map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("relay registration rejected: {err:?}"),
                    )
                })
        }
        RelayPacket::Data {
            group_id,
            peer_id,
            payload,
        } => {
            let mut state = state.lock().await;
            let Some(identity) = state.identity(connection_id) else {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "relay data received before registration",
                ));
            };
            if identity.group_id != group_id || identity.peer_id != peer_id {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "relay data identity does not match registration",
                ));
            }
            let encoded = Arc::<[u8]>::from(
                RelayPacket::Data {
                    group_id: group_id.clone(),
                    peer_id,
                    payload,
                }
                .encode(),
            );
            let forward = state.forward_frame(&group_id, connection_id, encoded);
            drop(state);
            if config.log_events {
                println!(
                    "WebSocket relay data forwarded targets={} slow_peers={}",
                    forward.forwarded, forward.disconnected_slow_peers,
                );
            }
            Ok(())
        }
    }
}

async fn send_websocket_binary<S>(stream: &mut WebSocketStream<S>, frame: Vec<u8>) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    stream
        .send(Message::Binary(Bytes::from(frame)))
        .await
        .map_err(websocket_io_error)
}

fn relay_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(64 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * MAX_TCP_FRAME_LEN)
        .max_message_size(Some(MAX_TCP_FRAME_LEN))
        .max_frame_size(Some(MAX_TCP_FRAME_LEN))
}

fn websocket_io_error(err: tokio_tungstenite::tungstenite::Error) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, err)
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
        if bytes.len() < RELAY_MAGIC.len() + 3
            || bytes.len() > MAX_TCP_FRAME_LEN
            || &bytes[..RELAY_MAGIC.len()] != RELAY_MAGIC
        {
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

fn validate_websocket_relay_url(value: String) -> Result<String, RelayConfigError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(RelayConfigError::EmptyId("relay_url"));
    }
    let scheme_valid = value.starts_with("ws://") || value.starts_with("wss://");
    let authority = value
        .split_once("://")
        .map(|(_, remainder)| remainder.split('/').next().unwrap_or_default())
        .unwrap_or_default();
    if !scheme_valid
        || authority.is_empty()
        || authority.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err(RelayConfigError::InvalidWebSocketUrl(value));
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

    #[test]
    fn relay_group_capability_is_secret_and_channel_scoped() {
        let control = derive_relay_group_id("network-a", "secret-a", "control").unwrap();
        let repeated = derive_relay_group_id("network-a", "secret-a", "control").unwrap();
        let discovery = derive_relay_group_id("network-a", "secret-a", "discovery").unwrap();
        let other_secret = derive_relay_group_id("network-a", "secret-b", "control").unwrap();

        assert_eq!(control, repeated);
        assert_eq!(control.len(), 64);
        assert!(control.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(!control.contains("network-a"));
        assert!(!control.contains("secret-a"));
        assert_ne!(control, discovery);
        assert_ne!(control, other_secret);
    }

    #[test]
    fn websocket_relay_url_requires_ws_or_wss() {
        assert!(
            WebSocketRelayTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                "wss://relay.example.test:8443/relay",
                "group",
                "peer",
            )
            .is_ok()
        );
        assert!(matches!(
            WebSocketRelayTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                "https://relay.example.test/relay",
                "group",
                "peer",
            ),
            Err(RelayConfigError::InvalidWebSocketUrl(_))
        ));
    }

    #[test]
    fn websocket_relay_heartbeat_timeout_never_precedes_the_probe_interval() {
        let config = WebSocketRelayTunnelConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            "wss://relay.example.test/relay",
            "group",
            "peer",
        )
        .unwrap()
        .with_heartbeat(Duration::from_secs(5), Duration::from_secs(2));

        assert_eq!(config.heartbeat_interval, Duration::from_secs(5));
        assert_eq!(config.heartbeat_timeout, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn websocket_relay_detects_a_silent_half_open_connection() {
        let config = WebSocketRelayTunnelConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            "ws://relay.example.test/relay",
            "group",
            "peer",
        )
        .unwrap()
        .with_heartbeat(Duration::from_millis(20), Duration::from_millis(60));
        let tunnel = BoundWebSocketRelayTunnel::bind(config)
            .await
            .expect("tunnel should bind");
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let client_stream = WebSocketStream::from_raw_socket(
            client_io,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        let _silent_server = WebSocketStream::from_raw_socket(
            server_io,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;
        let (_cancel_tx, cancel_rx) = broadcast::channel(1);
        let mut cancel_rx = cancel_rx;

        let err = timeout(
            Duration::from_secs(1),
            tunnel.run_connected(client_stream, &mut cancel_rx),
        )
        .await
        .expect("heartbeat timeout should be bounded")
        .expect_err("silent connection should be rejected");

        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert_eq!(err.to_string(), "WebSocket relay heartbeat timed out");
    }

    #[test]
    fn tcp_connection_cannot_change_identity_after_registration() {
        let mut state = TcpRelayServerState::default();
        let connection_id = state.next_connection_id();
        let (writer_tx, _writer_rx) = mpsc::channel(4);
        let (disconnect_tx, _disconnect_rx) = tokio::sync::watch::channel(false);

        state
            .register(
                "group-a".to_string(),
                "peer-a".to_string(),
                connection_id,
                writer_tx.clone(),
                disconnect_tx.clone(),
                4,
            )
            .unwrap();
        state
            .register(
                "group-a".to_string(),
                "peer-a".to_string(),
                connection_id,
                writer_tx.clone(),
                disconnect_tx.clone(),
                4,
            )
            .unwrap();

        let err = state
            .register(
                "group-b".to_string(),
                "peer-a".to_string(),
                connection_id,
                writer_tx,
                disconnect_tx,
                4,
            )
            .unwrap_err();
        assert_eq!(err, RelayRegistrationError::ConnectionIdentityChanged);
    }

    #[test]
    fn tcp_group_peer_limit_is_enforced() {
        let mut state = TcpRelayServerState::default();
        let (writer_a, _reader_a) = mpsc::channel(4);
        let (disconnect_a, _disconnect_reader_a) = tokio::sync::watch::channel(false);
        let first_connection = state.next_connection_id();
        state
            .register(
                "group".to_string(),
                "peer-a".to_string(),
                first_connection,
                writer_a,
                disconnect_a,
                1,
            )
            .unwrap();

        let (writer_b, _reader_b) = mpsc::channel(4);
        let (disconnect_b, _disconnect_reader_b) = tokio::sync::watch::channel(false);
        let second_connection = state.next_connection_id();
        let err = state
            .register(
                "group".to_string(),
                "peer-b".to_string(),
                second_connection,
                writer_b,
                disconnect_b,
                1,
            )
            .unwrap_err();

        assert_eq!(err, RelayRegistrationError::GroupPeerLimitReached);
    }

    #[test]
    fn tcp_forwarding_queue_is_bounded() {
        let mut state = TcpRelayServerState::default();
        let source_connection = state.next_connection_id();
        let target_connection = state.next_connection_id();
        let (writer_tx, _writer_rx) = mpsc::channel(1);
        let (disconnect_tx, disconnect_rx) = tokio::sync::watch::channel(false);
        state
            .register(
                "group".to_string(),
                "target".to_string(),
                target_connection,
                writer_tx,
                disconnect_tx,
                2,
            )
            .unwrap();

        let frame = std::sync::Arc::<[u8]>::from([1, 2, 3]);
        assert_eq!(
            state.forward_frame("group", source_connection, frame.clone()),
            RelayForwardResult {
                forwarded: 1,
                disconnected_slow_peers: 0,
            }
        );
        assert_eq!(
            state.forward_frame("group", source_connection, frame),
            RelayForwardResult {
                forwarded: 0,
                disconnected_slow_peers: 1,
            }
        );
        assert!(*disconnect_rx.borrow());
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

    #[tokio::test]
    async fn websocket_relay_tunnels_forward_existing_udp_protocol_both_ways() {
        let (cancel_tx, _) = broadcast::channel(1);
        let server =
            BoundTcpRelayServer::bind(TcpRelayServerConfig::new("127.0.0.1:0".parse().unwrap()))
                .await
                .expect("server should bind");
        let relay_addr = server.local_addr().expect("server addr");
        let server_task = tokio::spawn(server.run_websocket(cancel_tx.subscribe()));
        let relay_url = format!("ws://{relay_addr}/relay");

        let host_app = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("host app should bind");
        let host_app_addr = host_app.local_addr().expect("host app addr");
        let (host_sender, host_receiver) = host_app.split();

        let client_app = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("client app should bind");
        let (client_sender, client_receiver) = client_app.split();

        let host_tunnel = BoundWebSocketRelayTunnel::bind(
            WebSocketRelayTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                relay_url.clone(),
                "group",
                "host",
            )
            .unwrap()
            .with_local_target_addr(host_app_addr),
        )
        .await
        .expect("host tunnel should bind");
        let host_tunnel_addr = host_tunnel.local_addr().expect("host tunnel addr");
        let host_tunnel_task = tokio::spawn(host_tunnel.run(cancel_tx.subscribe()));

        let client_tunnel = BoundWebSocketRelayTunnel::bind(
            WebSocketRelayTunnelConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                relay_url,
                "group",
                "client",
            )
            .unwrap(),
        )
        .await
        .expect("client tunnel should bind");
        let client_tunnel_addr = client_tunnel.local_addr().expect("client tunnel addr");
        let client_tunnel_task = tokio::spawn(client_tunnel.run(cancel_tx.subscribe()));

        tokio::time::sleep(Duration::from_millis(100)).await;
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
