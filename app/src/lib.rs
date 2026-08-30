#[cfg(target_os = "macos")]
mod design_system;
mod mesh_admin;
pub mod preferences;
#[cfg(target_os = "macos")]
mod ui;

use remote_core::discovery::{
    DEFAULT_PEER_TTL, DiscoveredPeer, DiscoveryAnnouncement, DiscoveryCapabilities, DiscoveryEvent,
    DiscoveryPeerSnapshot, DiscoveryRouteOverride, DiscoveryRuntimeConfig, DiscoveryScope,
    REMOTE_PLAY_DISCOVERY_ENV, discovery_port_from_env, run_discovery_runtime,
};
use remote_core::mesh::{
    AppPrivateMeshConfigStore, EASYTIER_SIDECAR_LOG_FILE_NAME, EasyTierBinaryLocator,
    EasyTierHealthMonitorConfig, EasyTierHealthMonitorHandle, EasyTierHealthSnapshot,
    EasyTierProcessState, EasyTierSidecarManager, EasyTierSidecarRuntimeError, MeshStoreError,
    REMOTE_PLAY_MESH_ENV, default_app_private_mesh_dir, spawn_easytier_health_monitor,
    spawn_easytier_static_health_monitor,
};
use remote_core::net::{DEFAULT_CONTROL_PORT, UdpMultiplexer, UdpSender};
use remote_core::relay::{
    BoundTcpRelayTunnel, BoundWebSocketRelayTunnel, RelayConfigError, TcpRelayTunnelConfig,
    WebSocketRelayTunnelConfig, derive_relay_group_id,
};
use remote_core::role::{
    RoleChange, RolePeer, RoleSession, RoleState, RoleStateError, RoleStateMachine,
    RoleStateMachineConfig,
};
use remote_core::stats::Statistics;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, AtomicU32, Ordering::Relaxed},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

pub use client::{
    ClientMediaRuntime, ClientMediaRuntimeStatus, ClientSessionEvent, ClientSessionReceiverConfig,
    ClipboardRuntimeControl, FileTransferRuntimeControl, HostStats, MacDecodedVideoFrame,
    MeshPairingControl, MeshPairingMessageKind, MeshPairingSnapshot, TalkbackRuntimeControl,
    decoded_video_frame_surface, decoded_video_frame_surface_with_fit,
    spawn_client_session_receiver, start_clipboard_runtime_control,
    start_file_transfer_runtime_control, start_talkback_runtime_control,
};
pub use host::{HostServiceConfig, run_host_service};
#[cfg(target_os = "macos")]
pub use ui::run_unified_gui;

#[cfg(not(target_os = "macos"))]
pub async fn run_unified_gui(_config: UnifiedRuntimeConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    eprintln!("GUI is only available on macOS. Running in daemon/host headless mode.");
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnifiedAppConfig {
    pub role_config: RoleStateMachineConfig,
    pub first_session_id: u32,
}

impl Default for UnifiedAppConfig {
    fn default() -> Self {
        Self {
            role_config: RoleStateMachineConfig::default(),
            first_session_id: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDevice {
    pub device_id: String,
    pub display_name: String,
    pub endpoint: SocketAddr,
    pub scope: DiscoveryScope,
    pub can_stream: bool,
    pub can_view: bool,
    pub online: bool,
    pub last_seen_ms: u64,
}

impl AppDevice {
    pub fn is_streamable(&self) -> bool {
        self.online && self.can_stream && self.endpoint.port() != 0
    }

    pub fn role_peer(&self) -> RolePeer {
        RolePeer::new(
            self.device_id.clone(),
            self.display_name.clone(),
            self.endpoint,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewingRequest {
    pub change: RoleChange,
    pub target: SocketAddr,
    pub session_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamStartOptions {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

impl Default for StreamStartOptions {
    fn default() -> Self {
        let prefs = crate::preferences::UserPreferences::load_or_default();
        Self {
            width: prefs.stream.width,
            height: prefs.stream.height,
            fps: prefs.stream.fps,
            bitrate_kbps: prefs.stream.bitrate_kbps,
        }
    }
}

impl StreamStartOptions {
    fn start_message(self, session_id: u32) -> protocol::ControlMessage {
        protocol::ControlMessage::StartStream {
            width: self.width,
            height: self.height,
            fps: self.fps,
            bitrate_kbps: self.bitrate_kbps,
            session_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedDisconnectResult {
    pub change: Option<RoleChange>,
    pub target: Option<SocketAddr>,
    pub session_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedAppError {
    DeviceNotFound(String),
    DeviceNotStreamable(String),
    NoClientControlSender,
    ControlSend(String),
    Role(RoleStateError),
}

impl fmt::Display for UnifiedAppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnifiedAppError::DeviceNotFound(device_id) => {
                write!(f, "device {device_id:?} was not found")
            }
            UnifiedAppError::DeviceNotStreamable(device_id) => {
                write!(f, "device {device_id:?} is not streamable")
            }
            UnifiedAppError::NoClientControlSender => {
                f.write_str("client control sender is not configured")
            }
            UnifiedAppError::ControlSend(err) => write!(f, "control message send failed: {err}"),
            UnifiedAppError::Role(err) => write!(f, "{err}"),
        }
    }
}

impl Error for UnifiedAppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            UnifiedAppError::Role(err) => Some(err),
            UnifiedAppError::DeviceNotFound(_)
            | UnifiedAppError::DeviceNotStreamable(_)
            | UnifiedAppError::NoClientControlSender
            | UnifiedAppError::ControlSend(_) => None,
        }
    }
}

impl From<RoleStateError> for UnifiedAppError {
    fn from(value: RoleStateError) -> Self {
        Self::Role(value)
    }
}

#[derive(Debug, Clone)]
pub struct UnifiedAppRuntime {
    role: RoleStateMachine,
    devices: BTreeMap<String, AppDevice>,
    next_session_id: u32,
}

impl UnifiedAppRuntime {
    pub fn new(config: UnifiedAppConfig) -> Self {
        Self {
            role: RoleStateMachine::with_config(config.role_config),
            devices: BTreeMap::new(),
            next_session_id: config.first_session_id.max(1),
        }
    }

    pub fn role_state(&self) -> &RoleState {
        self.role.state()
    }

    pub fn devices(&self) -> Vec<AppDevice> {
        self.devices.values().cloned().collect()
    }

    pub fn streamable_devices(&self) -> Vec<AppDevice> {
        self.devices
            .values()
            .filter(|device| device.is_streamable())
            .cloned()
            .collect()
    }

    pub fn apply_discovery_snapshot(&mut self, snapshot: &DiscoveryPeerSnapshot) {
        let mut seen = BTreeSet::new();
        let mut selected = BTreeMap::new();
        for peer in snapshot.peers() {
            let device = app_device_from_peer(peer);
            seen.insert(device.device_id.clone());
            match selected.get(&device.device_id) {
                Some(existing)
                    if app_device_route_rank(existing) <= app_device_route_rank(&device) => {}
                _ => {
                    selected.insert(device.device_id.clone(), device);
                }
            }
        }

        for (device_id, device) in selected {
            self.devices.insert(device_id, device);
        }

        for (device_id, device) in &mut self.devices {
            if !seen.contains(device_id) {
                device.online = false;
            }
        }
    }

    pub fn connect_device(
        &mut self,
        device_id: &str,
        now_ms: u64,
    ) -> Result<ViewingRequest, UnifiedAppError> {
        let device = self
            .devices
            .get(device_id)
            .ok_or_else(|| UnifiedAppError::DeviceNotFound(device_id.to_string()))?
            .clone();
        if !device.is_streamable() {
            return Err(UnifiedAppError::DeviceNotStreamable(device_id.to_string()));
        }

        let session_id = self.allocate_session_id();
        let target = device.endpoint;
        let change = self
            .role
            .start_viewing(device.role_peer(), session_id, now_ms)?;
        Ok(ViewingRequest {
            change,
            target,
            session_id,
        })
    }

    pub fn mark_viewing_connected(
        &mut self,
        session_id: u32,
        now_ms: u64,
    ) -> Result<RoleChange, UnifiedAppError> {
        self.role
            .mark_viewing_connected(session_id, now_ms)
            .map_err(Into::into)
    }

    pub fn accept_inbound_stream(
        &mut self,
        peer: RolePeer,
        session_id: u32,
        now_ms: u64,
    ) -> Result<RoleChange, UnifiedAppError> {
        self.role
            .accept_serving(peer, session_id, now_ms)
            .map_err(Into::into)
    }

    pub fn active_session(&self) -> Option<&RoleSession> {
        self.role.state().session()
    }

    pub fn record_activity(
        &mut self,
        session_id: u32,
        now_ms: u64,
    ) -> Result<RoleChange, UnifiedAppError> {
        self.role
            .record_activity(session_id, now_ms)
            .map_err(Into::into)
    }

    pub fn stop_session(&mut self, session_id: u32) -> Result<Option<RoleChange>, UnifiedAppError> {
        self.role.stop_session(session_id).map_err(Into::into)
    }

    pub fn expire_timed_out(&mut self, now_ms: u64) -> Option<RoleChange> {
        self.role.expire_timed_out(now_ms)
    }

    fn allocate_session_id(&mut self) -> u32 {
        let session_id = self.next_session_id.max(1);
        self.next_session_id = session_id.wrapping_add(1);
        if self.next_session_id == 0 {
            self.next_session_id = 1;
        }
        session_id
    }
}

fn app_device_route_rank(device: &AppDevice) -> u8 {
    match device.scope {
        DiscoveryScope::Lan | DiscoveryScope::Mesh => 0,
        DiscoveryScope::Relay => 1,
    }
}

impl Default for UnifiedAppRuntime {
    fn default() -> Self {
        Self::new(UnifiedAppConfig::default())
    }
}

#[derive(Default)]
pub struct UnifiedServiceOwnerConfig {
    pub app: UnifiedAppConfig,
    pub mesh: Option<UnifiedMeshRuntimeConfig>,
    pub relay: Option<UnifiedRelayRuntimeConfig>,
    pub discovery: Option<DiscoveryRuntimeConfig>,
    pub passive_host: Option<HostServiceConfig>,
    pub client_receiver: Option<ClientSessionReceiverConfig>,
    pub client_session_event_rx: Option<mpsc::UnboundedReceiver<ClientSessionEvent>>,
    pub session_timeout_monitor: Option<UnifiedSessionTimeoutMonitorConfig>,
    pub viewing_keepalive: Option<UnifiedViewingKeepaliveConfig>,
    pub client_control_sender: Option<UdpSender>,
    pub side_services: UnifiedSideServiceControls,
    pub runtime_reload: Option<UnifiedRuntimeReloadConfig>,
}

pub struct UnifiedRuntimeReloadConfig {
    pub runtime: UnifiedRuntimeConfig,
    pub reload_rx: mpsc::UnboundedReceiver<()>,
}

pub const REMOTE_PLAY_RELAY_ENV: &str = "REMOTE_PLAY_RELAY";
pub const REMOTE_PLAY_RELAY_SERVER_ADDR_ENV: &str = "REMOTE_PLAY_RELAY_SERVER_ADDR";
pub const REMOTE_PLAY_RELAY_CONTROL_BIND_ADDR_ENV: &str = "REMOTE_PLAY_RELAY_CONTROL_BIND_ADDR";
pub const REMOTE_PLAY_RELAY_DISCOVERY_BIND_ADDR_ENV: &str = "REMOTE_PLAY_RELAY_DISCOVERY_BIND_ADDR";
pub const REMOTE_PLAY_RELAY_LOG_ENV: &str = "REMOTE_PLAY_RELAY_LOG";
pub const DEFAULT_RELAY_SERVER_URL: &str = "wss://relay.hackerlife.fun:8443/v1/relay";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnifiedSessionTimeoutMonitorConfig {
    pub poll_interval: Duration,
}

impl Default for UnifiedSessionTimeoutMonitorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(250),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnifiedViewingKeepaliveConfig {
    pub interval: Duration,
}

impl Default for UnifiedViewingKeepaliveConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(500),
        }
    }
}

#[derive(Default)]
struct UnifiedSideServicePreferences {
    clipboard_sync: AtomicBool,
    file_transfer: AtomicBool,
    talkback: AtomicBool,
}

#[derive(Clone)]
pub struct UnifiedSideServiceControls {
    pub clipboard: Option<ClipboardRuntimeControl>,
    pub file_transfer: Option<FileTransferRuntimeControl>,
    pub talkback: Option<TalkbackRuntimeControl>,
    preferences: Arc<UnifiedSideServicePreferences>,
}

impl Default for UnifiedSideServiceControls {
    fn default() -> Self {
        Self::new(None, None, None)
    }
}

impl UnifiedSideServiceControls {
    fn new(
        clipboard: Option<ClipboardRuntimeControl>,
        file_transfer: Option<FileTransferRuntimeControl>,
        talkback: Option<TalkbackRuntimeControl>,
    ) -> Self {
        let preferences = UnifiedSideServicePreferences {
            clipboard_sync: AtomicBool::new(clipboard.is_some()),
            file_transfer: AtomicBool::new(file_transfer.is_some()),
            talkback: AtomicBool::new(talkback.is_some()),
        };
        Self {
            clipboard,
            file_transfer,
            talkback,
            preferences: Arc::new(preferences),
        }
    }

    fn supports_clipboard_sync(&self) -> bool {
        self.clipboard.is_some()
    }

    fn supports_file_transfer(&self) -> bool {
        self.file_transfer.is_some()
    }

    fn supports_talkback(&self) -> bool {
        self.talkback.is_some()
    }

    fn clipboard_sync_enabled(&self) -> bool {
        self.supports_clipboard_sync() && self.preferences.clipboard_sync.load(Relaxed)
    }

    fn file_transfer_enabled(&self) -> bool {
        self.supports_file_transfer() && self.preferences.file_transfer.load(Relaxed)
    }

    fn talkback_enabled(&self) -> bool {
        self.supports_talkback() && self.preferences.talkback.load(Relaxed)
    }

    fn start_for_viewing(&self, target: SocketAddr, session_id: u32) {
        if self.clipboard_sync_enabled()
            && let Some(control) = &self.clipboard
        {
            control.start(target);
        }
        if self.file_transfer_enabled()
            && let Some(control) = &self.file_transfer
        {
            control.start(target);
        }
        if self.talkback_enabled()
            && let Some(control) = &self.talkback
        {
            control.start(target, session_id);
        }
    }

    fn set_clipboard_sync_enabled(&self, enabled: bool, target: Option<SocketAddr>) -> bool {
        let Some(control) = &self.clipboard else {
            return false;
        };
        self.preferences.clipboard_sync.store(enabled, Relaxed);
        let mut prefs = crate::preferences::UserPreferences::load_or_default();
        prefs.side_services.clipboard_sync = enabled;
        let _ = prefs.save();
        if enabled {
            if let Some(target) = target {
                control.start(target);
            }
        } else {
            control.stop();
        }
        true
    }

    fn set_file_transfer_enabled(&self, enabled: bool, target: Option<SocketAddr>) -> bool {
        let Some(control) = &self.file_transfer else {
            return false;
        };
        self.preferences.file_transfer.store(enabled, Relaxed);
        let mut prefs = crate::preferences::UserPreferences::load_or_default();
        prefs.side_services.file_transfer = enabled;
        let _ = prefs.save();
        if enabled {
            if let Some(target) = target {
                control.start(target);
            }
        } else {
            control.stop();
        }
        true
    }

    fn set_talkback_enabled(&self, enabled: bool, session: Option<(SocketAddr, u32)>) -> bool {
        let Some(control) = &self.talkback else {
            return false;
        };
        self.preferences.talkback.store(enabled, Relaxed);
        let mut prefs = crate::preferences::UserPreferences::load_or_default();
        prefs.side_services.talkback = enabled;
        let _ = prefs.save();
        if enabled {
            if let Some((target, session_id)) = session {
                control.start(target, session_id);
            }
        } else {
            control.stop();
        }
        true
    }

    fn stop_all(&self) {
        if let Some(control) = &self.clipboard {
            control.stop();
        }
        if let Some(control) = &self.file_transfer {
            control.stop();
        }
        if let Some(control) = &self.talkback {
            control.stop();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedMeshRuntimeConfig {
    pub display_name: String,
    pub mesh_dir: PathBuf,
    pub locator: EasyTierBinaryLocator,
    pub health_config: Option<EasyTierHealthMonitorConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedRelayRuntimeConfig {
    pub endpoint: UnifiedRelayEndpoint,
    pub control_group_id: String,
    pub discovery_group_id: String,
    pub peer_id: String,
    pub control_bind_addr: SocketAddr,
    pub discovery_bind_addr: SocketAddr,
    pub host_control_target_addr: Option<SocketAddr>,
    pub discovery_target_addr: Option<SocketAddr>,
    pub log_events: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedRelayEndpoint {
    Tcp(SocketAddr),
    WebSocket(String),
}

impl UnifiedRelayRuntimeConfig {
    pub fn new(
        relay_addr: SocketAddr,
        group_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> Self {
        let group_id = group_id.into();
        Self {
            endpoint: UnifiedRelayEndpoint::Tcp(relay_addr),
            control_group_id: relay_control_group(&group_id),
            discovery_group_id: relay_discovery_group(&group_id),
            peer_id: peer_id.into(),
            control_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            discovery_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            host_control_target_addr: None,
            discovery_target_addr: None,
            log_events: false,
        }
    }

    pub fn from_mesh_secret(
        endpoint: UnifiedRelayEndpoint,
        network_name: &str,
        network_secret: &str,
        peer_id: impl Into<String>,
    ) -> Result<Self, RelayConfigError> {
        Ok(Self {
            endpoint,
            control_group_id: derive_relay_group_id(network_name, network_secret, "control")?,
            discovery_group_id: derive_relay_group_id(network_name, network_secret, "discovery")?,
            peer_id: peer_id.into(),
            control_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            discovery_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            host_control_target_addr: None,
            discovery_target_addr: None,
            log_events: false,
        })
    }
}

impl UnifiedMeshRuntimeConfig {
    pub fn app_private(display_name: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            mesh_dir: default_app_private_mesh_dir(),
            locator: EasyTierBinaryLocator::from_environment(),
            health_config: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedRuntimeConfig {
    pub display_name: String,
    pub mesh_dir: PathBuf,
    pub host_bind_addr: SocketAddr,
    pub client_bind_addr: SocketAddr,
    pub discovery_port: u16,
    pub enable_mesh: bool,
    pub relay_endpoint: Option<UnifiedRelayEndpoint>,
    pub relay_control_bind_addr: SocketAddr,
    pub relay_discovery_bind_addr: SocketAddr,
    pub relay_log_events: bool,
    pub enable_discovery: bool,
    pub enable_passive_host: bool,
    pub enable_client_receiver: bool,
    pub enable_clipboard_sync: bool,
    pub enable_file_transfer: bool,
    pub enable_talkback: bool,
    pub enable_viewer_media: bool,
    pub enable_session_timeout_monitor: bool,
}

impl UnifiedRuntimeConfig {
    pub fn app_defaults() -> Self {
        Self {
            display_name: "RemotePlay".to_string(),
            mesh_dir: default_app_private_mesh_dir(),
            host_bind_addr: SocketAddr::from(([0, 0, 0, 0], DEFAULT_CONTROL_PORT)),
            client_bind_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
            discovery_port: remote_core::discovery::DEFAULT_DISCOVERY_PORT,
            enable_mesh: false,
            relay_endpoint: Some(UnifiedRelayEndpoint::WebSocket(
                DEFAULT_RELAY_SERVER_URL.to_string(),
            )),
            relay_control_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_discovery_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_log_events: false,
            enable_discovery: true,
            enable_passive_host: true,
            enable_client_receiver: true,
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
            enable_viewer_media: true,
            enable_session_timeout_monitor: true,
        }
    }

    pub fn from_env() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut config = Self::app_defaults();
        if let Ok(display_name) = std::env::var("REMOTE_PLAY_DISPLAY_NAME")
            && !display_name.trim().is_empty()
        {
            config.display_name = display_name.trim().to_string();
        }
        config.mesh_dir = default_app_private_mesh_dir();
        config.host_bind_addr =
            env_socket_addr("REMOTE_PLAY_HOST_BIND_ADDR", config.host_bind_addr)?;
        config.client_bind_addr =
            env_socket_addr("REMOTE_PLAY_CLIENT_BIND_ADDR", config.client_bind_addr)?;
        config.discovery_port = discovery_port_from_env()?;
        config.enable_mesh = env_flag_or(REMOTE_PLAY_MESH_ENV, bundled_mesh_available());
        config.relay_endpoint = if env_flag_or(REMOTE_PLAY_RELAY_ENV, true) {
            env_optional_relay_endpoint(REMOTE_PLAY_RELAY_SERVER_ADDR_ENV)?
                .or(config.relay_endpoint)
        } else {
            None
        };
        config.relay_control_bind_addr = env_socket_addr(
            REMOTE_PLAY_RELAY_CONTROL_BIND_ADDR_ENV,
            config.relay_control_bind_addr,
        )?;
        config.relay_discovery_bind_addr = env_socket_addr(
            REMOTE_PLAY_RELAY_DISCOVERY_BIND_ADDR_ENV,
            config.relay_discovery_bind_addr,
        )?;
        config.relay_log_events = env_flag_or(REMOTE_PLAY_RELAY_LOG_ENV, false);
        config.enable_discovery = env_flag_or(REMOTE_PLAY_DISCOVERY_ENV, true);
        config.enable_passive_host = env_flag_or("REMOTE_PLAY_PASSIVE_HOST", true);
        config.enable_client_receiver = env_flag_or("REMOTE_PLAY_CLIENT_RECEIVER", true);
        config.enable_clipboard_sync = env_flag_or("REMOTE_PLAY_CLIPBOARD_SYNC", false);
        config.enable_file_transfer = env_flag_or("REMOTE_PLAY_FILE_TRANSFER", false)
            || env_flag_or("REMOTE_PLAY_FILE_CLIPBOARD", false)
            || std::env::var_os("REMOTE_PLAY_SEND_FILE").is_some();
        config.enable_talkback = env_flag_or("REMOTE_PLAY_TALKBACK", false);
        config.enable_viewer_media = env_flag_or("REMOTE_PLAY_VIEWER_MEDIA", true);
        config.enable_session_timeout_monitor =
            env_flag_or("REMOTE_PLAY_SESSION_TIMEOUT_MONITOR", true);
        Ok(config)
    }
}

impl Default for UnifiedRuntimeConfig {
    fn default() -> Self {
        Self::app_defaults()
    }
}

pub struct UnifiedRuntimeHandle {
    pub owner: Arc<UnifiedServiceOwner>,
    pub stats: Arc<Statistics>,
    pub host_stats: Option<Arc<RwLock<HostStats>>>,
    pub viewer_frame: Option<Arc<Mutex<Option<MacDecodedVideoFrame>>>>,
    pub viewer_media_status: UnifiedViewerMediaStatus,
    pub mesh_pairing: Option<MeshPairingControl>,
    _viewer_media: Option<ClientMediaRuntime>,
    media_sink_tasks: Vec<AbortOnDropTask>,
}

impl UnifiedRuntimeHandle {
    pub fn background_task_count(&self) -> usize {
        self.owner.task_count() + self.media_sink_tasks.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedViewerMediaStatus {
    Disabled,
    Ready,
    AudioUnavailable { reason: String },
    Unavailable { reason: String },
}

impl From<ClientMediaRuntimeStatus> for UnifiedViewerMediaStatus {
    fn from(value: ClientMediaRuntimeStatus) -> Self {
        match value {
            ClientMediaRuntimeStatus::Ready => Self::Ready,
            ClientMediaRuntimeStatus::AudioUnavailable { reason } => {
                Self::AudioUnavailable { reason }
            }
        }
    }
}

type ViewerAudioTx = mpsc::Sender<client::audio_player::AudioPlayerEvent>;
type ViewerDecodeTx = mpsc::Sender<(protocol::RtpPacket, u32)>;

struct ViewerMediaChannels {
    audio_tx: ViewerAudioTx,
    decode_tx: ViewerDecodeTx,
    viewer_frame: Option<Arc<Mutex<Option<MacDecodedVideoFrame>>>>,
    viewer_media: Option<ClientMediaRuntime>,
    status: UnifiedViewerMediaStatus,
}

pub async fn start_unified_runtime(
    config: UnifiedRuntimeConfig,
) -> Result<UnifiedRuntimeHandle, Box<dyn Error + Send + Sync>> {
    remote_core::init_crypto_provider();
    let stats = Statistics::new();
    let mut media_sink_tasks = Vec::new();
    let mut owner_config = UnifiedServiceOwnerConfig::default();
    let mut viewer_frame = None;
    let mut viewer_media = None;
    let mut viewer_media_status = UnifiedViewerMediaStatus::Disabled;
    let (reload_tx, reload_rx) = if config.enable_mesh || config.enable_discovery {
        let (tx, rx) = mpsc::unbounded_channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let mesh_pairing = match MeshPairingControl::load_or_create(
        config.mesh_dir.clone(),
        config.display_name.clone(),
    ) {
        Ok(control) => Some(if let Some(reload_tx) = &reload_tx {
            control.with_mesh_reload_tx(reload_tx.clone())
        } else {
            control
        }),
        Err(err) => {
            eprintln!("Device-group pairing setup is unavailable: {err}");
            None
        }
    };

    if config.enable_mesh {
        owner_config.mesh = Some(UnifiedMeshRuntimeConfig {
            display_name: config.display_name.clone(),
            mesh_dir: config.mesh_dir.clone(),
            locator: EasyTierBinaryLocator::from_environment(),
            health_config: None,
        });
    }

    owner_config.relay = build_unified_relay_config(&config)?;

    if config.enable_discovery {
        owner_config.discovery = Some(build_unified_discovery_config(&config, None)?);
    }

    if config.enable_passive_host {
        owner_config.passive_host = Some(HostServiceConfig {
            bind_addr: config.host_bind_addr,
            stats: stats.clone(),
        });
    }

    let mut host_stats = None;
    if config.enable_client_receiver {
        let client_mux = UdpMultiplexer::bind(&config.client_bind_addr.to_string()).await?;
        let (client_sender, client_receiver) = client_mux.split();
        let media_channels = start_viewer_media_channels(
            config.enable_viewer_media,
            stats.clone(),
            &mut media_sink_tasks,
            ClientMediaRuntime::start,
        );
        let audio_tx = media_channels.audio_tx;
        let decode_tx = media_channels.decode_tx;
        viewer_frame = media_channels.viewer_frame;
        viewer_media = media_channels.viewer_media;
        viewer_media_status = media_channels.status;

        let (session_event_tx, session_event_rx) = mpsc::unbounded_channel();
        let host_stats_inner = Arc::new(RwLock::new(HostStats::default()));

        owner_config.client_control_sender = Some(client_sender.clone());
        owner_config.viewing_keepalive = Some(UnifiedViewingKeepaliveConfig::default());
        owner_config.client_session_event_rx = Some(session_event_rx);
        owner_config.side_services = build_client_side_services(&config, client_sender.clone());
        owner_config.client_receiver = Some(ClientSessionReceiverConfig {
            bind_addr: client_mux.local_addr()?,
            udp_receiver: client_receiver,
            stats: stats.clone(),
            active_session_id: Arc::new(AtomicU32::new(0)),
            host_stats: host_stats_inner.clone(),
            audio_tx,
            decode_tx,
            clipboard_control: owner_config.side_services.clipboard.clone(),
            file_transfer_control: owner_config.side_services.file_transfer.clone(),
            session_event_tx: Some(session_event_tx),
        });
        host_stats = Some(host_stats_inner);
    }

    if config.enable_session_timeout_monitor {
        owner_config.session_timeout_monitor = Some(UnifiedSessionTimeoutMonitorConfig::default());
    }

    if let Some(reload_rx) = reload_rx {
        owner_config.runtime_reload = Some(UnifiedRuntimeReloadConfig {
            runtime: config.clone(),
            reload_rx,
        });
    }

    let owner = Arc::new(UnifiedServiceOwner::start(owner_config).await?);
    Ok(UnifiedRuntimeHandle {
        owner,
        stats,
        host_stats,
        viewer_frame,
        viewer_media_status,
        mesh_pairing,
        _viewer_media: viewer_media,
        media_sink_tasks,
    })
}

fn start_viewer_media_channels<F>(
    enable_viewer_media: bool,
    stats: Arc<Statistics>,
    media_sink_tasks: &mut Vec<AbortOnDropTask>,
    start_media: F,
) -> ViewerMediaChannels
where
    F: FnOnce(Arc<Statistics>) -> Result<ClientMediaRuntime, Box<dyn Error + Send + Sync>>,
{
    if !enable_viewer_media {
        let (audio_tx, decode_tx) = spawn_viewer_media_sinks(media_sink_tasks);
        return ViewerMediaChannels {
            audio_tx,
            decode_tx,
            viewer_frame: None,
            viewer_media: None,
            status: UnifiedViewerMediaStatus::Disabled,
        };
    }

    match start_media(stats) {
        Ok(media) => ViewerMediaChannels {
            audio_tx: media.audio_tx.clone(),
            decode_tx: media.decode_tx.clone(),
            viewer_frame: Some(media.shared_frame()),
            status: media.status.clone().into(),
            viewer_media: Some(media),
        },
        Err(err) => {
            let reason = err.to_string();
            eprintln!("Viewer media runtime unavailable; using sink mode: {reason}");
            let (audio_tx, decode_tx) = spawn_viewer_media_sinks(media_sink_tasks);
            ViewerMediaChannels {
                audio_tx,
                decode_tx,
                viewer_frame: None,
                viewer_media: None,
                status: UnifiedViewerMediaStatus::Unavailable { reason },
            }
        }
    }
}

fn spawn_viewer_media_sinks(
    media_sink_tasks: &mut Vec<AbortOnDropTask>,
) -> (ViewerAudioTx, ViewerDecodeTx) {
    let (audio_tx, mut audio_rx) = mpsc::channel(256);
    let (decode_tx, mut decode_rx) = mpsc::channel(256);
    media_sink_tasks.push(AbortOnDropTask(tokio::spawn(async move {
        while audio_rx.recv().await.is_some() {}
    })));
    media_sink_tasks.push(AbortOnDropTask(tokio::spawn(async move {
        while decode_rx.recv().await.is_some() {}
    })));
    (audio_tx, decode_tx)
}

fn build_client_side_services(
    config: &UnifiedRuntimeConfig,
    client_sender: UdpSender,
) -> UnifiedSideServiceControls {
    UnifiedSideServiceControls::new(
        config
            .enable_clipboard_sync
            .then(|| start_clipboard_runtime_control(client_sender.clone())),
        config
            .enable_file_transfer
            .then(|| start_file_transfer_runtime_control(client_sender.clone())),
        config
            .enable_talkback
            .then(|| start_talkback_runtime_control(client_sender)),
    )
}

fn build_unified_discovery_config(
    config: &UnifiedRuntimeConfig,
    virtual_ip: Option<IpAddr>,
) -> Result<DiscoveryRuntimeConfig, MeshStoreError> {
    let mesh_store = AppPrivateMeshConfigStore::new(&config.mesh_dir);
    let mesh_config = mesh_store.load_or_generate(&config.display_name)?;
    let control_port = if config.enable_passive_host {
        config.host_bind_addr.port()
    } else {
        0
    };
    let capabilities = DiscoveryCapabilities {
        can_stream: config.enable_passive_host,
        can_view: config.enable_client_receiver,
        file_transfer: config.enable_file_transfer,
        clipboard_sync: config.enable_clipboard_sync,
        talkback: config.enable_talkback,
    };
    let announcement = DiscoveryAnnouncement {
        network_name: mesh_config.network_name,
        device_id: mesh_config.node_id,
        display_name: mesh_config.display_name,
        control_port,
        virtual_ip,
        capabilities,
        scope: if virtual_ip.is_some() {
            DiscoveryScope::Mesh
        } else {
            DiscoveryScope::Lan
        },
        ttl: DEFAULT_PEER_TTL,
    };
    Ok(DiscoveryRuntimeConfig::lan_on_port(
        announcement,
        config.discovery_port,
    ))
}

fn build_unified_relay_config(
    config: &UnifiedRuntimeConfig,
) -> Result<Option<UnifiedRelayRuntimeConfig>, Box<dyn Error + Send + Sync>> {
    let Some(relay_endpoint) = config.relay_endpoint.clone() else {
        return Ok(None);
    };
    let mesh_store = AppPrivateMeshConfigStore::new(&config.mesh_dir);
    let mesh_config = mesh_store.load_or_generate(&config.display_name)?;
    let mut relay_config = UnifiedRelayRuntimeConfig::from_mesh_secret(
        relay_endpoint,
        &mesh_config.network_name,
        mesh_config.network_secret.expose_secret(),
        mesh_config.node_id,
    )?;
    relay_config.control_bind_addr = config.relay_control_bind_addr;
    relay_config.discovery_bind_addr = config.relay_discovery_bind_addr;
    relay_config.host_control_target_addr = config
        .enable_passive_host
        .then_some(config.host_bind_addr)
        .and_then(local_udp_target_for_bind);
    relay_config.discovery_target_addr = config
        .enable_discovery
        .then_some(SocketAddr::from(([0, 0, 0, 0], config.discovery_port)))
        .and_then(local_udp_target_for_bind);
    relay_config.log_events = config.relay_log_events;
    Ok(Some(relay_config))
}

fn local_udp_target_for_bind(bind_addr: SocketAddr) -> Option<SocketAddr> {
    if bind_addr.port() == 0 {
        return None;
    }
    let ip = if bind_addr.ip().is_unspecified() {
        match bind_addr {
            SocketAddr::V4(_) => IpAddr::from([127, 0, 0, 1]),
            SocketAddr::V6(_) => IpAddr::from(std::net::Ipv6Addr::LOCALHOST),
        }
    } else {
        bind_addr.ip()
    };
    Some(SocketAddr::new(ip, bind_addr.port()))
}

fn env_optional_relay_endpoint(
    name: &'static str,
) -> Result<Option<UnifiedRelayEndpoint>, Box<dyn Error + Send + Sync>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        return Ok(Some(UnifiedRelayEndpoint::WebSocket(trimmed.to_string())));
    }
    trimmed
        .parse()
        .map(UnifiedRelayEndpoint::Tcp)
        .map(Some)
        .map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{name}={trimmed:?} is not a valid socket address or WebSocket URL: {err}"),
            )
            .into()
        })
}

fn env_socket_addr(
    name: &'static str,
    fallback: SocketAddr,
) -> Result<SocketAddr, Box<dyn Error + Send + Sync>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(fallback);
    };
    let value = value.to_string_lossy();
    value.parse().map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name}={value:?} is not a valid socket address: {err}"),
        )
        .into()
    })
}

fn env_flag_or(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => match value.as_str() {
            "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON" => true,
            "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

fn bundled_mesh_available() -> bool {
    EasyTierBinaryLocator::from_environment().locate().is_ok()
}

#[derive(Clone)]
struct ReloadableMeshRuntime {
    monitor: Arc<Mutex<Option<EasyTierHealthMonitorHandle>>>,
    health_tx: watch::Sender<EasyTierHealthSnapshot>,
}

#[derive(Clone)]
struct ReloadableDiscoveryRuntime {
    runtime: Arc<Mutex<Option<UnifiedDiscoveryRuntime>>>,
    snapshot_tx: watch::Sender<DiscoveryPeerSnapshot>,
}

struct UnifiedRelayRuntime {
    control_endpoint: SocketAddr,
    discovery_endpoint: SocketAddr,
    cancel_tx: broadcast::Sender<()>,
    tasks: Vec<AbortOnDropTask>,
}

impl UnifiedRelayRuntime {
    fn task_count(&self) -> usize {
        self.tasks.len()
    }
}

impl Drop for UnifiedRelayRuntime {
    fn drop(&mut self) {
        let _ = self.cancel_tx.send(());
    }
}

struct UnifiedDiscoveryRuntime {
    #[cfg_attr(not(test), allow(dead_code))]
    announcement: DiscoveryAnnouncement,
    #[cfg_attr(not(test), allow(dead_code))]
    route_overrides: Vec<DiscoveryRouteOverride>,
    cancel_tx: broadcast::Sender<()>,
    tasks: Vec<AbortOnDropTask>,
}

impl UnifiedDiscoveryRuntime {
    fn task_count(&self) -> usize {
        self.tasks.len()
    }
}

impl Drop for UnifiedDiscoveryRuntime {
    fn drop(&mut self) {
        let _ = self.cancel_tx.send(());
    }
}

pub struct UnifiedServiceOwner {
    runtime: Arc<Mutex<UnifiedAppRuntime>>,
    mesh_runtime: Option<ReloadableMeshRuntime>,
    mesh_health_rx: Option<watch::Receiver<EasyTierHealthSnapshot>>,
    relay_runtime: Option<UnifiedRelayRuntime>,
    discovery_runtime: Option<ReloadableDiscoveryRuntime>,
    discovery_snapshot_rx: Option<watch::Receiver<DiscoveryPeerSnapshot>>,
    client_control_sender: Option<UdpSender>,
    client_input_tx: Option<mpsc::UnboundedSender<(SocketAddr, protocol::InputEvent)>>,
    client_active_session_id: Option<Arc<AtomicU32>>,
    side_services: UnifiedSideServiceControls,
    tasks: Vec<AbortOnDropTask>,
}

impl UnifiedServiceOwner {
    pub async fn start(
        config: UnifiedServiceOwnerConfig,
    ) -> Result<Self, UnifiedServiceOwnerError> {
        let runtime = Arc::new(Mutex::new(UnifiedAppRuntime::new(config.app)));
        let mut tasks = Vec::new();
        let client_active_session_id = config
            .client_receiver
            .as_ref()
            .map(|receiver| receiver.active_session_id.clone());
        let (mesh_runtime, mesh_health_rx) = match config.mesh {
            Some(mesh_config) => {
                let monitor = start_unified_mesh_monitor(mesh_config).await?;
                let initial_snapshot = monitor.snapshot_rx.borrow().clone();
                let (health_tx, health_rx) = watch::channel(initial_snapshot);
                spawn_mesh_health_bridge(monitor.snapshot_rx.clone(), health_tx.clone());
                (
                    Some(ReloadableMeshRuntime {
                        monitor: Arc::new(Mutex::new(Some(monitor))),
                        health_tx,
                    }),
                    Some(health_rx),
                )
            }
            None => (None, None),
        };

        let relay_runtime = match config.relay {
            Some(relay_config) => Some(start_unified_relay_runtime(relay_config).await?),
            None => None,
        };

        let (discovery_runtime, discovery_snapshot_rx) = match config.discovery {
            Some(mut discovery_config) => {
                if discovery_config.announcement.virtual_ip.is_none()
                    && let Some(mesh_health_rx) = &mesh_health_rx
                    && let Some(virtual_ip) = mesh_health_rx.borrow().virtual_ip
                {
                    discovery_config.announcement.virtual_ip = Some(virtual_ip);
                    discovery_config.announcement.scope = DiscoveryScope::Mesh;
                }
                if let Some(relay_runtime) = &relay_runtime {
                    discovery_config
                        .announce_targets
                        .push(relay_runtime.discovery_endpoint);
                    discovery_config
                        .route_overrides
                        .push(DiscoveryRouteOverride {
                            source: relay_runtime.discovery_endpoint,
                            endpoint: relay_runtime.control_endpoint,
                            scope: DiscoveryScope::Relay,
                        });
                }
                let (snapshot_tx, snapshot_rx) = watch::channel(DiscoveryPeerSnapshot::default());
                let discovery =
                    spawn_unified_discovery_runtime(discovery_config, snapshot_tx.clone());
                tasks.push(spawn_discovery_snapshot_bridge(
                    runtime.clone(),
                    snapshot_rx.clone(),
                ));

                (
                    Some(ReloadableDiscoveryRuntime {
                        runtime: Arc::new(Mutex::new(Some(discovery))),
                        snapshot_tx,
                    }),
                    Some(snapshot_rx),
                )
            }
            None => (None, None),
        };

        if let Some(host_config) = config.passive_host {
            tasks.push(AbortOnDropTask(tokio::spawn(async move {
                if let Err(err) = run_host_service(host_config).await {
                    eprintln!("Unified passive host service stopped: {err}");
                }
            })));
        }

        if let Some(client_receiver_config) = config.client_receiver {
            tasks.push(AbortOnDropTask(spawn_client_session_receiver(
                client_receiver_config,
            )));
        }

        if let Some(client_session_event_rx) = config.client_session_event_rx {
            tasks.push(spawn_client_session_event_bridge(
                runtime.clone(),
                client_session_event_rx,
            ));
        }

        if let Some(timeout_config) = config.session_timeout_monitor {
            tasks.push(spawn_session_timeout_monitor(
                runtime.clone(),
                config.side_services.clone(),
                client_active_session_id.clone(),
                timeout_config,
            ));
        }

        if let (Some(keepalive_config), Some(sender)) = (
            config.viewing_keepalive,
            config.client_control_sender.clone(),
        ) {
            tasks.push(spawn_viewing_keepalive(
                runtime.clone(),
                sender,
                keepalive_config,
            ));
        }

        let client_input_tx = config.client_control_sender.clone().map(|sender| {
            let (input_tx, input_rx) = mpsc::unbounded_channel();
            tasks.push(spawn_client_input_sender(sender, input_rx));
            input_tx
        });

        if let Some(reload_config) = config.runtime_reload {
            tasks.push(spawn_unified_runtime_reload_task(
                runtime.clone(),
                mesh_runtime.clone(),
                discovery_runtime.clone(),
                reload_config,
            ));
        }

        Ok(Self {
            runtime,
            mesh_runtime,
            mesh_health_rx,
            relay_runtime,
            discovery_runtime,
            discovery_snapshot_rx,
            client_control_sender: config.client_control_sender,
            client_input_tx,
            client_active_session_id,
            side_services: config.side_services,
            tasks,
        })
    }

    pub fn runtime(&self) -> Arc<Mutex<UnifiedAppRuntime>> {
        self.runtime.clone()
    }

    pub fn mesh_health_rx(&self) -> Option<watch::Receiver<EasyTierHealthSnapshot>> {
        self.mesh_health_rx.clone()
    }

    pub fn discovery_snapshot_rx(&self) -> Option<watch::Receiver<DiscoveryPeerSnapshot>> {
        self.discovery_snapshot_rx.clone()
    }

    pub fn owns_mesh(&self) -> bool {
        self.mesh_runtime
            .as_ref()
            .and_then(|state| {
                state
                    .monitor
                    .lock()
                    .expect("mesh runtime lock")
                    .as_ref()
                    .map(|_| ())
            })
            .is_some()
    }

    pub fn owns_discovery(&self) -> bool {
        self.discovery_runtime
            .as_ref()
            .and_then(|state| {
                state
                    .runtime
                    .lock()
                    .expect("discovery runtime lock")
                    .as_ref()
                    .map(|_| ())
            })
            .is_some()
    }

    pub fn owns_relay(&self) -> bool {
        self.relay_runtime.is_some()
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
            + self
                .relay_runtime
                .as_ref()
                .map(UnifiedRelayRuntime::task_count)
                .unwrap_or(0)
            + self
                .discovery_runtime
                .as_ref()
                .and_then(|state| {
                    state
                        .runtime
                        .lock()
                        .expect("discovery runtime lock")
                        .as_ref()
                        .map(UnifiedDiscoveryRuntime::task_count)
                })
                .unwrap_or(0)
    }

    pub async fn connect_device(
        &self,
        device_id: &str,
        options: StreamStartOptions,
        now_ms: u64,
    ) -> Result<ViewingRequest, UnifiedAppError> {
        let sender = self
            .client_control_sender
            .clone()
            .ok_or(UnifiedAppError::NoClientControlSender)?;
        let request = {
            self.runtime
                .lock()
                .expect("unified runtime lock")
                .connect_device(device_id, now_ms)?
        };

        if let Some(active_session_id) = &self.client_active_session_id {
            active_session_id.store(request.session_id, Relaxed);
        }

        let message = options.start_message(request.session_id);
        if let Err(err) = sender.send_control(&message, request.target).await {
            if let Some(active_session_id) = &self.client_active_session_id {
                active_session_id.store(0, Relaxed);
            }
            let _ = self
                .runtime
                .lock()
                .expect("unified runtime lock")
                .stop_session(request.session_id);
            return Err(UnifiedAppError::ControlSend(err.to_string()));
        }

        self.side_services
            .start_for_viewing(request.target, request.session_id);
        Ok(request)
    }

    pub async fn update_stream_settings(
        &self,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<(), UnifiedAppError> {
        let (target, session_id) = {
            let runtime = self.runtime.lock().expect("unified runtime lock");
            match runtime.role_state() {
                RoleState::Viewing(session) | RoleState::Connecting(session) => {
                    (session.peer.endpoint, session.session_id)
                }
                _ => return Err(UnifiedAppError::Role(RoleStateError::NoActiveSession)),
            }
        };
        let Some(sender) = &self.client_control_sender else {
            return Err(UnifiedAppError::NoClientControlSender);
        };
        let msg = protocol::ControlMessage::UpdateStreamSettings {
            width,
            height,
            fps,
            bitrate_kbps,
            session_id,
        };
        sender
            .send_control(&msg, target)
            .await
            .map_err(|err| UnifiedAppError::ControlSend(err.to_string()))?;
        Ok(())
    }

    pub fn mark_viewing_connected(
        &self,
        session_id: u32,
        now_ms: u64,
    ) -> Result<RoleChange, UnifiedAppError> {
        self.runtime
            .lock()
            .expect("unified runtime lock")
            .mark_viewing_connected(session_id, now_ms)
    }

    pub fn queue_viewing_input(&self, event: protocol::InputEvent) -> bool {
        let target = {
            let runtime = self.runtime.lock().expect("unified runtime lock");
            match runtime.role_state() {
                RoleState::Viewing(session) => Some(session.peer.endpoint),
                RoleState::Idle | RoleState::Connecting(_) | RoleState::Serving(_) => None,
            }
        };
        let (Some(target), Some(input_tx)) = (target, &self.client_input_tx) else {
            return false;
        };
        input_tx.send((target, event)).is_ok()
    }

    pub fn supports_clipboard_sync(&self) -> bool {
        self.side_services.supports_clipboard_sync()
    }

    pub fn supports_file_transfer(&self) -> bool {
        self.side_services.supports_file_transfer()
    }

    pub fn supports_talkback(&self) -> bool {
        self.side_services.supports_talkback()
    }

    pub fn clipboard_sync_enabled(&self) -> bool {
        self.side_services.clipboard_sync_enabled()
    }

    pub fn file_transfer_enabled(&self) -> bool {
        self.side_services.file_transfer_enabled()
    }

    pub fn talkback_enabled(&self) -> bool {
        self.side_services.talkback_enabled()
    }

    pub fn set_clipboard_sync_enabled(&self, enabled: bool) -> bool {
        self.side_services.set_clipboard_sync_enabled(
            enabled,
            self.client_side_session().map(|s| s.peer.endpoint),
        )
    }

    pub fn set_file_transfer_enabled(&self, enabled: bool) -> bool {
        self.side_services
            .set_file_transfer_enabled(enabled, self.client_side_session().map(|s| s.peer.endpoint))
    }

    pub fn set_talkback_enabled(&self, enabled: bool) -> bool {
        self.side_services.set_talkback_enabled(
            enabled,
            self.client_side_session()
                .map(|s| (s.peer.endpoint, s.session_id)),
        )
    }

    fn client_side_session(&self) -> Option<RoleSession> {
        let runtime = self.runtime.lock().expect("unified runtime lock");
        match runtime.role_state() {
            RoleState::Connecting(session) | RoleState::Viewing(session) => Some(session.clone()),
            RoleState::Idle | RoleState::Serving(_) => None,
        }
    }

    pub fn expire_timed_out(&self, now_ms: u64) -> Option<RoleChange> {
        let change = self
            .runtime
            .lock()
            .expect("unified runtime lock")
            .expire_timed_out(now_ms);
        if change.is_some() {
            self.side_services.stop_all();
            self.clear_client_active_session();
        }
        change
    }

    pub async fn disconnect_active(&self) -> Result<UnifiedDisconnectResult, UnifiedAppError> {
        let session = self
            .runtime
            .lock()
            .expect("unified runtime lock")
            .active_session()
            .cloned();
        let Some(session) = session else {
            self.side_services.stop_all();
            self.clear_client_active_session();
            return Ok(UnifiedDisconnectResult {
                change: None,
                target: None,
                session_id: None,
            });
        };

        if let Some(sender) = &self.client_control_sender {
            sender
                .send_control(&protocol::ControlMessage::StopStream, session.peer.endpoint)
                .await
                .map_err(|err| UnifiedAppError::ControlSend(err.to_string()))?;
        }
        self.side_services.stop_all();
        let change = self
            .runtime
            .lock()
            .expect("unified runtime lock")
            .stop_session(session.session_id)?;
        self.clear_client_active_session();

        Ok(UnifiedDisconnectResult {
            change,
            target: Some(session.peer.endpoint),
            session_id: Some(session.session_id),
        })
    }

    fn clear_client_active_session(&self) {
        if let Some(active_session_id) = &self.client_active_session_id {
            active_session_id.store(0, Relaxed);
        }
    }
}

impl Drop for UnifiedServiceOwner {
    fn drop(&mut self) {
        let _ = self.discovery_runtime.take();
    }
}

#[derive(Debug)]
pub enum UnifiedServiceOwnerError {
    MeshStore(MeshStoreError),
    MeshRuntime(EasyTierSidecarRuntimeError),
    RelayConfig(RelayConfigError),
    RelayRuntime(std::io::Error),
}

impl fmt::Display for UnifiedServiceOwnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnifiedServiceOwnerError::MeshStore(err) => {
                write!(f, "mesh config initialization failed: {err}")
            }
            UnifiedServiceOwnerError::MeshRuntime(err) => {
                write!(f, "mesh sidecar startup failed: {err}")
            }
            UnifiedServiceOwnerError::RelayConfig(err) => {
                write!(f, "relay tunnel configuration failed: {err}")
            }
            UnifiedServiceOwnerError::RelayRuntime(err) => {
                write!(f, "relay tunnel startup failed: {err}")
            }
        }
    }
}

impl Error for UnifiedServiceOwnerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            UnifiedServiceOwnerError::MeshStore(err) => Some(err),
            UnifiedServiceOwnerError::MeshRuntime(err) => Some(err),
            UnifiedServiceOwnerError::RelayConfig(err) => Some(err),
            UnifiedServiceOwnerError::RelayRuntime(err) => Some(err),
        }
    }
}

impl From<MeshStoreError> for UnifiedServiceOwnerError {
    fn from(value: MeshStoreError) -> Self {
        Self::MeshStore(value)
    }
}

impl From<EasyTierSidecarRuntimeError> for UnifiedServiceOwnerError {
    fn from(value: EasyTierSidecarRuntimeError) -> Self {
        Self::MeshRuntime(value)
    }
}

impl From<RelayConfigError> for UnifiedServiceOwnerError {
    fn from(value: RelayConfigError) -> Self {
        Self::RelayConfig(value)
    }
}

impl From<std::io::Error> for UnifiedServiceOwnerError {
    fn from(value: std::io::Error) -> Self {
        Self::RelayRuntime(value)
    }
}

struct AbortOnDropTask(JoinHandle<()>);

impl Drop for AbortOnDropTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn start_unified_relay_runtime(
    config: UnifiedRelayRuntimeConfig,
) -> Result<UnifiedRelayRuntime, UnifiedServiceOwnerError> {
    let (cancel_tx, _) = broadcast::channel(1);
    let control_tunnel = build_unified_relay_tunnel(
        config.control_bind_addr,
        &config.endpoint,
        config.control_group_id,
        &config.peer_id,
        config.host_control_target_addr,
        config.log_events,
    )
    .await?;
    let control_endpoint = control_tunnel.local_addr()?;

    let discovery_tunnel = build_unified_relay_tunnel(
        config.discovery_bind_addr,
        &config.endpoint,
        config.discovery_group_id,
        &config.peer_id,
        config.discovery_target_addr,
        config.log_events,
    )
    .await?;
    let discovery_endpoint = discovery_tunnel.local_addr()?;

    let tasks = vec![
        AbortOnDropTask(tokio::spawn({
            let cancel_rx = cancel_tx.subscribe();
            async move {
                if let Err(err) = control_tunnel.run(cancel_rx).await {
                    eprintln!("Unified relay control tunnel stopped: {err}");
                }
            }
        })),
        AbortOnDropTask(tokio::spawn({
            let cancel_rx = cancel_tx.subscribe();
            async move {
                if let Err(err) = discovery_tunnel.run(cancel_rx).await {
                    eprintln!("Unified relay discovery tunnel stopped: {err}");
                }
            }
        })),
    ];

    Ok(UnifiedRelayRuntime {
        control_endpoint,
        discovery_endpoint,
        cancel_tx,
        tasks,
    })
}

enum UnifiedRelayTunnel {
    Tcp(BoundTcpRelayTunnel),
    WebSocket(BoundWebSocketRelayTunnel),
}

impl UnifiedRelayTunnel {
    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        match self {
            Self::Tcp(tunnel) => tunnel.local_addr(),
            Self::WebSocket(tunnel) => tunnel.local_addr(),
        }
    }

    async fn run(self, cancel_rx: broadcast::Receiver<()>) -> std::io::Result<()> {
        match self {
            Self::Tcp(tunnel) => tunnel.run(cancel_rx).await,
            Self::WebSocket(tunnel) => tunnel.run(cancel_rx).await,
        }
    }
}

async fn build_unified_relay_tunnel(
    bind_addr: SocketAddr,
    endpoint: &UnifiedRelayEndpoint,
    group_id: String,
    peer_id: &str,
    local_target_addr: Option<SocketAddr>,
    log_events: bool,
) -> Result<UnifiedRelayTunnel, UnifiedServiceOwnerError> {
    match endpoint {
        UnifiedRelayEndpoint::Tcp(relay_addr) => {
            let mut config = TcpRelayTunnelConfig::new(bind_addr, *relay_addr, group_id, peer_id)?;
            if let Some(local_target_addr) = local_target_addr {
                config = config.with_local_target_addr(local_target_addr);
            }
            Ok(UnifiedRelayTunnel::Tcp(
                BoundTcpRelayTunnel::bind(config.with_event_logging(log_events)).await?,
            ))
        }
        UnifiedRelayEndpoint::WebSocket(relay_url) => {
            let mut config =
                WebSocketRelayTunnelConfig::new(bind_addr, relay_url.clone(), group_id, peer_id)?;
            if let Some(local_target_addr) = local_target_addr {
                config = config.with_local_target_addr(local_target_addr);
            }
            Ok(UnifiedRelayTunnel::WebSocket(
                BoundWebSocketRelayTunnel::bind(config.with_event_logging(log_events)).await?,
            ))
        }
    }
}

fn relay_control_group(group_id: &str) -> String {
    format!("{group_id}:control")
}

fn relay_discovery_group(group_id: &str) -> String {
    format!("{group_id}:discovery")
}

async fn start_unified_mesh_monitor(
    config: UnifiedMeshRuntimeConfig,
) -> Result<EasyTierHealthMonitorHandle, UnifiedServiceOwnerError> {
    let store = AppPrivateMeshConfigStore::new(config.mesh_dir);
    let mesh_config = store.load_or_generate(config.display_name)?;
    let mut manager = EasyTierSidecarManager::from_locator(mesh_config, &config.locator)?;
    manager.set_log_file_path(store.root_dir().join(EASYTIER_SIDECAR_LOG_FILE_NAME));
    let expected_virtual_ip = manager.config().virtual_ipv4.map(IpAddr::V4);
    let health_config = config.health_config.unwrap_or_default();
    if use_system_mesh_daemon() {
        return Ok(spawn_easytier_static_health_monitor(
            EasyTierHealthSnapshot::from_process_and_probe(
                EasyTierProcessState::Running,
                expected_virtual_ip,
            ),
        ));
    }

    manager.start().await?;
    Ok(spawn_easytier_health_monitor(
        manager,
        health_config,
        expected_virtual_ip,
    ))
}

fn use_system_mesh_daemon() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos_mesh_daemon_plist_path().is_file()
    }

    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
fn macos_mesh_daemon_plist_path() -> PathBuf {
    let label = std::env::var("REMOTE_PLAY_MESH_DAEMON_LABEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "com.remoteplay.mesh".to_string());
    PathBuf::from("/Library/LaunchDaemons").join(format!("{label}.plist"))
}

fn spawn_mesh_health_bridge(
    mut source_rx: watch::Receiver<EasyTierHealthSnapshot>,
    target_tx: watch::Sender<EasyTierHealthSnapshot>,
) {
    tokio::spawn(async move {
        let _ = target_tx.send(source_rx.borrow().clone());
        while source_rx.changed().await.is_ok() {
            let _ = target_tx.send(source_rx.borrow().clone());
        }
    });
}

fn spawn_unified_discovery_runtime(
    discovery_config: DiscoveryRuntimeConfig,
    snapshot_tx: watch::Sender<DiscoveryPeerSnapshot>,
) -> UnifiedDiscoveryRuntime {
    let announcement = discovery_config.announcement.clone();
    let route_overrides = discovery_config.route_overrides.clone();
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = broadcast::channel(1);
    let tasks = vec![
        AbortOnDropTask(tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                if let DiscoveryEvent::Error(err) = event {
                    eprintln!("Unified discovery runtime: {err}");
                }
            }
        })),
        AbortOnDropTask(tokio::spawn(async move {
            if let Err(err) =
                run_discovery_runtime(discovery_config, events_tx, snapshot_tx, cancel_rx).await
            {
                eprintln!("Unified discovery runtime stopped: {err}");
            }
        })),
    ];

    UnifiedDiscoveryRuntime {
        announcement,
        route_overrides,
        cancel_tx,
        tasks,
    }
}

fn spawn_unified_runtime_reload_task(
    runtime: Arc<Mutex<UnifiedAppRuntime>>,
    mesh_runtime: Option<ReloadableMeshRuntime>,
    discovery_runtime: Option<ReloadableDiscoveryRuntime>,
    mut reload_config: UnifiedRuntimeReloadConfig,
) -> AbortOnDropTask {
    AbortOnDropTask(tokio::spawn(async move {
        while reload_config.reload_rx.recv().await.is_some() {
            if let Err(err) = reload_unified_runtime_components(
                runtime.clone(),
                mesh_runtime.clone(),
                discovery_runtime.clone(),
                &reload_config.runtime,
            )
            .await
            {
                eprintln!("Unified runtime reload failed: {err}");
            }
        }
    }))
}

async fn reload_unified_runtime_components(
    runtime: Arc<Mutex<UnifiedAppRuntime>>,
    mesh_runtime: Option<ReloadableMeshRuntime>,
    discovery_runtime: Option<ReloadableDiscoveryRuntime>,
    config: &UnifiedRuntimeConfig,
) -> Result<(), UnifiedServiceOwnerError> {
    let virtual_ip = if let Some(mesh_state) = mesh_runtime {
        let old_monitor = mesh_state.monitor.lock().expect("mesh runtime lock").take();
        drop(old_monitor);

        let mesh_config = UnifiedMeshRuntimeConfig {
            display_name: config.display_name.clone(),
            mesh_dir: config.mesh_dir.clone(),
            locator: EasyTierBinaryLocator::from_environment(),
            health_config: None,
        };
        match start_unified_mesh_monitor(mesh_config).await {
            Ok(monitor) => {
                let snapshot = monitor.snapshot_rx.borrow().clone();
                let virtual_ip = snapshot.virtual_ip;
                let _ = mesh_state.health_tx.send(snapshot);
                spawn_mesh_health_bridge(monitor.snapshot_rx.clone(), mesh_state.health_tx.clone());
                *mesh_state.monitor.lock().expect("mesh runtime lock") = Some(monitor);
                virtual_ip
            }
            Err(err) => {
                let _ = mesh_state.health_tx.send(EasyTierHealthSnapshot::degraded(
                    EasyTierProcessState::NotStarted,
                    None,
                    format!("EasyTier mesh reload failed: {err}"),
                ));
                eprintln!("Unified EasyTier mesh reload failed: {err}");
                None
            }
        }
    } else {
        None
    };

    if let Some(discovery_state) = discovery_runtime {
        let old_discovery = discovery_state
            .runtime
            .lock()
            .expect("discovery runtime lock")
            .take();
        drop(old_discovery);

        let _ = discovery_state
            .snapshot_tx
            .send(DiscoveryPeerSnapshot::default());
        runtime
            .lock()
            .expect("unified runtime lock")
            .apply_discovery_snapshot(&DiscoveryPeerSnapshot::default());
        let discovery_config = build_unified_discovery_config(config, virtual_ip)?;
        let discovery =
            spawn_unified_discovery_runtime(discovery_config, discovery_state.snapshot_tx.clone());
        *discovery_state
            .runtime
            .lock()
            .expect("discovery runtime lock") = Some(discovery);
    }

    Ok(())
}

fn spawn_discovery_snapshot_bridge(
    runtime: Arc<Mutex<UnifiedAppRuntime>>,
    mut snapshot_rx: watch::Receiver<DiscoveryPeerSnapshot>,
) -> AbortOnDropTask {
    AbortOnDropTask(tokio::spawn(async move {
        {
            let snapshot = snapshot_rx.borrow().clone();
            runtime
                .lock()
                .expect("unified runtime lock")
                .apply_discovery_snapshot(&snapshot);
        }

        while snapshot_rx.changed().await.is_ok() {
            let snapshot = snapshot_rx.borrow().clone();
            runtime
                .lock()
                .expect("unified runtime lock")
                .apply_discovery_snapshot(&snapshot);
        }
    }))
}

fn spawn_client_session_event_bridge(
    runtime: Arc<Mutex<UnifiedAppRuntime>>,
    mut event_rx: mpsc::UnboundedReceiver<ClientSessionEvent>,
) -> AbortOnDropTask {
    AbortOnDropTask(tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let now_ms = unix_now_ms();
            let mut runtime = runtime.lock().expect("unified runtime lock");
            let _ = apply_client_session_event(&mut runtime, event, now_ms);
        }
    }))
}

fn spawn_session_timeout_monitor(
    runtime: Arc<Mutex<UnifiedAppRuntime>>,
    side_services: UnifiedSideServiceControls,
    client_active_session_id: Option<Arc<AtomicU32>>,
    config: UnifiedSessionTimeoutMonitorConfig,
) -> AbortOnDropTask {
    AbortOnDropTask(tokio::spawn(async move {
        let poll_interval = if config.poll_interval.is_zero() {
            Duration::from_millis(1)
        } else {
            config.poll_interval
        };
        loop {
            tokio::time::sleep(poll_interval).await;
            let change = {
                runtime
                    .lock()
                    .expect("unified runtime lock")
                    .expire_timed_out(unix_now_ms())
            };
            if change.is_some() {
                side_services.stop_all();
                if let Some(active_session_id) = &client_active_session_id {
                    active_session_id.store(0, Relaxed);
                }
            }
        }
    }))
}

fn spawn_viewing_keepalive(
    runtime: Arc<Mutex<UnifiedAppRuntime>>,
    sender: UdpSender,
    config: UnifiedViewingKeepaliveConfig,
) -> AbortOnDropTask {
    AbortOnDropTask(tokio::spawn(async move {
        let interval = if config.interval.is_zero() {
            Duration::from_millis(1)
        } else {
            config.interval
        };
        loop {
            tokio::time::sleep(interval).await;
            let target = {
                let runtime = runtime.lock().expect("unified runtime lock");
                match runtime.role_state() {
                    RoleState::Connecting(session) | RoleState::Viewing(session) => {
                        Some(session.peer.endpoint)
                    }
                    RoleState::Idle | RoleState::Serving(_) => None,
                }
            };
            if let Some(target) = target {
                let _ = sender
                    .send_control(&protocol::ControlMessage::Heartbeat, target)
                    .await;
                let now_ms = (std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    & 0xFFFFFFFFFFFFFFFF) as u64;
                let _ = sender
                    .send_control(&protocol::ControlMessage::Ping { client_send_ts: now_ms }, target)
                    .await;
            }
        }
    }))
}

fn spawn_client_input_sender(
    sender: UdpSender,
    mut input_rx: mpsc::UnboundedReceiver<(SocketAddr, protocol::InputEvent)>,
) -> AbortOnDropTask {
    AbortOnDropTask(tokio::spawn(async move {
        while let Some((target, event)) = input_rx.recv().await {
            let _ = sender
                .send_control(&protocol::ControlMessage::Input(event), target)
                .await;
        }
    }))
}

fn apply_client_session_event(
    runtime: &mut UnifiedAppRuntime,
    event: ClientSessionEvent,
    now_ms: u64,
) -> Option<RoleChange> {
    match event {
        ClientSessionEvent::MediaReceived { session_id, .. } => {
            apply_viewing_activity(runtime, session_id, now_ms)
        }
        ClientSessionEvent::HostTelemetry { .. } => {
            let session_id = match runtime.role_state() {
                RoleState::Connecting(session) | RoleState::Viewing(session) => session.session_id,
                RoleState::Idle | RoleState::Serving(_) => return None,
            };
            apply_viewing_activity(runtime, session_id, now_ms)
        }
    }
}

fn apply_viewing_activity(
    runtime: &mut UnifiedAppRuntime,
    session_id: u32,
    now_ms: u64,
) -> Option<RoleChange> {
    match runtime.role_state().clone() {
        RoleState::Connecting(session) if session.session_id == session_id => {
            runtime.mark_viewing_connected(session_id, now_ms).ok()
        }
        RoleState::Viewing(session) if session.session_id == session_id => {
            runtime.record_activity(session_id, now_ms).ok()
        }
        _ => None,
    }
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn app_device_from_peer(peer: &DiscoveredPeer) -> AppDevice {
    AppDevice {
        device_id: peer.announcement.device_id.clone(),
        display_name: peer.announcement.display_name.clone(),
        endpoint: peer.endpoint,
        scope: peer.scope,
        can_stream: peer.announcement.capabilities.can_stream,
        can_view: peer.announcement.capabilities.can_view,
        online: true,
        last_seen_ms: peer.last_seen_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remote_core::discovery::{
        DEFAULT_PEER_TTL, DiscoveryAnnouncement, DiscoveryCapabilities, DiscoveryPeerCache,
    };
    use remote_core::net::{MultiplexedPacket, UdpMultiplexer, UdpReceiver};
    use remote_core::role::{InboundConflictPolicy, RoleChangeReason, RoleKind};
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn runtime_with_peer(
        device_id: &str,
        can_stream: bool,
        control_port: u16,
    ) -> UnifiedAppRuntime {
        let mut runtime = UnifiedAppRuntime::default();
        let mut cache = DiscoveryPeerCache::new("test-net", "local-device");
        cache.apply_announcement(
            DiscoveryAnnouncement {
                network_name: "test-net".to_string(),
                device_id: device_id.to_string(),
                display_name: format!("Device {device_id}"),
                control_port,
                virtual_ip: None,
                capabilities: DiscoveryCapabilities {
                    can_stream,
                    can_view: true,
                    file_transfer: true,
                    clipboard_sync: true,
                    talkback: true,
                },
                scope: DiscoveryScope::Lan,
                ttl: DEFAULT_PEER_TTL,
            },
            SocketAddr::from(([127, 0, 0, 1], 38117)),
            100,
        );
        runtime.apply_discovery_snapshot(&cache.snapshot());
        runtime
    }

    fn streamable_peer_snapshot(endpoint: SocketAddr) -> DiscoveryPeerSnapshot {
        let mut cache = DiscoveryPeerCache::new("test-net", "local-device");
        cache.apply_announcement(
            DiscoveryAnnouncement {
                network_name: "test-net".to_string(),
                device_id: "peer-a".to_string(),
                display_name: "Peer A".to_string(),
                control_port: endpoint.port(),
                virtual_ip: None,
                capabilities: DiscoveryCapabilities {
                    can_stream: true,
                    can_view: true,
                    ..DiscoveryCapabilities::default()
                },
                scope: DiscoveryScope::Lan,
                ttl: DEFAULT_PEER_TTL,
            },
            SocketAddr::new(endpoint.ip(), 38117),
            100,
        );
        cache.snapshot()
    }

    async fn owner_with_control_peer() -> (UnifiedServiceOwner, UdpReceiver, SocketAddr) {
        let control = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (control_sender, _control_rx) = control.split();
        let target = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let (_target_sender, target_rx) = target.split();
        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig {
            client_control_sender: Some(control_sender),
            ..UnifiedServiceOwnerConfig::default()
        })
        .await
        .unwrap();
        owner
            .runtime()
            .lock()
            .expect("runtime lock")
            .apply_discovery_snapshot(&streamable_peer_snapshot(target_addr));
        (owner, target_rx, target_addr)
    }

    async fn owner_with_control_peer_and_session_events() -> (
        UnifiedServiceOwner,
        UdpReceiver,
        mpsc::UnboundedSender<ClientSessionEvent>,
    ) {
        let control = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (control_sender, _control_rx) = control.split();
        let target = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let (_target_sender, target_rx) = target.split();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig {
            client_control_sender: Some(control_sender),
            client_session_event_rx: Some(event_rx),
            ..UnifiedServiceOwnerConfig::default()
        })
        .await
        .unwrap();
        owner
            .runtime()
            .lock()
            .expect("runtime lock")
            .apply_discovery_snapshot(&streamable_peer_snapshot(target_addr));
        (owner, target_rx, event_tx)
    }

    async fn wait_for_owner_role(owner: &UnifiedServiceOwner, kind: RoleKind) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let current = {
                    owner
                        .runtime()
                        .lock()
                        .expect("runtime lock")
                        .role_state()
                        .kind()
                };
                if current == kind {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("role state should update");
    }

    fn temp_mesh_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("remote-play-{name}-{unique}"))
    }

    #[test]
    fn app_defaults_enable_the_managed_public_relay_fallback() {
        let config = UnifiedRuntimeConfig::app_defaults();

        assert!(config.relay_endpoint.is_some());
        assert_eq!(
            config.relay_endpoint,
            Some(UnifiedRelayEndpoint::WebSocket(
                DEFAULT_RELAY_SERVER_URL.to_string()
            ))
        );
    }

    #[test]
    fn relay_runtime_config_uses_persistent_runtime_settings_without_process_env() {
        let mesh_dir = temp_mesh_dir("default-relay");
        let config = UnifiedRuntimeConfig {
            display_name: "Default Relay".to_string(),
            mesh_dir: mesh_dir.clone(),
            host_bind_addr: SocketAddr::from(([127, 0, 0, 1], 8123)),
            client_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            discovery_port: 39118,
            enable_mesh: false,
            relay_endpoint: Some(UnifiedRelayEndpoint::WebSocket(
                DEFAULT_RELAY_SERVER_URL.to_string(),
            )),
            relay_control_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_discovery_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_log_events: false,
            enable_discovery: true,
            enable_passive_host: true,
            enable_client_receiver: true,
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
            enable_viewer_media: false,
            enable_session_timeout_monitor: false,
        };

        let relay = build_unified_relay_config(&config)
            .expect("relay config")
            .expect("relay should be enabled");

        assert_eq!(relay.endpoint, config.relay_endpoint.unwrap());
        assert_eq!(
            relay.host_control_target_addr,
            Some(SocketAddr::from(([127, 0, 0, 1], 8123)))
        );
        assert_eq!(
            relay.discovery_target_addr,
            Some(SocketAddr::from(([127, 0, 0, 1], 39118)))
        );

        let _ = std::fs::remove_dir_all(mesh_dir);
    }

    #[test]
    fn unified_discovery_config_advertises_dual_role_capabilities() {
        let mesh_dir = temp_mesh_dir("dual-role-discovery");
        let config = UnifiedRuntimeConfig {
            display_name: "Unified Test".to_string(),
            mesh_dir: mesh_dir.clone(),
            host_bind_addr: SocketAddr::from(([127, 0, 0, 1], 8123)),
            client_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            discovery_port: 39117,
            enable_mesh: false,
            relay_endpoint: None,
            relay_control_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_discovery_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_log_events: false,
            enable_discovery: true,
            enable_passive_host: true,
            enable_client_receiver: true,
            enable_clipboard_sync: true,
            enable_file_transfer: true,
            enable_talkback: true,
            enable_viewer_media: true,
            enable_session_timeout_monitor: true,
        };

        let discovery =
            build_unified_discovery_config(&config, Some("10.77.0.8".parse().unwrap())).unwrap();

        assert_eq!(discovery.bind_addr, SocketAddr::from(([0, 0, 0, 0], 39117)));
        assert_eq!(discovery.announcement.display_name, "Unified Test");
        assert_eq!(discovery.announcement.control_port, 8123);
        assert_eq!(
            discovery.announcement.virtual_ip,
            Some("10.77.0.8".parse().unwrap())
        );
        assert_eq!(discovery.announcement.scope, DiscoveryScope::Mesh);
        assert!(discovery.announcement.capabilities.can_stream);
        assert!(discovery.announcement.capabilities.can_view);
        assert!(discovery.announcement.capabilities.clipboard_sync);
        assert!(discovery.announcement.capabilities.file_transfer);
        assert!(discovery.announcement.capabilities.talkback);

        let _ = std::fs::remove_dir_all(mesh_dir);
    }

    #[tokio::test]
    async fn unified_runtime_starts_core_services_with_ephemeral_ports() {
        let mesh_dir = temp_mesh_dir("runtime-core");
        let runtime = start_unified_runtime(UnifiedRuntimeConfig {
            display_name: "Unified Runtime Test".to_string(),
            mesh_dir: mesh_dir.clone(),
            host_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            client_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            discovery_port: 0,
            enable_mesh: false,
            relay_endpoint: None,
            relay_control_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_discovery_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_log_events: false,
            enable_discovery: false,
            enable_passive_host: true,
            enable_client_receiver: true,
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
            enable_viewer_media: false,
            enable_session_timeout_monitor: true,
        })
        .await
        .unwrap();

        assert!(!runtime.owner.owns_mesh());
        assert!(!runtime.owner.owns_discovery());
        assert!(runtime.host_stats.is_some());
        assert_eq!(
            runtime.viewer_media_status,
            UnifiedViewerMediaStatus::Disabled
        );
        assert!(runtime.mesh_pairing.is_some());
        assert_eq!(
            runtime
                .mesh_pairing
                .as_ref()
                .expect("pairing control")
                .snapshot()
                .display_name,
            "Unified Runtime Test"
        );
        assert_eq!(runtime.owner.task_count(), 6);
        assert_eq!(runtime.background_task_count(), 8);

        let _ = std::fs::remove_dir_all(mesh_dir);
    }

    #[tokio::test]
    async fn viewer_media_start_failure_falls_back_to_sink_channels() {
        let stats = Statistics::new();
        let mut media_sink_tasks = Vec::new();

        let channels = start_viewer_media_channels(true, stats, &mut media_sink_tasks, |_stats| {
            Err(std::io::Error::other("media denied").into())
        });

        assert!(channels.viewer_frame.is_none());
        assert!(channels.viewer_media.is_none());
        assert_eq!(
            channels.status,
            UnifiedViewerMediaStatus::Unavailable {
                reason: "media denied".to_string()
            }
        );
        assert_eq!(media_sink_tasks.len(), 2);
    }

    #[tokio::test]
    async fn runtime_reload_rebuilds_discovery_from_saved_device_group() {
        let mesh_dir = temp_mesh_dir("reload-discovery");
        let config = UnifiedRuntimeConfig {
            display_name: "Reload Device".to_string(),
            mesh_dir: mesh_dir.clone(),
            host_bind_addr: SocketAddr::from(([127, 0, 0, 1], 8123)),
            client_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            discovery_port: 39119,
            enable_mesh: false,
            relay_endpoint: None,
            relay_control_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_discovery_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_log_events: false,
            enable_discovery: true,
            enable_passive_host: true,
            enable_client_receiver: true,
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
            enable_viewer_media: false,
            enable_session_timeout_monitor: false,
        };
        let (snapshot_tx, _snapshot_rx) = watch::channel(DiscoveryPeerSnapshot::default());
        let discovery_state = ReloadableDiscoveryRuntime {
            runtime: Arc::new(Mutex::new(None)),
            snapshot_tx,
        };
        let pairing =
            MeshPairingControl::load_or_create(&mesh_dir, "Reload Device").expect("pairing");
        let paired = pairing.create_new_group();

        reload_unified_runtime_components(
            Arc::new(Mutex::new(UnifiedAppRuntime::default())),
            None,
            Some(discovery_state.clone()),
            &config,
        )
        .await
        .unwrap();

        let guard = discovery_state
            .runtime
            .lock()
            .expect("discovery runtime lock");
        let discovery = guard.as_ref().expect("reloaded discovery runtime");
        assert_eq!(discovery.announcement.network_name, paired.network_name);
        assert_eq!(discovery.announcement.device_id, paired.device_id);
        assert_eq!(discovery.announcement.display_name, "Reload Device");
        assert_eq!(discovery.announcement.control_port, 8123);

        let _ = std::fs::remove_dir_all(mesh_dir);
    }

    #[tokio::test]
    async fn unified_pairing_actions_request_runtime_reload() {
        let mesh_dir = temp_mesh_dir("runtime-pairing-reload");
        let runtime = start_unified_runtime(UnifiedRuntimeConfig {
            display_name: "Pairing Reload".to_string(),
            mesh_dir: mesh_dir.clone(),
            host_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            client_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            discovery_port: 39120,
            enable_mesh: false,
            relay_endpoint: None,
            relay_control_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_discovery_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_log_events: false,
            enable_discovery: true,
            enable_passive_host: false,
            enable_client_receiver: false,
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
            enable_viewer_media: false,
            enable_session_timeout_monitor: false,
        })
        .await
        .unwrap();
        let pairing = runtime
            .mesh_pairing
            .as_ref()
            .expect("pairing control")
            .clone();
        let paired = pairing.create_new_group();

        assert!(!paired.restart_required);
        assert!(paired.message.contains("Network services are refreshing"));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let matches_pairing = runtime
                    .owner
                    .discovery_runtime
                    .as_ref()
                    .and_then(|state| {
                        state
                            .runtime
                            .lock()
                            .expect("discovery runtime lock")
                            .as_ref()
                            .map(|discovery| {
                                discovery.announcement.network_name == paired.network_name
                                    && discovery.announcement.device_id == paired.device_id
                            })
                    })
                    .unwrap_or(false);
                if matches_pairing {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pairing action should trigger runtime reload");

        let _ = std::fs::remove_dir_all(mesh_dir);
    }

    #[test]
    fn discovery_snapshot_updates_streamable_devices() {
        let mut runtime = UnifiedAppRuntime::default();
        let mut cache = DiscoveryPeerCache::new("test-net", "local-device");
        cache.apply_announcement(
            DiscoveryAnnouncement {
                network_name: "test-net".to_string(),
                device_id: "streamer".to_string(),
                display_name: "Streamer".to_string(),
                control_port: DEFAULT_CONTROL_PORT,
                virtual_ip: Some("10.0.0.8".parse().unwrap()),
                capabilities: DiscoveryCapabilities {
                    can_stream: true,
                    can_view: true,
                    ..DiscoveryCapabilities::default()
                },
                scope: DiscoveryScope::Mesh,
                ttl: DEFAULT_PEER_TTL,
            },
            SocketAddr::from(([192, 168, 1, 9], 38117)),
            100,
        );
        cache.apply_announcement(
            DiscoveryAnnouncement {
                network_name: "test-net".to_string(),
                device_id: "viewer-only".to_string(),
                display_name: "Viewer".to_string(),
                control_port: 0,
                virtual_ip: None,
                capabilities: DiscoveryCapabilities {
                    can_view: true,
                    ..DiscoveryCapabilities::default()
                },
                scope: DiscoveryScope::Lan,
                ttl: DEFAULT_PEER_TTL,
            },
            SocketAddr::from(([192, 168, 1, 10], 38117)),
            100,
        );

        runtime.apply_discovery_snapshot(&cache.snapshot());

        let devices = runtime.devices();
        assert_eq!(devices.len(), 2);
        let streamable = runtime.streamable_devices();
        assert_eq!(streamable.len(), 1);
        assert_eq!(streamable[0].device_id, "streamer");
        assert_eq!(
            streamable[0].endpoint,
            SocketAddr::from(([10, 0, 0, 8], DEFAULT_CONTROL_PORT))
        );
        assert_eq!(streamable[0].scope, DiscoveryScope::Mesh);
    }

    #[test]
    fn discovery_snapshot_prefers_direct_route_over_relay_for_same_device() {
        let mut runtime = UnifiedAppRuntime::default();
        let mut cache = DiscoveryPeerCache::new("test-net", "local-device");
        cache
            .apply_announcement(
                DiscoveryAnnouncement {
                    network_name: "test-net".to_string(),
                    device_id: "peer-a".to_string(),
                    display_name: "Peer A".to_string(),
                    control_port: DEFAULT_CONTROL_PORT,
                    virtual_ip: None,
                    capabilities: DiscoveryCapabilities {
                        can_stream: true,
                        can_view: true,
                        ..DiscoveryCapabilities::default()
                    },
                    scope: DiscoveryScope::Lan,
                    ttl: DEFAULT_PEER_TTL,
                },
                SocketAddr::from(([192, 168, 1, 50], 38117)),
                100,
            )
            .expect("direct route");
        cache
            .apply_announcement_with_route_override(
                DiscoveryAnnouncement {
                    network_name: "test-net".to_string(),
                    device_id: "peer-a".to_string(),
                    display_name: "Peer A".to_string(),
                    control_port: DEFAULT_CONTROL_PORT,
                    virtual_ip: None,
                    capabilities: DiscoveryCapabilities {
                        can_stream: true,
                        can_view: true,
                        ..DiscoveryCapabilities::default()
                    },
                    scope: DiscoveryScope::Lan,
                    ttl: DEFAULT_PEER_TTL,
                },
                SocketAddr::from(([127, 0, 0, 1], 48117)),
                120,
                remote_core::discovery::DiscoveryRouteOverride {
                    source: SocketAddr::from(([127, 0, 0, 1], 48117)),
                    endpoint: SocketAddr::from(([127, 0, 0, 1], 49171)),
                    scope: DiscoveryScope::Relay,
                },
            )
            .expect("relay route");

        runtime.apply_discovery_snapshot(&cache.snapshot());

        let devices = runtime.devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(
            devices[0].endpoint,
            SocketAddr::from(([192, 168, 1, 50], DEFAULT_CONTROL_PORT))
        );
        assert_eq!(devices[0].scope, DiscoveryScope::Lan);
    }

    #[test]
    fn discovery_snapshot_uses_relay_route_when_it_is_the_only_candidate() {
        let mut runtime = UnifiedAppRuntime::default();
        let mut cache = DiscoveryPeerCache::new("test-net", "local-device");
        let relay_endpoint = SocketAddr::from(([127, 0, 0, 1], 49171));
        cache
            .apply_announcement_with_route_override(
                DiscoveryAnnouncement {
                    network_name: "test-net".to_string(),
                    device_id: "peer-a".to_string(),
                    display_name: "Peer A".to_string(),
                    control_port: DEFAULT_CONTROL_PORT,
                    virtual_ip: None,
                    capabilities: DiscoveryCapabilities {
                        can_stream: true,
                        can_view: true,
                        ..DiscoveryCapabilities::default()
                    },
                    scope: DiscoveryScope::Lan,
                    ttl: DEFAULT_PEER_TTL,
                },
                SocketAddr::from(([127, 0, 0, 1], 48117)),
                100,
                remote_core::discovery::DiscoveryRouteOverride {
                    source: SocketAddr::from(([127, 0, 0, 1], 48117)),
                    endpoint: relay_endpoint,
                    scope: DiscoveryScope::Relay,
                },
            )
            .expect("relay route");

        runtime.apply_discovery_snapshot(&cache.snapshot());

        let devices = runtime.streamable_devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].endpoint, relay_endpoint);
        assert_eq!(devices[0].scope, DiscoveryScope::Relay);
    }

    #[test]
    fn discovery_snapshot_marks_missing_devices_offline() {
        let mut runtime = runtime_with_peer("peer-a", true, DEFAULT_CONTROL_PORT);
        let empty_cache = DiscoveryPeerCache::new("test-net", "local-device");

        runtime.apply_discovery_snapshot(&empty_cache.snapshot());

        let devices = runtime.devices();
        assert_eq!(devices.len(), 1);
        assert!(!devices[0].online);
        assert!(runtime.streamable_devices().is_empty());
    }

    #[test]
    fn connect_device_starts_viewing_with_allocated_session() {
        let mut runtime = runtime_with_peer("peer-a", true, DEFAULT_CONTROL_PORT);

        let request = runtime.connect_device("peer-a", 200).unwrap();

        assert_eq!(request.session_id, 1);
        assert_eq!(
            request.target,
            SocketAddr::from(([127, 0, 0, 1], DEFAULT_CONTROL_PORT))
        );
        assert_eq!(request.change.current.kind(), RoleKind::Connecting);
        assert_eq!(runtime.role_state().kind(), RoleKind::Connecting);
    }

    #[test]
    fn connect_device_rejects_missing_or_non_streamable_devices() {
        let mut runtime = runtime_with_peer("viewer-only", false, 0);

        assert_eq!(
            runtime.connect_device("missing", 200).unwrap_err(),
            UnifiedAppError::DeviceNotFound("missing".to_string())
        );
        assert_eq!(
            runtime.connect_device("viewer-only", 200).unwrap_err(),
            UnifiedAppError::DeviceNotStreamable("viewer-only".to_string())
        );
    }

    #[test]
    fn same_device_reconnect_allocates_new_session() {
        let mut runtime = runtime_with_peer("peer-a", true, DEFAULT_CONTROL_PORT);

        let first = runtime.connect_device("peer-a", 200).unwrap();
        runtime
            .mark_viewing_connected(first.session_id, 220)
            .unwrap();
        let second = runtime.connect_device("peer-a", 300).unwrap();

        assert_eq!(first.session_id, 1);
        assert_eq!(second.session_id, 2);
        assert_eq!(second.change.previous.kind(), RoleKind::Viewing);
        assert_eq!(second.change.current.kind(), RoleKind::Connecting);
    }

    #[test]
    fn role_conflict_blocks_connect_while_serving() {
        let mut runtime = runtime_with_peer("peer-a", true, DEFAULT_CONTROL_PORT);
        runtime
            .accept_inbound_stream(
                RolePeer::new("viewer", "Viewer", SocketAddr::from(([127, 0, 0, 1], 9000))),
                77,
                200,
            )
            .unwrap();

        let err = runtime.connect_device("peer-a", 300).unwrap_err();

        assert!(matches!(
            err,
            UnifiedAppError::Role(RoleStateError::Busy { .. })
        ));
        assert_eq!(runtime.role_state().kind(), RoleKind::Serving);
    }

    #[test]
    fn explicit_inbound_policy_can_take_over_active_viewing() {
        let mut runtime = UnifiedAppRuntime::new(UnifiedAppConfig {
            role_config: RoleStateMachineConfig {
                inbound_conflict_policy: InboundConflictPolicy::StopActiveAndServe,
                ..RoleStateMachineConfig::default()
            },
            first_session_id: 9,
        });
        let mut peer_runtime = runtime_with_peer("peer-a", true, DEFAULT_CONTROL_PORT);
        runtime.apply_discovery_snapshot(&peer_runtime.devices_to_snapshot_like_cache());

        let request = runtime.connect_device("peer-a", 200).unwrap();
        runtime
            .mark_viewing_connected(request.session_id, 220)
            .unwrap();

        let change = runtime
            .accept_inbound_stream(
                RolePeer::new("viewer", "Viewer", SocketAddr::from(([127, 0, 0, 1], 9000))),
                77,
                300,
            )
            .unwrap();

        assert_eq!(change.previous.kind(), RoleKind::Viewing);
        assert_eq!(change.current.kind(), RoleKind::Serving);
    }

    #[test]
    fn timeout_delegates_to_role_state_machine() {
        let mut runtime = UnifiedAppRuntime::new(UnifiedAppConfig {
            role_config: RoleStateMachineConfig {
                session_timeout_ms: 50,
                ..RoleStateMachineConfig::default()
            },
            first_session_id: 1,
        });
        runtime
            .accept_inbound_stream(
                RolePeer::new("viewer", "Viewer", SocketAddr::from(([127, 0, 0, 1], 9000))),
                77,
                200,
            )
            .unwrap();
        runtime.record_activity(77, 240).unwrap();

        assert!(runtime.expire_timed_out(290).is_none());
        let timeout = runtime.expire_timed_out(291).unwrap();

        assert_eq!(timeout.previous.kind(), RoleKind::Serving);
        assert_eq!(timeout.current, RoleState::Idle);
    }

    #[tokio::test]
    async fn service_owner_can_start_without_services() {
        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig::default())
            .await
            .unwrap();

        assert!(!owner.owns_mesh());
        assert!(!owner.owns_discovery());
        assert_eq!(owner.task_count(), 0);
        assert_eq!(
            owner.runtime().lock().expect("runtime lock").role_state(),
            &RoleState::Idle
        );
    }

    #[tokio::test]
    async fn service_owner_starts_discovery_runtime() {
        let discovery = DiscoveryRuntimeConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            announce_targets: Vec::new(),
            route_overrides: Vec::new(),
            announcement: DiscoveryAnnouncement {
                network_name: "test-net".to_string(),
                device_id: "local-device".to_string(),
                display_name: "Local".to_string(),
                control_port: DEFAULT_CONTROL_PORT,
                virtual_ip: None,
                capabilities: DiscoveryCapabilities {
                    can_stream: true,
                    can_view: true,
                    ..DiscoveryCapabilities::default()
                },
                scope: DiscoveryScope::Lan,
                ttl: DEFAULT_PEER_TTL,
            },
            announce_interval: Duration::from_millis(50),
            prune_interval: Duration::from_millis(50),
        };

        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig {
            discovery: Some(discovery),
            ..UnifiedServiceOwnerConfig::default()
        })
        .await
        .unwrap();

        assert!(owner.owns_discovery());
        assert!(owner.discovery_snapshot_rx().is_some());
        assert_eq!(owner.task_count(), 3);
    }

    #[tokio::test]
    async fn service_owner_starts_relay_and_wires_discovery_route_override() {
        let (relay_cancel_tx, _) = broadcast::channel(1);
        let relay_server = remote_core::relay::BoundTcpRelayServer::bind(
            remote_core::relay::TcpRelayServerConfig::new("127.0.0.1:0".parse().unwrap()),
        )
        .await
        .unwrap();
        let relay_addr = relay_server.local_addr().unwrap();
        let relay_task = tokio::spawn(relay_server.run(relay_cancel_tx.subscribe()));
        let discovery = DiscoveryRuntimeConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            announce_targets: Vec::new(),
            route_overrides: Vec::new(),
            announcement: DiscoveryAnnouncement {
                network_name: "test-net".to_string(),
                device_id: "local-device".to_string(),
                display_name: "Local".to_string(),
                control_port: DEFAULT_CONTROL_PORT,
                virtual_ip: None,
                capabilities: DiscoveryCapabilities {
                    can_stream: true,
                    can_view: true,
                    ..DiscoveryCapabilities::default()
                },
                scope: DiscoveryScope::Lan,
                ttl: DEFAULT_PEER_TTL,
            },
            announce_interval: Duration::from_millis(50),
            prune_interval: Duration::from_millis(50),
        };

        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig {
            relay: Some(UnifiedRelayRuntimeConfig {
                endpoint: UnifiedRelayEndpoint::Tcp(relay_addr),
                control_group_id: relay_control_group("test-net"),
                discovery_group_id: relay_discovery_group("test-net"),
                peer_id: "local-device".to_string(),
                control_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
                discovery_bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
                host_control_target_addr: Some(SocketAddr::from((
                    [127, 0, 0, 1],
                    DEFAULT_CONTROL_PORT,
                ))),
                discovery_target_addr: Some(SocketAddr::from(([127, 0, 0, 1], 38117))),
                log_events: false,
            }),
            discovery: Some(discovery),
            ..UnifiedServiceOwnerConfig::default()
        })
        .await
        .unwrap();

        let relay_runtime = owner.relay_runtime.as_ref().expect("relay runtime");
        let discovery_runtime = owner
            .discovery_runtime
            .as_ref()
            .and_then(|state| {
                state
                    .runtime
                    .lock()
                    .expect("discovery runtime lock")
                    .as_ref()
                    .map(|runtime| {
                        (
                            runtime.route_overrides.clone(),
                            runtime.announcement.clone(),
                        )
                    })
            })
            .expect("discovery runtime");
        assert!(owner.owns_relay());
        assert!(owner.owns_discovery());
        assert_eq!(owner.task_count(), 5);
        assert_eq!(
            discovery_runtime.0,
            vec![DiscoveryRouteOverride {
                source: relay_runtime.discovery_endpoint,
                endpoint: relay_runtime.control_endpoint,
                scope: DiscoveryScope::Relay,
            }]
        );
        assert_eq!(discovery_runtime.1.scope, DiscoveryScope::Lan);

        drop(owner);
        let _ = relay_cancel_tx.send(());
        relay_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn service_owner_starts_websocket_relay_and_wires_discovery_route_override() {
        let (relay_cancel_tx, _) = broadcast::channel(1);
        let relay_server = remote_core::relay::BoundTcpRelayServer::bind(
            remote_core::relay::TcpRelayServerConfig::new("127.0.0.1:0".parse().unwrap()),
        )
        .await
        .unwrap();
        let relay_addr = relay_server.local_addr().unwrap();
        let relay_task = tokio::spawn(relay_server.run_websocket(relay_cancel_tx.subscribe()));
        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig {
            relay: Some(
                UnifiedRelayRuntimeConfig::from_mesh_secret(
                    UnifiedRelayEndpoint::WebSocket(format!("ws://{relay_addr}/relay")),
                    "test-net",
                    "test-secret",
                    "local-device",
                )
                .unwrap(),
            ),
            ..UnifiedServiceOwnerConfig::default()
        })
        .await
        .unwrap();

        assert!(owner.owns_relay());
        assert_eq!(owner.relay_runtime.as_ref().unwrap().task_count(), 2);

        drop(owner);
        let _ = relay_cancel_tx.send(());
        relay_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn discovery_bridge_updates_owned_runtime_devices() {
        let runtime = Arc::new(Mutex::new(UnifiedAppRuntime::default()));
        let (snapshot_tx, snapshot_rx) = watch::channel(DiscoveryPeerSnapshot::default());
        let _bridge = spawn_discovery_snapshot_bridge(runtime.clone(), snapshot_rx);
        let mut cache = DiscoveryPeerCache::new("test-net", "local-device");
        cache.apply_announcement(
            DiscoveryAnnouncement {
                network_name: "test-net".to_string(),
                device_id: "peer-a".to_string(),
                display_name: "Peer A".to_string(),
                control_port: DEFAULT_CONTROL_PORT,
                virtual_ip: None,
                capabilities: DiscoveryCapabilities {
                    can_stream: true,
                    can_view: true,
                    ..DiscoveryCapabilities::default()
                },
                scope: DiscoveryScope::Lan,
                ttl: DEFAULT_PEER_TTL,
            },
            SocketAddr::from(([127, 0, 0, 1], 38117)),
            100,
        );

        snapshot_tx.send(cache.snapshot()).unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let devices = { runtime.lock().expect("runtime lock").streamable_devices() };
                if devices.len() == 1 && devices[0].device_id == "peer-a" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("bridge should update runtime");
    }

    #[tokio::test]
    async fn owner_connect_device_sends_start_stream_and_waits_for_connected_mark() {
        let (owner, target_rx, target_addr) = owner_with_control_peer().await;

        let request = owner
            .connect_device(
                "peer-a",
                StreamStartOptions {
                    width: 1280,
                    height: 720,
                    fps: 30,
                    bitrate_kbps: 4_000,
                },
                200,
            )
            .await
            .unwrap();

        assert_eq!(request.session_id, 1);
        assert_eq!(request.target, target_addr);
        assert_eq!(
            owner
                .runtime()
                .lock()
                .expect("runtime lock")
                .role_state()
                .kind(),
            RoleKind::Connecting
        );
        match tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            MultiplexedPacket::Control(
                protocol::ControlMessage::StartStream {
                    width,
                    height,
                    fps,
                    bitrate_kbps,
                    session_id,
                },
                _,
            ) => {
                assert_eq!(width, 1280);
                assert_eq!(height, 720);
                assert_eq!(fps, 30);
                assert_eq!(bitrate_kbps, 4_000);
                assert_eq!(session_id, request.session_id);
            }
            other => panic!("expected StartStream, got {other:?}"),
        }

        owner
            .mark_viewing_connected(request.session_id, 240)
            .unwrap();
        assert_eq!(
            owner
                .runtime()
                .lock()
                .expect("runtime lock")
                .role_state()
                .kind(),
            RoleKind::Viewing
        );
    }

    #[tokio::test]
    async fn owner_keeps_an_idle_viewing_session_alive_without_user_input() {
        let control = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (control_sender, _control_rx) = control.split();
        let target = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let (_target_sender, target_rx) = target.split();
        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig {
            client_control_sender: Some(control_sender),
            viewing_keepalive: Some(UnifiedViewingKeepaliveConfig {
                interval: Duration::from_millis(20),
            }),
            ..UnifiedServiceOwnerConfig::default()
        })
        .await
        .unwrap();
        owner
            .runtime()
            .lock()
            .expect("runtime lock")
            .apply_discovery_snapshot(&streamable_peer_snapshot(target_addr));

        let request = owner
            .connect_device("peer-a", StreamStartOptions::default(), 200)
            .await
            .unwrap();
        let _start_stream = tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .unwrap()
            .unwrap();

        match tokio::time::timeout(Duration::from_millis(200), target_rx.recv())
            .await
            .expect("connecting heartbeat should be periodic")
            .expect("target receiver should remain open")
        {
            MultiplexedPacket::Control(protocol::ControlMessage::Heartbeat | protocol::ControlMessage::Ping { .. }, _) => {}
            other => panic!("expected Heartbeat or Ping while connecting, got {other:?}"),
        }

        owner
            .mark_viewing_connected(request.session_id, 240)
            .expect("session should transition to viewing");
        match tokio::time::timeout(Duration::from_millis(200), target_rx.recv())
            .await
            .expect("viewing heartbeat should be periodic")
            .expect("target receiver should remain open")
        {
            MultiplexedPacket::Control(protocol::ControlMessage::Heartbeat | protocol::ControlMessage::Ping { .. }, _) => {}
            other => panic!("expected Heartbeat or Ping while viewing, got {other:?}"),
        }

        owner
            .runtime()
            .lock()
            .expect("runtime lock")
            .stop_session(request.session_id)
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(80), target_rx.recv())
                .await
                .is_err(),
            "idle owners must stop sending viewing heartbeats"
        );
    }

    #[tokio::test]
    async fn owner_routes_input_only_while_actively_viewing() {
        let control = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (control_sender, _control_rx) = control.split();
        let target = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let (_target_sender, target_rx) = target.split();
        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig {
            client_control_sender: Some(control_sender),
            ..UnifiedServiceOwnerConfig::default()
        })
        .await
        .unwrap();
        owner
            .runtime()
            .lock()
            .expect("runtime lock")
            .apply_discovery_snapshot(&streamable_peer_snapshot(target_addr));

        assert!(!owner.queue_viewing_input(protocol::InputEvent::MouseDown(0)));
        let request = owner
            .connect_device("peer-a", StreamStartOptions::default(), 200)
            .await
            .unwrap();
        let _start_stream = tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!owner.queue_viewing_input(protocol::InputEvent::MouseDown(0)));

        owner
            .mark_viewing_connected(request.session_id, 240)
            .expect("session should transition to viewing");
        assert!(owner.queue_viewing_input(protocol::InputEvent::MouseMove { dx: 12, dy: -4 }));
        match tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .expect("viewing input should arrive")
            .expect("target receiver should remain open")
        {
            MultiplexedPacket::Control(
                protocol::ControlMessage::Input(protocol::InputEvent::MouseMove { dx, dy }),
                _,
            ) => {
                assert_eq!((dx, dy), (12, -4));
            }
            other => panic!("expected mouse input while viewing, got {other:?}"),
        }

        owner.disconnect_active().await.unwrap();
        let _stop_stream = tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!owner.queue_viewing_input(protocol::InputEvent::MouseUp(0)));
    }

    #[tokio::test]
    async fn owner_exposes_and_persists_side_service_preferences() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _receiver) = mux.split();
        let mut runtime_config = UnifiedRuntimeConfig::app_defaults();
        runtime_config.enable_clipboard_sync = true;
        runtime_config.enable_file_transfer = true;
        runtime_config.enable_talkback = true;
        let side_services = build_client_side_services(&runtime_config, sender);
        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig {
            side_services,
            ..UnifiedServiceOwnerConfig::default()
        })
        .await
        .unwrap();

        assert!(owner.supports_clipboard_sync());
        assert!(owner.supports_file_transfer());
        assert!(owner.supports_talkback());
        assert!(owner.clipboard_sync_enabled());
        assert!(owner.file_transfer_enabled());
        assert!(owner.talkback_enabled());

        assert!(owner.set_clipboard_sync_enabled(false));
        assert!(owner.set_file_transfer_enabled(false));
        assert!(owner.set_talkback_enabled(false));
        assert!(!owner.clipboard_sync_enabled());
        assert!(!owner.file_transfer_enabled());
        assert!(!owner.talkback_enabled());
    }

    #[tokio::test]
    async fn unavailable_side_services_cannot_be_enabled() {
        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig::default())
            .await
            .unwrap();

        assert!(!owner.supports_clipboard_sync());
        assert!(!owner.set_clipboard_sync_enabled(true));
        assert!(!owner.clipboard_sync_enabled());
    }

    #[tokio::test]
    async fn owner_connect_without_control_sender_does_not_change_role() {
        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig::default())
            .await
            .unwrap();
        let target_addr = SocketAddr::from(([127, 0, 0, 1], DEFAULT_CONTROL_PORT));
        owner
            .runtime()
            .lock()
            .expect("runtime lock")
            .apply_discovery_snapshot(&streamable_peer_snapshot(target_addr));

        let err = owner
            .connect_device("peer-a", StreamStartOptions::default(), 200)
            .await
            .unwrap_err();

        assert_eq!(err, UnifiedAppError::NoClientControlSender);
        assert_eq!(
            owner.runtime().lock().expect("runtime lock").role_state(),
            &RoleState::Idle
        );
    }

    #[tokio::test]
    async fn owner_disconnect_active_sends_stop_stream_and_returns_idle() {
        let (owner, target_rx, _) = owner_with_control_peer().await;
        let request = owner
            .connect_device("peer-a", StreamStartOptions::default(), 200)
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .unwrap()
            .unwrap();
        owner
            .mark_viewing_connected(request.session_id, 240)
            .unwrap();

        let result = owner.disconnect_active().await.unwrap();

        assert_eq!(result.session_id, Some(request.session_id));
        assert!(result.change.is_some());
        match tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            MultiplexedPacket::Control(protocol::ControlMessage::StopStream, _) => {}
            other => panic!("expected StopStream, got {other:?}"),
        }
        assert_eq!(
            owner.runtime().lock().expect("runtime lock").role_state(),
            &RoleState::Idle
        );
    }

    #[tokio::test]
    async fn owner_expire_timed_out_returns_active_session_to_idle() {
        let control = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (control_sender, _control_rx) = control.split();
        let target = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let (_target_sender, target_rx) = target.split();
        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig {
            app: UnifiedAppConfig {
                role_config: RoleStateMachineConfig {
                    connecting_timeout_ms: 50,
                    ..RoleStateMachineConfig::default()
                },
                first_session_id: 1,
            },
            client_control_sender: Some(control_sender),
            ..UnifiedServiceOwnerConfig::default()
        })
        .await
        .unwrap();
        owner
            .runtime()
            .lock()
            .expect("runtime lock")
            .apply_discovery_snapshot(&streamable_peer_snapshot(target_addr));

        let request = owner
            .connect_device("peer-a", StreamStartOptions::default(), 200)
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .unwrap()
            .unwrap();

        let change = owner.expire_timed_out(251).expect("timeout change");

        assert_eq!(change.reason, RoleChangeReason::Timeout);
        assert_eq!(change.previous.session_id(), Some(request.session_id));
        assert_eq!(
            owner.runtime().lock().expect("runtime lock").role_state(),
            &RoleState::Idle
        );
    }

    #[tokio::test]
    async fn owner_timeout_monitor_expires_connecting_session() {
        let control = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (control_sender, _control_rx) = control.split();
        let target = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let (_target_sender, target_rx) = target.split();
        let owner = UnifiedServiceOwner::start(UnifiedServiceOwnerConfig {
            app: UnifiedAppConfig {
                role_config: RoleStateMachineConfig {
                    connecting_timeout_ms: 1,
                    ..RoleStateMachineConfig::default()
                },
                first_session_id: 1,
            },
            client_control_sender: Some(control_sender),
            session_timeout_monitor: Some(UnifiedSessionTimeoutMonitorConfig {
                poll_interval: Duration::from_millis(5),
            }),
            ..UnifiedServiceOwnerConfig::default()
        })
        .await
        .unwrap();
        owner
            .runtime()
            .lock()
            .expect("runtime lock")
            .apply_discovery_snapshot(&streamable_peer_snapshot(target_addr));

        owner
            .connect_device("peer-a", StreamStartOptions::default(), unix_now_ms())
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .unwrap()
            .unwrap();

        wait_for_owner_role(&owner, RoleKind::Idle).await;
        assert_eq!(owner.task_count(), 2);
    }

    #[tokio::test]
    async fn client_media_event_marks_connecting_owner_as_viewing() {
        let (owner, target_rx, event_tx) = owner_with_control_peer_and_session_events().await;
        let request = owner
            .connect_device("peer-a", StreamStartOptions::default(), 200)
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .unwrap()
            .unwrap();

        event_tx
            .send(ClientSessionEvent::MediaReceived {
                session_id: request.session_id,
                payload_type: protocol::PayloadType::VideoH265 as u8,
            })
            .unwrap();

        wait_for_owner_role(&owner, RoleKind::Viewing).await;
    }

    #[tokio::test]
    async fn mismatched_client_media_event_does_not_mark_owner_connected() {
        let (owner, target_rx, event_tx) = owner_with_control_peer_and_session_events().await;
        let request = owner
            .connect_device("peer-a", StreamStartOptions::default(), 200)
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .unwrap()
            .unwrap();

        event_tx
            .send(ClientSessionEvent::MediaReceived {
                session_id: request.session_id + 1,
                payload_type: protocol::PayloadType::VideoH265 as u8,
            })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            owner
                .runtime()
                .lock()
                .expect("runtime lock")
                .role_state()
                .kind(),
            RoleKind::Connecting
        );
    }

    #[tokio::test]
    async fn client_telemetry_event_marks_current_connecting_owner_as_viewing() {
        let (owner, target_rx, event_tx) = owner_with_control_peer_and_session_events().await;
        let _request = owner
            .connect_device("peer-a", StreamStartOptions::default(), 200)
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .unwrap()
            .unwrap();

        event_tx
            .send(ClientSessionEvent::HostTelemetry {
                fps: 60.0,
                encode_latency_ms: 4.0,
                jitter_ms: 1.0,
                bitrate_kbps: 8_000,
                rtt_ms: 0.0,
                e2e_latency_ms: 0.0,
                decode_latency_ms: 0.0,
            })
            .unwrap();

        wait_for_owner_role(&owner, RoleKind::Viewing).await;
    }

    trait SnapshotFixture {
        fn devices_to_snapshot_like_cache(&mut self) -> DiscoveryPeerSnapshot;
    }

    impl SnapshotFixture for UnifiedAppRuntime {
        fn devices_to_snapshot_like_cache(&mut self) -> DiscoveryPeerSnapshot {
            let mut cache = DiscoveryPeerCache::new("test-net", "local-device");
            for device in self.devices() {
                cache.apply_announcement(
                    DiscoveryAnnouncement {
                        network_name: "test-net".to_string(),
                        device_id: device.device_id,
                        display_name: device.display_name,
                        control_port: device.endpoint.port(),
                        virtual_ip: Some(device.endpoint.ip()),
                        capabilities: DiscoveryCapabilities {
                            can_stream: device.can_stream,
                            can_view: device.can_view,
                            file_transfer: false,
                            clipboard_sync: false,
                            talkback: false,
                        },
                        scope: device.scope,
                        ttl: Duration::from_secs(8),
                    },
                    SocketAddr::from(([127, 0, 0, 1], 38117)),
                    device.last_seen_ms,
                );
            }
            cache.snapshot()
        }
    }
}
