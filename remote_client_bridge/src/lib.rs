pub mod publisher_bridge;
pub mod touch_mapper;
pub mod workspace_bridge;
mod workspace_touch;

#[cfg(feature = "android")]
pub mod jni_bridge;

#[cfg(feature = "wasm")]
pub mod wasm_bridge;

use protocol::session::{CaptureSource, SessionCommand, SubscriptionRequest};
use protocol::{ControlMessage, TouchAction};
use remote_core::client_session::{AudioIngressEvent, SharedHostStats};
use remote_core::discovery::{
    DEFAULT_DISCOVERY_PORT, DEFAULT_PEER_TTL, DiscoveryAnnouncement, DiscoveryCapabilities,
    DiscoveryPeerSnapshot, DiscoveryRouteOverride, DiscoveryRuntimeConfig, DiscoveryScope,
    run_discovery_runtime,
};
use remote_core::mesh::{AppPrivateMeshConfigStore, MeshConfig, default_app_private_mesh_dir};
#[cfg(test)]
use remote_core::net::UdpMultiplexer;
use remote_core::p2p::{
    BoundP2pTunnel, P2pTunnelConfig, derive_p2p_group_id, resolve_p2p_rendezvous_addrs,
};
use remote_core::pairing_qr::parse_pairing_qr;
use remote_core::relay::{
    BoundWebSocketRelayTunnel, WebSocketRelayTunnelConfig, derive_relay_group_id,
};
use remote_core::workspace_session::WorkspaceConnection;

use remote_core::stats::Statistics;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::{broadcast, mpsc, watch};
use touch_mapper::{TouchMode, TouchStateTracker};

const DEFAULT_P2P_RENDEZVOUS: &str = "p.hackerlife.fun:3478";
const DEFAULT_RELAY_URL: &str = "wss://relay.hackerlife.fun:8443/v1/relay";
static BRIDGE_DEVICE_GROUP_DIR: OnceLock<PathBuf> = OnceLock::new();
pub(crate) static PUBLISHING_PORT: std::sync::atomic::AtomicU16 =
    std::sync::atomic::AtomicU16::new(0);
static PUBLISHER_CAPABILITIES: Mutex<Option<watch::Receiver<DiscoveryCapabilities>>> = Mutex::new(None);

fn inbound_capabilities(port: u16, capture_authorized: bool) -> DiscoveryCapabilities {
    DiscoveryCapabilities {
        can_view: true,
        can_stream: port != 0 && capture_authorized,
        file_transfer: port != 0,
        clipboard_sync: false,
        talkback: false,
    }
}

fn publisher_capabilities() -> watch::Receiver<DiscoveryCapabilities> {
    PUBLISHER_CAPABILITIES.lock().unwrap().clone()
        .unwrap_or_else(|| watch::channel(inbound_capabilities(0, false)).1)
}

pub fn configure_device_group_dir(path: impl Into<PathBuf>) -> Result<(), PathBuf> {
    BRIDGE_DEVICE_GROUP_DIR.set(path.into())
}

fn bridge_device_group_dir() -> PathBuf {
    BRIDGE_DEVICE_GROUP_DIR
        .get()
        .cloned()
        .unwrap_or_else(default_app_private_mesh_dir)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BridgeSessionState {
    Disconnected,
    Connecting,
    Streaming,
    Reconnecting,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeTelemetry {
    pub fps: f32,
    pub latency_ms: f32,
    pub jitter_ms: f32,
    pub packet_loss_percent: f32,
    pub video_bitrate_kbps: u32,
    pub audio_bitrate_kbps: u32,
    pub control_bitrate_kbps: u32,
    pub file_bitrate_kbps: u32,
    pub transport_health_score: f32,
}

impl Default for BridgeTelemetry {
    fn default() -> Self {
        Self {
            fps: 0.0,
            latency_ms: 0.0,
            jitter_ms: 0.0,
            packet_loss_percent: 0.0,
            video_bitrate_kbps: 0,
            audio_bitrate_kbps: 0,
            control_bitrate_kbps: 0,
            file_bitrate_kbps: 0,
            transport_health_score: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeDiscoveredDevice {
    pub device_id: String,
    pub display_name: String,
    pub endpoint: String,
    pub scope: String,
    pub can_stream: bool,
    #[serde(default)]
    pub can_view: bool,
    #[serde(default)]
    pub file_transfer: bool,
    #[serde(default)]
    pub clipboard_sync: bool,
    #[serde(default)]
    pub has_service: bool,
    pub online: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedVideoNalu {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub pts_us: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedAudioPacket {
    pub data: Vec<u8>,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub pts_us: i64,
}

struct LiveSession {
    connection: Arc<WorkspaceConnection>,
    session_id: u32,
    activity_revision: std::sync::atomic::AtomicU64,
    supports_input: Arc<AtomicBool>,
    video_activity: Arc<Mutex<Option<Instant>>>,
    _decode_pump: tokio::task::JoinHandle<()>,
    _audio_pump: tokio::task::JoinHandle<()>,
    _telemetry: tokio::task::JoinHandle<()>,
    _events: tokio::task::JoinHandle<()>,
}

impl LiveSession {
    fn input_ready(&self) -> bool {
        self.supports_input.load(Relaxed)
            && self.video_activity.lock().unwrap().is_some()
            && self.connection.input_ready(self.session_id, 0)
    }
    fn input(&self, event: protocol::InputEvent) {
        let _ = self.connection.try_control(SessionCommand::SourceInput {
            id: self.session_id,
            source_revision: 0,
            event,
        });
    }
    async fn stop(self) {
        self.supports_input.store(false, Relaxed);
        let tasks = [
            self._decode_pump,
            self._audio_pump,
            self._telemetry,
            self._events,
        ];
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
        // Media stops immediately; the shared connection actor stays alive to
        // deliver release and retry Close until its matching acknowledgement.
        let _ = self
            .connection
            .control(SessionCommand::ReleaseInput {
                id: self.session_id,
            })
            .await;
        self.connection.close().await;
    }
}

fn manual_subscription(id: u32, rates: (u32, u32)) -> SubscriptionRequest {
    SubscriptionRequest {
        id,
        source: CaptureSource::MainDisplay,
        width: 1920,
        height: 1080,
        fps: rates.0,
        bitrate_kbps: rates.1,
        audio: true,
    }
}

pub struct RemoteBridgeClient {
    runtime: OnceLock<Runtime>,
    state_tx: watch::Sender<BridgeSessionState>,
    pub state_rx: watch::Receiver<BridgeSessionState>,
    telemetry_tx: watch::Sender<BridgeTelemetry>,
    pub telemetry_rx: watch::Receiver<BridgeTelemetry>,
    last_error_tx: watch::Sender<String>,
    pub last_error_rx: watch::Receiver<String>,
    devices_tx: watch::Sender<Vec<BridgeDiscoveredDevice>>,
    pub devices_rx: watch::Receiver<Vec<BridgeDiscoveredDevice>>,
    touch_tracker: Arc<Mutex<TouchStateTracker>>,
    host_stats: Arc<SharedHostStats>,
    nalu_queue: Arc<Mutex<VecDeque<EncodedVideoNalu>>>,
    audio_queue: Arc<Mutex<VecDeque<EncodedAudioPacket>>>,
    live: Mutex<Option<LiveSession>>,
    virtual_keys: Mutex<touch_mapper::VirtualKeyboard>,
    manual_input_intent: Mutex<Option<(protocol::realtime_input::InputBinding, Instant, u64)>>,
    manual_token_binding: Mutex<Option<(protocol::realtime_input::InputBinding, u64)>>,
    manual_token_sequence: AtomicU64,
    manual_input_owner: AtomicU64,
    manual_input_revoked: AtomicU64,
    lifecycle: Mutex<()>,
    last_connect: Mutex<Option<(String, String)>>,
    connected_at: Mutex<Option<Instant>>,
    active_target: Arc<RwLock<Option<String>>>,
    active_session_id: Arc<AtomicU32>,
    next_session_id: AtomicU32,
    stream_rates: Mutex<(u32, u32)>,
    desired_media_paused: AtomicBool,
    network_reload_tx: mpsc::UnboundedSender<()>,
    network_enabled: Arc<AtomicBool>,
}

impl Default for RemoteBridgeClient {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RemoteBridgeClient {
    fn drop(&mut self) {
        self.stop_live();
    }
}

impl RemoteBridgeClient {
    pub fn new() -> Self {
        let (state_tx, state_rx) = watch::channel(BridgeSessionState::Disconnected);
        let (telemetry_tx, telemetry_rx) = watch::channel(BridgeTelemetry::default());
        let (last_error_tx, last_error_rx) = watch::channel(String::new());
        let (devices_tx, devices_rx) = watch::channel(Vec::new());
        let (network_reload_tx, network_reload_rx) = mpsc::unbounded_channel();
        let client = Self {
            runtime: OnceLock::new(),
            state_tx,
            state_rx,
            telemetry_tx,
            telemetry_rx,
            last_error_tx,
            last_error_rx,
            devices_tx,
            devices_rx,
            touch_tracker: Arc::new(Mutex::new(TouchStateTracker::default())),
            host_stats: Arc::new(SharedHostStats::default()),
            nalu_queue: Arc::new(Mutex::new(VecDeque::with_capacity(8))),
            audio_queue: Arc::new(Mutex::new(VecDeque::with_capacity(16))),
            live: Mutex::new(None),
            virtual_keys: Mutex::new(touch_mapper::VirtualKeyboard::default()),
            manual_input_intent: Mutex::new(None),
            manual_token_binding: Mutex::new(None),
            manual_token_sequence: AtomicU64::new(0),
            manual_input_owner: AtomicU64::new(0),
            manual_input_revoked: AtomicU64::new(0),
            lifecycle: Mutex::new(()),
            last_connect: Mutex::new(None),
            connected_at: Mutex::new(None),
            active_target: Arc::new(RwLock::new(None)),
            active_session_id: Arc::new(AtomicU32::new(0)),
            next_session_id: AtomicU32::new(1),
            stream_rates: Mutex::new((60, 20_000)),
            desired_media_paused: AtomicBool::new(false),
            network_reload_tx,
            network_enabled: Arc::new(AtomicBool::new(true)),
        };
        client.ensure_discovery(network_reload_rx);
        client
    }

    fn runtime(&self) -> &Runtime {
        self.runtime.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("remote-play-bridge")
                .build()
                .expect("bridge tokio runtime")
        })
    }

    pub fn set_touch_mode(&self, mode: TouchMode) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        let releases = self
            .touch_tracker
            .lock()
            .map(|mut tracker| tracker.set_mode(mode))
            .unwrap_or_default();
        if let Some(live) = self.live.lock().unwrap().as_ref() {
            for event in releases {
                live.input(event);
            }
        }
    }

    pub fn set_screen_bounds(&self, width: u16, height: u16) {
        if let Ok(mut tracker) = self.touch_tracker.lock() {
            tracker.set_bounds(width, height);
        }
    }

    /// A viewport captures this local session token once. It cannot refresh a
    /// replacement connection; telemetry/background keepalives never call this.
    // Local opaque tokens include the complete grant, including its epoch.
    // Called under lifecycle; issuing a token neither renews nor revokes input.
    fn manual_token(&self, binding: protocol::realtime_input::InputBinding) -> u64 {
        let mut current = self.manual_token_binding.lock().unwrap();
        if let Some((old, token)) = *current && old == binding { return token; }
        let token = self.manual_token_sequence.fetch_update(Relaxed, Relaxed, |v| v.checked_add(1))
            .expect("manual input token exhausted") + 1;
        *current = Some((binding, token));
        token
    }
    fn revoke_manual_token(&self, token: u64) {
        if token != 0 && self.manual_input_owner.load(Relaxed) == token {
            self.manual_input_revoked.fetch_max(token, Relaxed);
        }
    }
    pub fn input_intent_token(&self) -> u64 {
        let Ok(_lifecycle) = self.lifecycle.try_lock() else { return 0; };
        let Ok(live) = self.live.try_lock() else { return 0; };
        live.as_ref().filter(|live| !self.desired_media_paused.load(Relaxed) && live.input_ready())
            .and_then(|live| live.connection.input_binding(live.session_id, 0))
            .map_or(0, |binding| self.manual_token(binding))
    }
    // Called under lifecycle + live locks, before any gesture tail or health
    // renewal. DirectTouch Down has no core held button yet, so its local
    // tracker needs the same deadline and exact epoch independently.
    fn expire_manual_input(&self, live: &LiveSession, binding: protocol::realtime_input::InputBinding, now: Instant) -> bool {
        let mut intent = self.manual_input_intent.lock().unwrap();
        // A failed nonblocking UI call may have dropped a gesture's Cancel.
        // Consume that invalidation before another heartbeat can renew it.
        let revoked = intent.is_some_and(|(_, _, token)| self.manual_input_revoked.compare_exchange(token, 0, Relaxed, Relaxed).is_ok());
        if revoked || intent.is_some_and(|(old, last, _)| old != binding || now.saturating_duration_since(last).as_millis() >= protocol::realtime_input::HOLD_TIMEOUT_MS as u128) {
            self.touch_tracker.lock().unwrap().cancel_all();
            *self.virtual_keys.lock().unwrap() = touch_mapper::VirtualKeyboard::default();
            if let Some((old, _, _)) = intent.take() { live.connection.try_bound_input(old, None); }
            self.manual_input_owner.store(0, Relaxed);
            return true;
        }
        false
    }
    pub fn refresh_input_intent(&self, token: u64) -> bool {
        let Ok(_lifecycle) = self.lifecycle.try_lock() else { self.revoke_manual_token(token); return false; };
        let Ok(live) = self.live.try_lock() else { self.revoke_manual_token(token); return false; };
        let Some(live) = live.as_ref().filter(|live| !self.desired_media_paused.load(Relaxed) && live.input_ready()) else { self.revoke_manual_token(token); return false; };
        let Some(binding) = live.connection.input_binding(live.session_id, 0) else { return false; };
        if token == 0 || token != self.manual_token(binding) { return false; }
        let now = Instant::now();
        if self.expire_manual_input(live, binding, now) { return false; }
        if !live.connection.refresh_bound_input(binding) {
            self.touch_tracker.lock().unwrap().cancel_all();
            *self.virtual_keys.lock().unwrap() = touch_mapper::VirtualKeyboard::default();
            *self.manual_input_intent.lock().unwrap() = None;
            self.manual_input_owner.store(0, Relaxed);
            live.connection.try_bound_input(binding, None);
            return false;
        }
        if let Some((_, last, _)) = self.manual_input_intent.lock().unwrap().as_mut() { *last = now; }
        true
    }

    pub fn handle_touch_input(
        &self, action: TouchAction, pointer_id: u32, norm_x: f32, norm_y: f32, pressure: f32,
    ) {
        let Ok(_lifecycle) = self.lifecycle.try_lock() else { self.revoke_manual_token(self.manual_input_owner.load(Relaxed)); return; };
        let Ok(live) = self.live.try_lock() else { self.revoke_manual_token(self.manual_input_owner.load(Relaxed)); return; };
        let Some(live) = live.as_ref().filter(|live| !self.desired_media_paused.load(Relaxed) && live.input_ready()) else {
            self.touch_tracker.lock().unwrap().cancel_all();
            *self.manual_input_intent.lock().unwrap() = None;
            self.manual_input_owner.store(0, Relaxed);
            return;
        };
        let Some(binding) = live.connection.input_binding(live.session_id, 0) else { return; };
        let now = Instant::now();
        self.expire_manual_input(live, binding, now);
        {
            let mut intent = self.manual_input_intent.lock().unwrap();
            if !matches!(action, TouchAction::Down) && intent.is_none() { return; }
            let token = self.manual_token(binding);
            *intent = Some((binding, now, token));
            self.manual_input_owner.store(token, Relaxed);
        }
        let events = self.touch_tracker.lock().unwrap().process_touch(action, pointer_id, norm_x, norm_y, pressure, now);
        for event in events { live.connection.try_bound_input(binding, Some(event)); }
    }

    pub fn send_virtual_key(&self, key_name: &str, pressed: bool) {
        let Ok(_lifecycle) = self.lifecycle.try_lock() else { self.revoke_manual_token(self.manual_input_owner.load(Relaxed)); return; };
        if pressed && self.desired_media_paused.load(Relaxed) { return; }
        let Ok(live) = self.live.try_lock() else { self.revoke_manual_token(self.manual_input_owner.load(Relaxed)); return; };
        if let Some(live) = live.as_ref().filter(|live| live.input_ready()) {
            let Some(binding) = live.connection.input_binding(live.session_id, 0) else { return; };
            let now = Instant::now();
            self.expire_manual_input(live, binding, now);
            {
                let mut intent = self.manual_input_intent.lock().unwrap();
                if !pressed && intent.is_none() { return; }
                let token = self.manual_token(binding);
            *intent = Some((binding, now, token));
            self.manual_input_owner.store(token, Relaxed);
            }
            for event in self.virtual_keys.lock().unwrap().event(key_name, pressed) {
                live.connection.try_bound_input(binding, Some(event));
            }
        } else {
            *self.virtual_keys.lock().unwrap() = touch_mapper::VirtualKeyboard::default();
            *self.manual_input_intent.lock().unwrap() = None;
            self.manual_input_owner.store(0, Relaxed);
        }
    }

    pub fn poll_video_nalu(&self) -> Option<EncodedVideoNalu> {
        self.nalu_queue.lock().ok()?.pop_front()
    }

    pub fn poll_audio_packet(&self) -> Option<EncodedAudioPacket> {
        self.audio_queue.lock().ok()?.pop_front()
    }

    pub fn devices_json(&self) -> String {
        serde_json::to_string(&*self.devices_rx.borrow()).unwrap_or_else(|_| "[]".to_string())
    }

    pub fn network_identity_json(&self) -> String {
        let store = AppPrivateMeshConfigStore::new(bridge_device_group_dir());
        match store.load() {
            Ok(Some(config)) => serde_json::json!({
                "network_name": config.network_name,
                "device_id": config.node_id,
                "display_name": config.display_name,
            })
            .to_string(),
            _ => "{}".into(),
        }
    }

    pub fn join_pairing_payload(&self, raw: &str) -> Result<String, String> {
        self.join_pairing_payload_with_change(raw)
            .map(|(message, _)| message)
    }

    pub(crate) fn join_pairing_payload_with_change(
        &self,
        raw: &str,
    ) -> Result<(String, bool), String> {
        let payload = parse_pairing_qr(raw).map_err(|err| err.to_string())?;
        let _lifecycle = self.lifecycle.lock().unwrap();
        let store = AppPrivateMeshConfigStore::new(bridge_device_group_dir());
        let (config, changed) = store
            .prepare_join(&payload.invite_code, "RemotePlay Android")
            .map_err(|err| err.to_string())?;
        if !changed {
            return Ok((format!("Already in network {}", config.network_name), false));
        }
        store.save(&config).map_err(|err| err.to_string())?;
        self.stop_live();
        *self.last_connect.lock().unwrap() = None;
        let _ = self.state_tx.send(BridgeSessionState::Disconnected);
        remote_core::session_crypto::refresh_paired_session_secret(
            config.network_secret.expose_secret(),
        );
        self.devices_tx.send_replace(Vec::new());
        self.network_enabled.store(true, Relaxed);
        let _ = self.network_reload_tx.send(());
        Ok((
            format!(
                "Joined network {}. Discovering members...",
                config.network_name
            ),
            true,
        ))
    }

    fn ensure_discovery(&self, network_reload_rx: mpsc::UnboundedReceiver<()>) {
        let devices_tx = self.devices_tx.clone();
        self.runtime().spawn(run_bridge_network_supervisor(
            devices_tx,
            network_reload_rx,
            self.network_enabled.clone(),
        ));
    }

    /// Suspend discovery and route keepalives only after the live session has ended.
    pub fn set_network_enabled(&self, enabled: bool) {
        if self.network_enabled.swap(enabled, Relaxed) != enabled {
            let _ = self.network_reload_tx.send(());
        }
    }

    fn next_session(&self) -> u32 {
        loop {
            let id = self.next_session_id.fetch_add(1, Relaxed);
            if id != 0 {
                return id;
            }
        }
    }

    pub fn connect(&self, device_id: String, endpoint: String) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        self.stop_live();
        *self.last_connect.lock().unwrap() = None;
        self.connect_locked(device_id, endpoint);
    }

    fn connect_locked(&self, device_id: String, endpoint: String) {
        self.last_error_tx.send_replace(String::new());
        if *self.state_rx.borrow() != BridgeSessionState::Reconnecting {
            let _ = self.state_tx.send(BridgeSessionState::Connecting);
        }
        let parsed = match endpoint.parse::<SocketAddr>() {
            Ok(addr) => addr,
            Err(err) => {
                eprintln!("Bridge connect failed to parse endpoint {endpoint}: {err}");
                let _ = self.state_tx.send(BridgeSessionState::Error);
                return;
            }
        };
        *self.active_target.write().unwrap() = Some(device_id.clone());

        let state_tx = self.state_tx.clone();
        let telemetry_tx = self.telemetry_tx.clone();
        let last_error_tx = self.last_error_tx.clone();
        let host_stats = self.host_stats.clone();
        let nalu_queue = self.nalu_queue.clone();
        let active_session_id = self.active_session_id.clone();
        *self.last_connect.lock().unwrap() = Some((device_id.clone(), endpoint.clone()));
        *self.connected_at.lock().unwrap() = Some(Instant::now());
        let session_id = self.next_session();
        active_session_id.store(session_id, Relaxed);
        let stream_rates = *self.stream_rates.lock().unwrap();
        let paused = self.desired_media_paused.load(Relaxed);

        let audio_queue = self.audio_queue.clone();
        let result = self.runtime().block_on(async move {
            start_live_session(
                parsed,
                session_id,
                stream_rates,
                paused,
                host_stats,
                nalu_queue,
                audio_queue,
                active_session_id,
                telemetry_tx,
                state_tx,
                last_error_tx,
            )
            .await
        });

        match result {
            Ok(live) => {
                *self.live.lock().unwrap() = Some(live);
            }
            Err(err) => {
                let reason = err.to_string();
                eprintln!("Bridge connect failed: {reason}");
                self.stop_live();
                self.last_error_tx.send_replace(reason);
                let _ = self.state_tx.send(BridgeSessionState::Error);
            }
        }
    }

    pub fn disconnect(&self) {
        self.desired_media_paused.store(false, Relaxed);
        let _lifecycle = self.lifecycle.lock().unwrap();
        self.stop_live();
        *self.last_connect.lock().unwrap() = None;
        let _ = self.state_tx.send(BridgeSessionState::Disconnected);
    }

    fn stop_live(&self) {
        *self.manual_token_binding.lock().unwrap() = None;
        *self.manual_input_intent.lock().unwrap() = None;
            self.manual_input_owner.store(0, Relaxed);
        self.host_stats.media_pause.end_session();
        if let Some(live) = self.live.lock().unwrap().take() {
            self.runtime().block_on(live.stop());
        }
        *self.virtual_keys.lock().unwrap() = touch_mapper::VirtualKeyboard::default();
        self.touch_tracker.lock().unwrap().cancel_all();
        self.active_session_id.store(0, Relaxed);
        self.nalu_queue.lock().unwrap().clear();
        self.audio_queue.lock().unwrap().clear();
        self.host_stats.reset();
        self.telemetry_tx.send_replace(BridgeTelemetry::default());
        *self.active_target.write().unwrap() = None;
        *self.connected_at.lock().unwrap() = None;
    }

    pub fn update_stream_rates(&self, fps: u32, bitrate_kbps: u32) -> Result<(), String> {
        protocol::validate_video_settings(1920, 1080, fps, bitrate_kbps).map_err(str::to_owned)?;
        let _lifecycle = self.lifecycle.lock().unwrap();
        *self.stream_rates.lock().unwrap() = (fps, bitrate_kbps);
        if let Some(live) = self.live.lock().unwrap().as_ref() {
            self.runtime().block_on(
                live.connection
                    .update_settings(&manual_subscription(live.session_id, (fps, bitrate_kbps))),
            )?;
        }
        Ok(())
    }

    pub fn set_media_paused(&self, paused: bool) {
        self.desired_media_paused.store(paused, Relaxed);
        let _lifecycle = self.lifecycle.lock().unwrap();
        if self.host_stats.media_pause.is_paused() == paused {
            return;
        }
        if !self.host_stats.media_pause.set_paused(paused) {
            return;
        }
        self.touch_tracker.lock().unwrap().cancel_all();
        *self.virtual_keys.lock().unwrap() = touch_mapper::VirtualKeyboard::default();
        *self.manual_input_intent.lock().unwrap() = None;
            self.manual_input_owner.store(0, Relaxed);
        if let Some(live) = self.live.lock().unwrap().as_ref() {
            let revision = live.activity_revision.fetch_add(1, Relaxed) + 1;
            let _ = self
                .runtime()
                .block_on(live.connection.control(SessionCommand::SetActivity {
                    id: live.session_id,
                    revision,
                    video: !paused,
                    audio: !paused,
                }));
        }
        self.nalu_queue.lock().unwrap().clear();
        self.audio_queue.lock().unwrap().clear();
        if !paused {
            *self.connected_at.lock().unwrap() = Some(Instant::now());
            if let Some(live) = self.live.lock().unwrap().as_ref() {
                *live.video_activity.lock().unwrap() = None;
            }
        }
    }

    pub fn maybe_reconnect(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        if !matches!(
            *self.state_rx.borrow(),
            BridgeSessionState::Streaming
                | BridgeSessionState::Connecting
                | BridgeSessionState::Reconnecting
        ) {
            return;
        }
        let last_video = self
            .live
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|live| *live.video_activity.lock().unwrap());
        let host = self.host_stats.snapshot();
        // Some capture backends emit no frames for a static desktop. Fresh zero-FPS
        // telemetry keeps that established session alive; startup still needs video.
        if (last_video.is_some() || self.host_stats.media_pause.is_paused())
            && host.fps == 0.0
            && host
                .updated_at
                .is_some_and(|updated| updated.elapsed() <= Duration::from_secs(12))
        {
            return;
        }
        let stale = match (last_video, *self.connected_at.lock().unwrap()) {
            (Some(updated), _) => updated.elapsed() > Duration::from_secs(12),
            (None, Some(connected_at)) => connected_at.elapsed() > Duration::from_secs(12),
            (None, None) => false,
        };
        if !stale {
            return;
        }
        let Some((device_id, previous_endpoint)) = self.last_connect.lock().unwrap().clone() else {
            return;
        };
        let endpoint = self
            .devices_rx
            .borrow()
            .iter()
            .find(|device| device.device_id == device_id && device.online && device.can_stream)
            .map(|device| device.endpoint.clone())
            .unwrap_or(previous_endpoint);
        let _ = self.state_tx.send(BridgeSessionState::Reconnecting);
        self.stop_live();
        self.connect_locked(device_id, endpoint);
    }

    pub fn request_keyframe(&self) {
        if self.host_stats.media_pause.is_paused() {
            return;
        }
        if let Some(live) = self.live.lock().unwrap().as_ref() {
            let _ = self.runtime().block_on(live.connection.sender.send_control(
                &ControlMessage::RequestKeyframe {
                    session_id: live.session_id,
                },
                live.connection.target,
            ));
        }
    }

    pub fn update_telemetry(&self, telemetry: BridgeTelemetry) {
        let _ = self.telemetry_tx.send(telemetry);
    }

    pub fn refresh_telemetry_from_stats(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        let snap = self.host_stats.snapshot();
        if snap.updated_at.is_none() {
            self.telemetry_tx.send_replace(BridgeTelemetry::default());
            return;
        }
        let _ = self.telemetry_tx.send(BridgeTelemetry {
            fps: snap.fps,
            latency_ms: snap.e2e_latency_ms.max(snap.latency),
            jitter_ms: snap.jitter,
            packet_loss_percent: snap.packet_loss_rate,
            video_bitrate_kbps: snap.bitrate_kbps,
            audio_bitrate_kbps: 0,
            control_bitrate_kbps: 0,
            file_bitrate_kbps: 0,
            transport_health_score: (100.0 - snap.packet_loss_rate).clamp(0.0, 100.0),
        });
    }

    pub fn update_devices_from_snapshot(&self, snapshot: &DiscoveryPeerSnapshot) {
        let _ = self.devices_tx.send(devices_from_snapshot(snapshot));
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_live_session(
    target: SocketAddr,
    session_id: u32,
    stream_rates: (u32, u32),
    paused: bool,
    host_stats: Arc<SharedHostStats>,
    nalu_queue: Arc<Mutex<VecDeque<EncodedVideoNalu>>>,
    audio_queue: Arc<Mutex<VecDeque<EncodedAudioPacket>>>,
    _active_session_id: Arc<AtomicU32>,
    telemetry_tx: watch::Sender<BridgeTelemetry>,
    state_tx: watch::Sender<BridgeSessionState>,
    last_error_tx: watch::Sender<String>,
) -> Result<LiveSession, Box<dyn std::error::Error + Send + Sync>> {
    let (connection, events) = WorkspaceConnection::connect_media(target)
        .await
        .map_err(std::io::Error::other)?;
    let connection = Arc::new(connection);
    let mut audio_rx = events.audio;
    let mut session_event_rx = events.control;
    host_stats.media_pause.begin_session(target, session_id);
    host_stats.media_pause.set_paused(paused);
    let (decode_tx, mut decode_rx) = mpsc::channel(64);
    connection
        .subscribe(
            manual_subscription(session_id, stream_rates),
            decode_tx,
            Statistics::new(),
            host_stats.clone(),
        )
        .await
        .map_err(std::io::Error::other)?;
    if paused {
        connection
            .control(SessionCommand::SetActivity {
                id: session_id,
                revision: 1,
                video: false,
                audio: false,
            })
            .await
            .map_err(std::io::Error::other)?;
    }
    let supports_input = Arc::new(AtomicBool::new(false));
    let event_input = supports_input.clone();
    let event_state_tx = state_tx.clone();
    let event_error_tx = last_error_tx.clone();
    let event_connection = connection.clone();
    let events = tokio::spawn(async move {
        while let Some(event) = session_event_rx.recv().await {
            match event {
                SessionCommand::Subscribed {
                    id, supports_input, ..
                } if id == session_id => {
                    event_input.store(supports_input, Relaxed);
                }
                SessionCommand::Error { request_id, reason } if request_id == session_id => {
                    event_input.store(false, Relaxed);
                    event_error_tx.send_replace(reason);
                    let _ = event_state_tx.send(BridgeSessionState::Error);
                    let _ = event_connection
                        .control(SessionCommand::Unsubscribe { id: session_id })
                        .await;
                    break;
                }
                SessionCommand::Closed { .. } => {
                    event_input.store(false, Relaxed);
                    break;
                }
                _ => {}
            }
        }
    });

    let recovery_connection = connection.clone();
    let video_activity = Arc::new(Mutex::new(None));
    let activity = video_activity.clone();
    let media_stats = host_stats.clone();
    let decode_pump = tokio::spawn(async move {
        let mut waiting_for_keyframe = true;
        let mut received_video = false;
        while let Some((packet, timing)) = decode_rx.recv().await {
            if media_stats.media_pause.is_paused() {
                waiting_for_keyframe = true;
                continue;
            }
            if activity.lock().unwrap().is_none() {
                waiting_for_keyframe = true;
            }
            let keyframe = hevc_keyframe(&packet.payload);
            let nalu = EncodedVideoNalu {
                data: packet.payload,
                keyframe,
                pts_us: timing.capture_ts_us as i64,
            };
            if let Ok(mut queue) = nalu_queue.lock() {
                if queue.len() >= 8 {
                    queue.clear();
                    waiting_for_keyframe = true;
                    let sender = recovery_connection.sender.clone();
                    tokio::spawn(async move {
                        let _ = sender
                            .send_control(&ControlMessage::RequestKeyframe { session_id }, target)
                            .await;
                    });
                }
                if waiting_for_keyframe && !keyframe {
                    continue;
                }
                waiting_for_keyframe = false;
                *activity.lock().unwrap() = Some(Instant::now());
                queue.push_back(nalu);
                if !received_video {
                    received_video = true;
                    let _ = state_tx.send(BridgeSessionState::Streaming);
                }
            }
        }
    });

    let audio_media_stats = host_stats.clone();
    let audio_pump = tokio::spawn(async move {
        let mut sample_rate = 48_000u32;
        let mut channels = 2u16;
        while let Some(event) = audio_rx.recv().await {
            match event {
                AudioIngressEvent::StreamConfig(config) => {
                    sample_rate = config.sample_rate_hz;
                    channels = config.channels;
                }
                AudioIngressEvent::Packet(packet) => {
                    if audio_media_stats.media_pause.is_paused() {
                        continue;
                    }
                    let pkt = EncodedAudioPacket {
                        data: packet.payload,
                        sample_rate_hz: sample_rate,
                        channels,
                        pts_us: i64::from(packet.header.timestamp) * 1000,
                    };
                    if let Ok(mut queue) = audio_queue.lock() {
                        if queue.len() >= 16 {
                            queue.pop_front();
                        }
                        queue.push_back(pkt);
                    }
                }
            }
        }
    });

    let telemetry = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let snap = host_stats.snapshot();
            if snap.updated_at.is_none() {
                continue;
            }
            let _ = telemetry_tx.send(BridgeTelemetry {
                fps: snap.fps,
                latency_ms: snap.e2e_latency_ms.max(snap.latency),
                jitter_ms: snap.jitter,
                packet_loss_percent: snap.packet_loss_rate,
                video_bitrate_kbps: snap.bitrate_kbps,
                audio_bitrate_kbps: 0,
                control_bitrate_kbps: 0,
                file_bitrate_kbps: 0,
                transport_health_score: (100.0 - snap.packet_loss_rate).clamp(0.0, 100.0),
            });
        }
    });

    Ok(LiveSession {
        connection,
        session_id,
        activity_revision: std::sync::atomic::AtomicU64::new(u64::from(paused)),
        supports_input,
        video_activity,
        _decode_pump: decode_pump,
        _audio_pump: audio_pump,
        _telemetry: telemetry,
        _events: events,
    })
}

fn hevc_keyframe(data: &[u8]) -> bool {
    // The last three bytes of either Annex B prefix are 00 00 01.
    // Require the complete two-byte HEVC header and an IRAP VCL type.
    data.windows(5).any(|window| {
        window[..3] == [0, 0, 1]
            && window[3] & 0x80 == 0
            && window[4] & 0x07 != 0
            && (16..=21).contains(&((window[3] >> 1) & 0x3f))
    })
}

async fn run_bridge_network_supervisor(
    devices_tx: watch::Sender<Vec<BridgeDiscoveredDevice>>,
    mut reload_rx: mpsc::UnboundedReceiver<()>,
    enabled: Arc<AtomicBool>,
) {
    loop {
        if !enabled.load(Relaxed) {
            if reload_rx.recv().await.is_none() {
                break;
            }
            continue;
        }
        let (cancel_tx, _) = broadcast::channel(8);
        let network_devices_tx = devices_tx.clone();
        let network_cancel_tx = cancel_tx.clone();
        let mut network_task =
            tokio::spawn(
                async move { run_bridge_network(network_devices_tx, network_cancel_tx).await },
            );

        tokio::select! {
            reload = reload_rx.recv() => {
                let _ = cancel_tx.send(());
                let _ = network_task.await;
                devices_tx.send_replace(Vec::new());
                if reload.is_none() {
                    break;
                }
            }
            result = &mut network_task => {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => eprintln!("Bridge network stopped: {err}"),
                    Err(err) if !err.is_cancelled() => eprintln!("Bridge network task stopped: {err}"),
                    Err(_) => {}
                }
                devices_tx.send_replace(Vec::new());
                tokio::select! {
                    reload = reload_rx.recv() => {
                        if reload.is_none() {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
            }
        }
    }
}

async fn run_bridge_network(
    devices_tx: watch::Sender<Vec<BridgeDiscoveredDevice>>,
    cancel_tx: broadcast::Sender<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Subscribe before startup awaits so a background cancellation is not lost.
    let mut cancel_rx = cancel_tx.subscribe();
    let store = AppPrivateMeshConfigStore::new(bridge_device_group_dir());
    let identity = store.load_or_generate("RemotePlay Android")?;
    let live_capabilities = publisher_capabilities();
    let announcement = DiscoveryAnnouncement {
        network_name: identity.network_name.clone(),
        device_id: identity.node_id.clone(),
        display_name: identity.display_name.clone(),
        control_port: PUBLISHING_PORT.load(Relaxed),
        virtual_ip: None,
        capabilities: *live_capabilities.borrow(),
        scope: DiscoveryScope::Lan,
        ttl: DEFAULT_PEER_TTL,
    };
    let discovery_target = SocketAddr::from(([127, 0, 0, 1], DEFAULT_DISCOVERY_PORT));
    let mut discovery_config = DiscoveryRuntimeConfig::lan_default(announcement.clone());
    discovery_config.accept_any_network = false;
    discovery_config.live_capabilities = Some(live_capabilities.clone());
    let mut network_tasks = Vec::new();

    // Unit tests stay hermetic. Android/native builds use the same public P2P + relay
    // fabric as the desktop app.
    if !cfg!(test) {
        match start_bridge_p2p(
            &identity,
            &announcement,
            live_capabilities.clone(),
            discovery_target,
            cancel_tx.clone(),
        )
        .await
        {
            Ok(task) => network_tasks.push(task),
            Err(err) => eprintln!("Bridge P2P unavailable; relay fallback remains active: {err}"),
        }

        let host_target = (announcement.control_port != 0)
            .then(|| SocketAddr::from(([127, 0, 0, 1], announcement.control_port)));
        match start_bridge_relay(&identity, host_target, discovery_target, cancel_tx.clone()).await {
            Ok((control_endpoint, discovery_endpoint, routes, tasks)) => {
                discovery_config
                    .relay_routes
                    .push((discovery_endpoint, routes));
                discovery_config.announce_targets.push(discovery_endpoint);
                discovery_config
                    .route_overrides
                    .push(DiscoveryRouteOverride {
                        source: discovery_endpoint,
                        endpoint: control_endpoint,
                        scope: DiscoveryScope::Relay,
                    });
                network_tasks.extend(tasks);
            }
            Err(err) => eprintln!("Bridge relay unavailable; LAN/P2P remain active: {err}"),
        }
    }

    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let (snapshot_tx, mut snapshot_rx) = watch::channel(DiscoveryPeerSnapshot::default());
    let discovery_cancel_rx = cancel_tx.subscribe();
    let discovery_task = tokio::spawn(run_discovery_runtime(
        discovery_config,
        events_tx,
        snapshot_tx,
        discovery_cancel_rx,
    ));
    let event_task = tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            if let remote_core::discovery::DiscoveryEvent::Error(err) = event {
                eprintln!("Bridge discovery: {err}");
            }
        }
    });
    loop {
        tokio::select! {
            _ = cancel_rx.recv() => break,
            changed = snapshot_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let snapshot = snapshot_rx.borrow().clone();
                devices_tx.send_replace(devices_from_snapshot(&snapshot));
            }
        }
    }

    let _ = cancel_tx.send(());
    discovery_task.abort();
    event_task.abort();
    let _ = discovery_task.await;
    let _ = event_task.await;
    for task in network_tasks {
        task.abort();
        let _ = task.await;
    }
    Ok(())
}

async fn start_bridge_p2p(
    identity: &MeshConfig,
    announcement: &DiscoveryAnnouncement,
    live_capabilities: watch::Receiver<DiscoveryCapabilities>,
    discovery_target: SocketAddr,
    cancel_tx: broadcast::Sender<()>,
) -> Result<tokio::task::JoinHandle<()>, Box<dyn std::error::Error + Send + Sync>> {
    let rendezvous_name = std::env::var("REMOTE_PLAY_P2P_RENDEZVOUS")
        .unwrap_or_else(|_| DEFAULT_P2P_RENDEZVOUS.to_string());
    let bind_addr = SocketAddr::from(([0, 0, 0, 0], 0));
    let rendezvous_addrs = resolve_p2p_rendezvous_addrs(&rendezvous_name, bind_addr).await?;
    let group_id = derive_p2p_group_id(
        &identity.network_name,
        identity.network_secret.expose_secret(),
    )?;
    let mut config = P2pTunnelConfig::new(
        bind_addr,
        rendezvous_addrs[0],
        group_id,
        identity.node_id.clone(),
        announcement.encode()?,
    )?
    .with_rendezvous_addrs(rendezvous_addrs)
    .with_live_capabilities(live_capabilities)
    .with_routing_key(remote_core::routed_relay::derive_routing_key(
        identity.network_secret.expose_secret().as_bytes(),
    ))
    .with_discovery_target_addr(discovery_target);
    config.local_target_addr = (announcement.control_port != 0)
        .then(|| SocketAddr::from(([127, 0, 0, 1], announcement.control_port)));
    let tunnel = BoundP2pTunnel::bind(config).await?;
    let (snapshot_tx, _snapshot_rx) = watch::channel(Default::default());
    Ok(tokio::spawn(async move {
        if let Err(err) = tunnel.run(cancel_tx.subscribe(), snapshot_tx).await {
            eprintln!("Bridge P2P tunnel stopped: {err}");
        }
    }))
}

async fn start_bridge_relay(
    identity: &MeshConfig,
    host_target: Option<SocketAddr>,
    discovery_target: SocketAddr,
    cancel_tx: broadcast::Sender<()>,
) -> Result<
    (
        SocketAddr,
        SocketAddr,
        remote_core::routed_relay::PeerRelayRoutes,
        Vec<tokio::task::JoinHandle<()>>,
    ),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let relay_url = std::env::var("REMOTE_PLAY_RELAY_SERVER_ADDR")
        .unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string());
    let control_group = derive_relay_group_id(
        &identity.network_name,
        identity.network_secret.expose_secret(),
        "control-device-v1",
    )?;
    let discovery_group = derive_relay_group_id(
        &identity.network_name,
        identity.network_secret.expose_secret(),
        "discovery",
    )?;
    let router = remote_core::routed_relay::RoutedRelay::bind(
        identity.node_id.clone(),
        remote_core::routed_relay::derive_routing_key(
            identity.network_secret.expose_secret().as_bytes(),
        ),
        host_target,
    )
    .await?;
    let router_target = router.local_addr()?;
    let routes = router.routes();
    let control_tunnel = BoundWebSocketRelayTunnel::bind(
        WebSocketRelayTunnelConfig::new(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_url.clone(),
            control_group,
            identity.node_id.clone(),
        )?
        .with_local_target_addr(router_target),
    )
    .await?;
    let control_endpoint = control_tunnel.local_addr()?;
    let discovery_tunnel = BoundWebSocketRelayTunnel::bind(
        WebSocketRelayTunnelConfig::new(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_url,
            discovery_group,
            identity.node_id.clone(),
        )?
        .with_local_target_addr(discovery_target),
    )
    .await?;
    let discovery_endpoint = discovery_tunnel.local_addr()?;
    let control_cancel_tx = cancel_tx.clone();
    let router_cancel = cancel_tx.subscribe();
    let discovery_cancel_tx = cancel_tx;
    let tasks = vec![
        tokio::spawn(async move {
            if let Err(error) = router.run(control_endpoint, router_cancel).await {
                eprintln!("Bridge addressed relay stopped: {error}");
            }
        }),
        tokio::spawn(async move {
            if let Err(err) = control_tunnel.run(control_cancel_tx.subscribe()).await {
                eprintln!("Bridge relay control tunnel stopped: {err}");
            }
        }),
        tokio::spawn(async move {
            if let Err(err) = discovery_tunnel.run(discovery_cancel_tx.subscribe()).await {
                eprintln!("Bridge relay discovery tunnel stopped: {err}");
            }
        }),
    ];
    Ok((control_endpoint, discovery_endpoint, routes, tasks))
}

fn devices_from_snapshot(snapshot: &DiscoveryPeerSnapshot) -> Vec<BridgeDiscoveredDevice> {
    let mut selected = BTreeMap::<String, (u8, BridgeDiscoveredDevice)>::new();
    for peer in snapshot.peers() {
        if peer.scope == DiscoveryScope::Mesh {
            continue;
        }
        let rank = match peer.scope {
            DiscoveryScope::Lan => 0,
            DiscoveryScope::P2p => 1,
            DiscoveryScope::Relay => 2,
            DiscoveryScope::Mesh => continue,
        };
        let device = BridgeDiscoveredDevice {
            device_id: peer.announcement.device_id.clone(),
            display_name: peer.announcement.display_name.clone(),
            endpoint: peer.endpoint.to_string(),
            scope: match peer.scope {
                DiscoveryScope::Lan => "LAN",
                DiscoveryScope::P2p => "P2P Direct",
                DiscoveryScope::Relay => "Relay",
                DiscoveryScope::Mesh => continue,
            }
            .to_string(),
            can_stream: peer.announcement.capabilities.can_stream,
            can_view: peer.announcement.capabilities.can_view,
            file_transfer: peer.announcement.capabilities.file_transfer,
            clipboard_sync: peer.announcement.capabilities.clipboard_sync,
            has_service: peer.announcement.control_port != 0 && peer.endpoint.port() != 0,
            online: true,
        };
        match selected.get(&device.device_id) {
            Some((existing_rank, _)) if *existing_rank <= rank => {}
            _ => {
                selected.insert(device.device_id.clone(), (rank, device));
            }
        }
    }
    selected.into_values().map(|(_, device)| device).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_receiver_does_not_advertise_screen_capture_authorization() {
        let files = inbound_capabilities(5001, false);
        assert!(files.can_view && files.file_transfer);
        assert!(!files.can_stream);
        let capture = inbound_capabilities(5001, true);
        assert!(capture.can_stream && capture.file_transfer);
        assert_eq!(inbound_capabilities(0, true), inbound_capabilities(0, false));
        assert!(!inbound_capabilities(0, false).file_transfer);
    }

    #[test]
    fn bridge_client_starts_disconnected() {
        let client = RemoteBridgeClient::new();
        assert_eq!(*client.state_rx.borrow(), BridgeSessionState::Disconnected);
        assert_eq!(client.poll_video_nalu(), None);
    }

    #[test]
    fn custom_rates_and_pause_intent_survive_reconnection() {
        let client = RemoteBridgeClient::new();
        client.update_stream_rates(37, 3500).unwrap();
        assert!(client.update_stream_rates(0, 3500).is_err());
        let (_sender, _receiver, address) = test_host(&client);
        client.connect("host".into(), address.to_string());
        client.set_media_paused(true);
        let original = client.active_session_id.load(Relaxed);
        *client.connected_at.lock().unwrap() = Some(Instant::now() - Duration::from_secs(13));
        client.host_stats.apply_host_telemetry(0.0, 0.0, 0.0, 0);
        client.maybe_reconnect();
        assert_eq!(
            client.active_session_id.load(Relaxed),
            original,
            "paused before first frame stays connected with fresh telemetry"
        );
        client.host_stats.reset();
        client.maybe_reconnect();
        assert_ne!(client.active_session_id.load(Relaxed), original);
        assert_eq!(*client.stream_rates.lock().unwrap(), (37, 3500));
        assert!(client.host_stats.media_pause.is_paused());
        client.disconnect();
        assert!(!client.desired_media_paused.load(Relaxed));
    }

    #[test]
    fn telemetry_defaults_are_zero_not_marketing_numbers() {
        let telemetry = BridgeTelemetry::default();
        assert_eq!(telemetry.fps, 0.0);
        assert_eq!(telemetry.latency_ms, 0.0);
        assert_eq!(telemetry.video_bitrate_kbps, 0);
    }

    #[test]
    fn bridge_devices_choose_lan_then_p2p_then_relay() {
        let mut cache = remote_core::discovery::DiscoveryPeerCache::new("group", "android");
        let announcement = |scope, control_port| DiscoveryAnnouncement {
            network_name: "group".to_string(),
            device_id: "host-a".to_string(),
            display_name: "Host A".to_string(),
            control_port,
            virtual_ip: None,
            capabilities: DiscoveryCapabilities {
                can_stream: true,
                can_view: true,
                ..DiscoveryCapabilities::default()
            },
            scope,
            ttl: DEFAULT_PEER_TTL,
        };
        cache.apply_announcement(
            announcement(DiscoveryScope::Relay, 46001),
            "127.0.0.1:46000".parse().unwrap(),
            1,
        );
        cache.apply_announcement(
            announcement(DiscoveryScope::P2p, 45001),
            "127.0.0.1:45000".parse().unwrap(),
            2,
        );
        let devices = devices_from_snapshot(&cache.snapshot());
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].scope, "P2P Direct");
        assert_eq!(devices[0].endpoint, "127.0.0.1:45001");

        cache.apply_announcement(
            announcement(DiscoveryScope::Lan, 39271),
            "192.168.1.20:38117".parse().unwrap(),
            3,
        );
        let devices = devices_from_snapshot(&cache.snapshot());
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].scope, "LAN");
        assert_eq!(devices[0].endpoint, "192.168.1.20:39271");
    }

    #[test]
    fn network_members_are_visible_without_video_publishing() {
        let mut cache = remote_core::discovery::DiscoveryPeerCache::new("group", "local");
        for (id, port, files) in [("viewer", 0, false), ("file-server", 42000, true)] {
            cache.apply_announcement(
                DiscoveryAnnouncement {
                    network_name: "group".into(),
                    device_id: id.into(),
                    display_name: id.into(),
                    control_port: port,
                    virtual_ip: None,
                    capabilities: DiscoveryCapabilities {
                        can_view: true,
                        file_transfer: files,
                        clipboard_sync: files,
                        ..DiscoveryCapabilities::default()
                    },
                    scope: DiscoveryScope::Lan,
                    ttl: DEFAULT_PEER_TTL,
                },
                "192.168.1.10:38117".parse().unwrap(),
                1,
            );
        }
        let devices = devices_from_snapshot(&cache.snapshot());
        assert_eq!(devices.len(), 2);
        let viewer = devices.iter().find(|d| d.device_id == "viewer").unwrap();
        assert!(viewer.can_view);
        assert!(!viewer.can_stream);
        assert!(!viewer.has_service);
        let files = devices
            .iter()
            .find(|d| d.device_id == "file-server")
            .unwrap();
        assert!(files.has_service && files.file_transfer && files.clipboard_sync);
        assert!(!files.can_stream);
        let json = serde_json::to_value(files).unwrap();
        assert_eq!(json["file_transfer"], true);
        assert_eq!(json["can_stream"], false);
    }

    #[test]
    fn legacy_mesh_route_does_not_reappear_as_a_network_member() {
        let mut cache = remote_core::discovery::DiscoveryPeerCache::new("group", "local");
        cache.apply_announcement(
            DiscoveryAnnouncement {
                network_name: "group".into(),
                device_id: "legacy".into(),
                display_name: "Legacy".into(),
                control_port: 40000,
                virtual_ip: None,
                capabilities: DiscoveryCapabilities::all_interactive(),
                scope: DiscoveryScope::Mesh,
                ttl: DEFAULT_PEER_TTL,
            },
            "10.0.0.5:38117".parse().unwrap(),
            1,
        );
        assert!(devices_from_snapshot(&cache.snapshot()).is_empty());
    }

    #[test]
    fn keyframes_require_an_irap_header_not_just_a_start_code() {
        for prefix in [&[0, 0, 1][..], &[0, 0, 0, 1][..]] {
            for nal_type in 0..64 {
                let mut nalu = prefix.to_vec();
                nalu.extend_from_slice(&[nal_type << 1, 1, 42]);
                assert_eq!(hevc_keyframe(&nalu), (16..=21).contains(&nal_type));
            }
        }
        assert!(!hevc_keyframe(&[0, 0, 1, 38]));
        assert!(!hevc_keyframe(&[0, 0, 1, 38, 0]));
        assert!(!hevc_keyframe(&[0, 0, 1, 0x80 | 38, 1]));
        assert!(hevc_keyframe(&[0, 0, 1, 64, 1, 42, 0, 0, 1, 38, 1]));
    }

    #[test]
    fn session_ids_skip_zero_without_repeating_one_at_wrap() {
        let client = RemoteBridgeClient::new();
        client.next_session_id.store(u32::MAX, Relaxed);
        assert_eq!(client.next_session(), u32::MAX);
        assert_eq!(client.next_session(), 1);
        assert_eq!(client.next_session(), 2);
    }

    fn test_host(
        client: &RemoteBridgeClient,
    ) -> (
        remote_core::net::UdpSender,
        mpsc::UnboundedReceiver<(ControlMessage, SocketAddr)>,
        SocketAddr,
    ) {
        let host = client
            .runtime()
            .block_on(UdpMultiplexer::bind("127.0.0.1:0"))
            .unwrap();
        let address = host.local_addr().unwrap();
        let (sender, receiver) = host.split();
        let reply = sender.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        client.runtime().spawn(async move {
            let mut close_seen = std::collections::HashSet::new();
            while let Ok(remote_core::net::MultiplexedPacket::Control(message, peer)) =
                receiver.recv().await
            {
                let response = match &message {
                    ControlMessage::Session(command) => match **command {
                        SessionCommand::Open {
                            connection_id,
                            version,
                        } => Some(SessionCommand::Opened {
                            connection_id,
                            version,
                            max_subscriptions: 8,
                            files: false,
                            clipboard: false,
                            window_capture: true,
                        }),
                        SessionCommand::Subscribe(ref request) => {
                            Some(SessionCommand::Subscribed {
                                id: request.id,
                                audio_owner: Some(request.id),
                                supports_input: true,
                            })
                        }
                        SessionCommand::SetActivity {
                            id,
                            revision,
                            video,
                            audio,
                        } => Some(SessionCommand::Activity {
                            id,
                            revision,
                            video,
                            audio,
                        }),
                        SessionCommand::Close { connection_id }
                            if !close_seen.insert(connection_id) =>
                        {
                            Some(SessionCommand::Closed {
                                connection_id,
                                reason: String::new(),
                            })
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(response) = response {
                    reply
                        .send_control(&ControlMessage::Session(Box::new(response)), peer)
                        .await
                        .unwrap();
                }
                let _ = tx.send((message, peer));
            }
        });
        (sender, rx, address)
    }

    async fn next_control(
        receiver: &mut mpsc::UnboundedReceiver<(ControlMessage, SocketAddr)>,
    ) -> (ControlMessage, SocketAddr) {
        tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("control deadline")
            .unwrap()
    }
    async fn next_subscribe(
        receiver: &mut mpsc::UnboundedReceiver<(ControlMessage, SocketAddr)>,
    ) -> (SubscriptionRequest, SocketAddr) {
        loop {
            let (message, peer) = next_control(receiver).await;
            if let ControlMessage::Session(command) = message {
                if let SessionCommand::Subscribe(request) = *command {
                    return (request, peer);
                }
            }
        }
    }

    #[test]
    fn paused_session_uses_sparse_shared_keepalive_and_stops_input_grants() {
        let client = RemoteBridgeClient::new();
        let (_, mut receiver, address) = test_host(&client);
        client.connect("idle".into(), address.to_string());
        client.set_media_paused(true);
        client.runtime().block_on(async {
            loop { let (message,_)=next_control(&mut receiver).await;
                if matches!(message,ControlMessage::Session(ref c) if matches!(**c,SessionCommand::SetActivity{video:false,..})) {break;}
            }
            let deadline=tokio::time::Instant::now()+Duration::from_millis(1100);
            let mut pings=0;
            while let Ok(Some((message,_)))=tokio::time::timeout_at(deadline,receiver.recv()).await {
                match message {
                    ControlMessage::Ping{..}=>pings+=1,
                    ControlMessage::Session(c)=>assert!(!matches!(*c,SessionCommand::RequestInputBinding{..}|SessionCommand::RealtimeInput(_))),
                    _=>{}
                }
            }
            assert!(pings<=1,"shared session has a five-second keepalive");
        });
        assert!(!client.host_stats.media_pause.is_pending());
        client.disconnect();
    }

    #[test]
    fn replacing_session_stops_old_tasks_and_disconnect_reaches_host() {
        let client = RemoteBridgeClient::new();
        let (_sender, mut receiver, address) = test_host(&client);
        let endpoint = address.to_string();
        client.connect("first".into(), endpoint.clone());
        let (_, old_address) = client.runtime().block_on(next_subscribe(&mut receiver));
        let old_tasks = {
            let live = client.live.lock().unwrap();
            let live = live.as_ref().unwrap();
            [
                live._decode_pump.abort_handle(),
                live._audio_pump.abort_handle(),
                live._telemetry.abort_handle(),
            ]
        };
        client
            .audio_queue
            .lock()
            .unwrap()
            .push_back(EncodedAudioPacket {
                data: vec![1],
                sample_rate_hz: 48_000,
                channels: 2,
                pts_us: 0,
            });
        client.connect("second".into(), endpoint);
        assert!(old_tasks.iter().all(|task| task.is_finished()));
        assert_eq!(client.poll_audio_packet(), None);
        let mut saw_stop = false;
        let new_address = client.runtime().block_on(async {
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let (message, source) = next_control(&mut receiver).await;
                    match message {
                        ControlMessage::Session(ref c)
                            if matches!(**c, SessionCommand::Close { .. }) =>
                        {
                            assert_eq!(source, old_address);
                            saw_stop = true;
                        }
                        ControlMessage::Session(ref c)
                            if matches!(**c, SessionCommand::Subscribe(_)) =>
                        {
                            break source;
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("old Close followed by new Subscribe")
        });
        assert!(saw_stop);
        client.disconnect();
        client.runtime().block_on(async {
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let (message, source) = next_control(&mut receiver).await;
                    if matches!(message, ControlMessage::Session(ref c) if matches!(**c,SessionCommand::Close{..})) {
                        assert_eq!(source, new_address);
                        break;
                    }
                }
            })
            .await
            .expect("disconnect Close")
        });
        assert_eq!(*client.state_rx.borrow(), BridgeSessionState::Disconnected);
        assert!(client.last_connect.lock().unwrap().is_none());
        assert_eq!(client.poll_video_nalu(), None);
        assert_eq!(client.telemetry_rx.borrow().transport_health_score, 0.0);
        client.refresh_telemetry_from_stats();
        assert_eq!(client.telemetry_rx.borrow().transport_health_score, 0.0);
    }

    #[test]
    fn streaming_waits_for_video_and_overflow_recovers_at_keyframe() {
        let client = RemoteBridgeClient::new();
        let (sender, mut receiver, address) = test_host(&client);
        client.connect("test-host".into(), address.to_string());
        let (request, target) = client.runtime().block_on(next_subscribe(&mut receiver));
        let session_id = request.id;
        assert_eq!(*client.state_rx.borrow(), BridgeSessionState::Connecting);
        let send_video = |sequence_number, keyframe| {
            let packet = protocol::RtpPacket {
                header: protocol::RtpHeader {
                    version: 2,
                    payload_type: 96,
                    sequence_number,
                    timestamp: 1,
                    ssrc: session_id,
                },
                payload: vec![0, 0, 0, 1, if keyframe { 38 } else { 2 }, 1, 42],
            };
            client
                .runtime()
                .block_on(sender.send_data(
                    &remote_core::media_plane::rtp_to_realtime_data(&packet).unwrap(),
                    target,
                ))
                .unwrap();
        };
        // Initial P-frame is unusable, and cannot declare the session streaming.
        send_video(0, false);
        send_video(1, true);
        client.runtime().block_on(async {
            let mut state = client.state_rx.clone();
            tokio::time::timeout(
                Duration::from_secs(1),
                state.wait_for(|state| *state == BridgeSessionState::Streaming),
            )
            .await
            .unwrap()
            .unwrap();
        });
        // Eight unread frames plus one overflow: discard the broken prediction chain.
        for sequence in 2..=9 {
            send_video(sequence, false);
        }
        client.runtime().block_on(async {
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    // Shared receiver recovery can also request the missing
                    // initial IDR. Wait for the overflow state as well as a
                    // request, rather than assuming exactly one startup request.
                    if matches!(
                        next_control(&mut receiver).await.0,
                        ControlMessage::RequestKeyframe { .. }
                    ) && client.nalu_queue.lock().unwrap().is_empty()
                    {
                        break;
                    }
                }
            })
            .await
            .expect("overflow requests a recovery keyframe")
        });
        assert!(client.nalu_queue.lock().unwrap().is_empty());
        send_video(10, false);
        send_video(11, true);
        client.runtime().block_on(async {
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if !client.nalu_queue.lock().unwrap().is_empty() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
        });
        assert!(client.poll_video_nalu().unwrap().keyframe);
        assert!(client.poll_video_nalu().is_none());
        client.disconnect();
    }

    #[test]
    fn fresh_telemetry_does_not_hide_video_timeout_and_disconnect_cancels_retry() {
        let client = RemoteBridgeClient::new();
        let (_sender, _receiver, address) = test_host(&client);
        client.connect("silent-host".into(), address.to_string());
        let original = client.active_session_id.load(Relaxed);
        client
            .host_stats
            .apply_host_telemetry(60.0, 1.0, 0.0, 20_000);
        *client.connected_at.lock().unwrap() = Some(Instant::now() - Duration::from_secs(13));
        client.maybe_reconnect();
        assert_ne!(client.active_session_id.load(Relaxed), original);
        assert_eq!(*client.state_rx.borrow(), BridgeSessionState::Reconnecting);
        client.disconnect();
        client.maybe_reconnect();
        assert_eq!(client.active_session_id.load(Relaxed), 0);
        assert_eq!(*client.state_rx.borrow(), BridgeSessionState::Disconnected);
    }

    #[test]
    fn invalid_connection_replaces_live_session_without_stale_media() {
        let client = RemoteBridgeClient::new();
        let (_sender, _receiver, address) = test_host(&client);
        client.connect("host".into(), address.to_string());
        client.connect("invalid".into(), "not-an-endpoint".into());
        assert_eq!(*client.state_rx.borrow(), BridgeSessionState::Error);
        assert!(client.live.lock().unwrap().is_none());
        assert!(client.last_connect.lock().unwrap().is_none());
        assert!(client.active_target.read().unwrap().is_none());
        assert_eq!(client.active_session_id.load(Relaxed), 0);
    }

    #[test]
    fn idle_desktop_with_fresh_zero_fps_telemetry_does_not_reconnect() {
        let client = RemoteBridgeClient::new();
        let (_sender, _receiver, address) = test_host(&client);
        client.connect("idle-host".into(), address.to_string());
        let original = client.active_session_id.load(Relaxed);
        *client
            .live
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .video_activity
            .lock()
            .unwrap() = Some(Instant::now() - Duration::from_secs(13));
        client.state_tx.send_replace(BridgeSessionState::Streaming);
        client.host_stats.apply_host_telemetry(0.0, 0.0, 0.0, 0);
        client.maybe_reconnect();
        assert_eq!(client.active_session_id.load(Relaxed), original);
        assert_eq!(*client.state_rx.borrow(), BridgeSessionState::Streaming);
        client.disconnect();
    }
    async fn next_input(
        rx: &mut mpsc::UnboundedReceiver<(ControlMessage, SocketAddr)>,
        event: bool,
    ) -> protocol::realtime_input::RealtimeInput {
        loop {
            let (message, _) = next_control(rx).await;
            assert!(!matches!(
                message,
                ControlMessage::Input(_)
                    | ControlMessage::StartStream { .. }
                    | ControlMessage::StopStream
            ));
            if let ControlMessage::Session(c) = message {
                if let SessionCommand::RealtimeInput(packet) = *c {
                    if matches!(
                        packet.payload,
                        protocol::realtime_input::InputPayload::Event(_)
                    ) == event
                    {
                        return packet;
                    }
                }
            }
        }
    }

    fn grant_manual(
        client: &RemoteBridgeClient,
        sender: &remote_core::net::UdpSender,
        rx: &mut mpsc::UnboundedReceiver<(ControlMessage, SocketAddr)>,
        epoch: u8,
    ) -> protocol::realtime_input::InputBinding {
        client.runtime().block_on(async {
            let (binding, request, peer) = loop {
                let (message, peer) = next_control(rx).await;
                if let ControlMessage::Session(c) = message {
                    if let SessionCommand::RequestInputBinding {
                        connection_id,
                        id,
                        source_revision,
                        request,
                    } = *c
                    {
                        if connection_id
                            != client.live.lock().unwrap().as_ref().unwrap().connection.id
                        {
                            continue;
                        }
                        break (
                            protocol::realtime_input::InputBinding {
                                connection_id,
                                subscription_id: id,
                                source_revision,
                                epoch: [epoch; 16],
                            },
                            request,
                            peer,
                        );
                    }
                }
            };
            sender
                .send_control(
                    &ControlMessage::Session(Box::new(SessionCommand::InputBinding {
                        binding,
                        request,
                        host_time_ms: remote_core::session_crypto::now_unix_ms(),
                    })),
                    peer,
                )
                .await
                .unwrap();
            // An existing epoch can remain ready while this reply is still
            // in flight. Do not deliver the next gesture until the exact
            // replacement binding has been committed by the actor.
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if client
                        .live
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .connection
                        .input_binding(binding.subscription_id, binding.source_revision)
                        == Some(binding)
                    {
                        break;
                    }
                    // The first queued request may have been superseded by
                    // the actor's periodic request. Serve subsequent requests
                    // within the same deadline, keeping the exact epoch check.
                    if let Ok(Some((ControlMessage::Session(command), target))) =
                        tokio::time::timeout(Duration::from_millis(1), rx.recv()).await
                        && let SessionCommand::RequestInputBinding { connection_id, id, source_revision, request } = *command
                        && connection_id == binding.connection_id
                        && id == binding.subscription_id
                        && source_revision == binding.source_revision
                    {
                        sender.send_control(&ControlMessage::Session(Box::new(SessionCommand::InputBinding {
                            binding, request, host_time_ms: remote_core::session_crypto::now_unix_ms(),
                        })), target).await.unwrap();
                    }
                }
            })
            .await
            .unwrap();
            binding
        })
    }

    #[test]
    fn workspace_touch_uses_shared_grant_and_releases_cancelled_drag() {
        use protocol::realtime_input::{InputBinding, InputPayload};
        let client = RemoteBridgeClient::new();
        let (sender, mut rx, address) = test_host(&client);
        let directory = std::env::temp_dir().join(format!("rp-workspace-touch-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&directory).unwrap();
        client.runtime().block_on(async {
            let workspace = workspace_bridge::MobileWorkspace::connect(address, directory.clone()).await.unwrap();
            workspace.command(&serde_json::to_string(&SessionCommand::Subscribe(SubscriptionRequest {
                id: 256, source: CaptureSource::Display(1), width: 1280, height: 720,
                fps: 60, bitrate_kbps: 20_000, audio: false,
            })).unwrap()).await.unwrap();
            let (binding, request, peer) = loop {
                let (message, peer) = next_control(&mut rx).await;
                if let ControlMessage::Session(command) = message
                    && let SessionCommand::RequestInputBinding { connection_id, id, source_revision, request } = *command {
                    break (InputBinding { connection_id, subscription_id: id, source_revision, epoch: [9; 16] }, request, peer);
                }
            };
            sender.send_control(&ControlMessage::Session(Box::new(SessionCommand::InputBinding {
                binding, request, host_time_ms: remote_core::session_crypto::now_unix_ms(),
            })), peer).await.unwrap();
            let held = next_input(&mut rx, false).await;
            assert_eq!(held.binding, binding);
            tokio::time::timeout(Duration::from_secs(1), async {
                while !workspace.events().contains("Subscribed") { tokio::time::sleep(Duration::from_millis(1)).await; }
            }).await.unwrap();
            let surface = |phase| serde_json::json!({"id":256,"token":1,"phase":phase}).to_string();
            let gesture = |action, x| serde_json::json!({"id":256,"token":1,"event":{"Touch":{
                "action":action,"pointer_id":7,"normalized_x":x,"normalized_y":0.5,"pressure":1.0}}}).to_string();
            workspace.surface(&surface("begin")).unwrap();
            workspace.touch(&gesture("Down", 0.1)).unwrap(); // No frame: cannot accumulate a press.
            workspace.surface(&surface("ready")).unwrap();
            workspace.touch(&gesture("Move", 0.8)).unwrap();
            workspace.touch(&gesture("Down", 0.1)).unwrap();
            workspace.touch(&gesture("Move", 0.8)).unwrap();
            let first = next_input(&mut rx, true).await;
            assert!(matches!(first.payload, InputPayload::Event(protocol::InputEvent::MouseMoveAbsolute { x, .. }) if x < 10000));
            let drag = next_input(&mut rx, true).await;
            assert_eq!(drag.binding, binding);
            assert!(matches!(drag.payload, InputPayload::Event(protocol::InputEvent::MouseDown(0))));
            workspace.surface(&surface("cancel")).unwrap();
            workspace.touch(&gesture("Up", 0.8)).unwrap();
            loop {
                let packet = next_input(&mut rx, false).await;
                if matches!(packet.payload, InputPayload::Release) { assert_eq!(packet.binding, binding); break; }
            }
            let held = next_input(&mut rx, false).await;
            assert!(matches!(held.payload, InputPayload::Held(held) if held.is_empty()));
            // The UI producer now goes silent while the native transport keeps running.
            workspace.touch(&gesture("Down", 0.1)).unwrap();
            workspace.touch(&gesture("Move", 0.8)).unwrap();
            tokio::time::timeout(Duration::from_millis(650), async {
                loop {
                    let packet = next_input(&mut rx, false).await;
                    if matches!(packet.payload, InputPayload::Release) {
                        assert_eq!(packet.binding, binding); break;
                    }
                }
            }).await.expect("native watchdog must release a silent UI producer");
            workspace.surface(&surface("alive")).unwrap();
            workspace.touch(&gesture("Up", 0.8)).unwrap();
            let held = next_input(&mut rx, false).await;
            assert!(matches!(held.payload, InputPayload::Held(held) if held.is_empty()));
            let raw = serde_json::json!({"Input":{"id":256,"event":{"Touch":{
                "action":"Down","pointer_id":7,"normalized_x":0.5,"normalized_y":0.5,"pressure":1.0}}}});
            assert!(workspace.command(&raw.to_string()).await.is_err());
            drop(workspace);
        });
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn manual_entry_requires_binding_and_video_then_preserves_chords_and_recovers_lost_up() {
        use protocol::realtime_input::{InputLease, InputPayload};
        let client = RemoteBridgeClient::new();
        let (sender, mut rx, address) = test_host(&client);
        client.connect("manual".into(), address.to_string());
        client.send_virtual_key("ctrl", true); // no grant: must not accumulate Ctrl.
        let (request, peer) = client.runtime().block_on(next_subscribe(&mut rx));
        let binding = grant_manual(&client, &sender, &mut rx, 1);
        client.send_virtual_key("alt", true); // no video: must not accumulate Alt.
        let packet = protocol::RtpPacket {
            header: protocol::RtpHeader {
                version: 2,
                payload_type: 96,
                sequence_number: 0,
                timestamp: 1,
                ssrc: request.id,
            },
            payload: vec![0, 0, 1, 38, 1, 42],
        };
        client
            .runtime()
            .block_on(sender.send_data(
                &remote_core::media_plane::rtp_to_realtime_data(&packet).unwrap(),
                peer,
            ))
            .unwrap();
        client.runtime().block_on(async {
            let mut state = client.state_rx.clone();
            tokio::time::timeout(
                Duration::from_secs(1),
                state.wait_for(|s| *s == BridgeSessionState::Streaming),
            )
            .await
            .unwrap()
            .unwrap();
        });
        client.send_virtual_key("A", true);
        let modifier = client.runtime().block_on(next_input(&mut rx, true));
        assert!(matches!(
            modifier.payload,
            InputPayload::Event(protocol::InputEvent::ModifiersChanged(1))
        ));
        let key = client.runtime().block_on(next_input(&mut rx, true));
        assert!(matches!(
            key.payload,
            InputPayload::Event(protocol::InputEvent::Key {
                pressed: true,
                modifiers: 1,
                ..
            })
        ));
        assert_eq!(key.binding, binding);
        let mut host = InputLease::new(binding, modifier.created_host_ms);
        assert!(host.accept(&modifier, modifier.created_host_ms).is_some());
        assert!(host.accept(&key, key.created_host_ms).is_some());
        assert!(
            host.accept(&key, key.created_host_ms).is_none(),
            "duplicate cannot repeat"
        );
        let held_down = loop {
            let p = client.runtime().block_on(next_input(&mut rx, false));
            if matches!(&p.payload,InputPayload::Held(h) if !h.is_empty()) {
                break p;
            }
        };
        let mut lost_down = InputLease::new(binding, held_down.created_host_ms);
        assert!(
            lost_down
                .accept(&held_down, held_down.created_host_ms)
                .unwrap()
                .is_empty(),
            "snapshots cannot recreate a lost down"
        );
        host.accept(&held_down, held_down.created_host_ms).unwrap();
        client.send_virtual_key("A", false);
        // Drop both key-up and temporary modifier-up; an empty matching snapshot releases them.
        let held = loop {
            let p = client.runtime().block_on(next_input(&mut rx, false));
            if matches!(&p.payload,InputPayload::Held(h) if h.is_empty()) {
                break p;
            }
        };
        let effects = host.accept(&held, held.created_host_ms).unwrap();
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, protocol::InputEvent::Key { pressed: false, .. }))
        );
        assert!(effects.contains(&protocol::InputEvent::ModifiersChanged(0)));
        // A missing key-down is never recreated by its matching held snapshot.
        let mut lost_down = InputLease::new(binding, held.created_host_ms);
        assert!(
            lost_down
                .accept(&held, held.created_host_ms)
                .unwrap()
                .is_empty()
        );
        client.disconnect();
    }

    #[test]
    fn reconnect_and_stop_do_not_replay_old_holds_and_close_retries_loss() {
        use protocol::realtime_input::{InputLease, InputPayload};
        let client = RemoteBridgeClient::new();
        let (sender, mut rx, address) = test_host(&client);
        client.connect("manual".into(), address.to_string());
        let old = grant_manual(&client, &sender, &mut rx, 1);
        *client
            .live
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .video_activity
            .lock()
            .unwrap() = Some(Instant::now());
        client.send_virtual_key("ctrl", true);
        let down = client.runtime().block_on(next_input(&mut rx, true));
        client.connect("manual".into(), address.to_string());
        let new = grant_manual(&client, &sender, &mut rx, 2);
        assert_ne!(old.connection_id, new.connection_id);
        *client
            .live
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .video_activity
            .lock()
            .unwrap() = Some(Instant::now());
        client.send_virtual_key("c", true);
        let key = client.runtime().block_on(next_input(&mut rx, true));
        assert_eq!(key.binding, new);
        assert!(matches!(
            key.payload,
            InputPayload::Event(protocol::InputEvent::Key { modifiers: 0, .. })
        ));
        let mut host = InputLease::new(new, key.created_host_ms);
        assert!(host.accept(&down, key.created_host_ms).is_none());
        assert!(host.accept(&key, key.created_host_ms).is_some());
        client.disconnect();
        let mut closes = 0;
        while let Ok((message, _)) = rx.try_recv() {
            if matches!(message,ControlMessage::Session(ref c) if matches!(**c,SessionCommand::Close{connection_id} if connection_id==new.connection_id))
            {
                closes += 1;
            }
        }
        assert!(
            closes >= 2,
            "first Close was lost; actor must retry before stopping"
        );
    }

    #[test]
    fn manual_ui_input_never_waits_for_connection_lifecycle_or_live_lock() {
        for hold_lifecycle in [true, false] {
            let client = Arc::new(RemoteBridgeClient::new());
            let lifecycle = hold_lifecycle.then(|| client.lifecycle.lock().unwrap());
            let live = (!hold_lifecycle).then(|| client.live.lock().unwrap());
            let worker_client = client.clone();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                ready_tx.send(()).unwrap();
                let token = worker_client.input_intent_token();
                let refreshed = worker_client.refresh_input_intent(1);
                worker_client.handle_touch_input(TouchAction::Cancel, 1, 0.0, 0.0, 0.0);
                worker_client.send_virtual_key("ctrl", false);
                done_tx.send((token, refreshed)).unwrap();
            });
            ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let result = done_rx.recv_timeout(Duration::from_millis(200));
            // Always unblock and join even on failure, so a regression cannot
            // strand the test worker holding a client runtime.
            drop(live);
            drop(lifecycle);
            worker.join().unwrap();
            assert_eq!(result.unwrap(), (0, false));
            assert_eq!(client.manual_input_owner.load(Relaxed), 0);
        }
    }

    #[test]
    fn stale_busy_heartbeat_cannot_revoke_a_new_session_or_epoch() {
        let client = RemoteBridgeClient::new();
        let (sender, mut rx, address) = test_host(&client);
        client.connect("scope".into(), address.to_string());
        grant_manual(&client, &sender, &mut rx, 1);
        *client.live.lock().unwrap().as_ref().unwrap().video_activity.lock().unwrap() = Some(Instant::now());
        let mut old = client.input_intent_token();
        assert_eq!(client.manual_input_owner.load(Relaxed), 0);
        {
            let _busy = client.lifecycle.lock().unwrap();
            assert!(!client.refresh_input_intent(old));
        }
        assert_eq!(client.manual_input_revoked.load(Relaxed), 0, "idle failure must not poison a future gesture");
        client.handle_touch_input(TouchAction::Down, 1, 0.1, 0.1, 1.0);
        assert!(client.refresh_input_intent(old), "a fresh Down after idle contention stays healthy");
        for reconnect in [false, true] {
            if reconnect { client.connect("scope".into(), address.to_string()); }
            grant_manual(&client, &sender, &mut rx, 2);
            *client.live.lock().unwrap().as_ref().unwrap().video_activity.lock().unwrap() = Some(Instant::now());
            client.handle_touch_input(TouchAction::Down, 1, 0.1, 0.1, 1.0);
            let current = client.input_intent_token();
            assert_ne!(current, old);
            {
                let _busy = client.lifecycle.lock().unwrap();
                assert_eq!(client.input_intent_token(), 0); // query is read-only
                assert!(!client.refresh_input_intent(old));
            }
            assert!(client.refresh_input_intent(current));
            assert!(client.touch_tracker.lock().unwrap().has_active_pointers());
            assert_eq!(client.manual_input_intent.lock().unwrap().as_ref().unwrap().2, current);
            old = current;
        }
        client.disconnect();
    }

    #[test]
    fn manual_direct_touch_expiry_and_epoch_change_discard_unheld_tail() {
        let client = RemoteBridgeClient::new();
        let (sender, mut rx, address) = test_host(&client);
        client.connect("touch-expiry".into(), address.to_string());
        grant_manual(&client, &sender, &mut rx, 1);
        *client.live.lock().unwrap().as_ref().unwrap().video_activity.lock().unwrap() = Some(Instant::now());
        for heartbeat_first in [false, true] {
            client.handle_touch_input(TouchAction::Down, 1, 0.1, 0.1, 1.0);
            assert!(client.touch_tracker.lock().unwrap().has_active_pointers());
            // Simulate a stalled frontend without delaying unrelated actor IO.
            client.manual_input_intent.lock().unwrap().as_mut().unwrap().1 = Instant::now() - Duration::from_millis(400);
            if heartbeat_first { assert!(!client.refresh_input_intent(client.input_intent_token())); }
            client.handle_touch_input(TouchAction::Up, 1, 0.1, 0.1, 0.0);
            assert!(!client.touch_tracker.lock().unwrap().has_active_pointers());
            assert!(client.manual_input_intent.lock().unwrap().is_none());
        }
        client.handle_touch_input(TouchAction::Down, 1, 0.1, 0.1, 1.0);
        let token = client.input_intent_token();
        {
            let _busy = client.lifecycle.lock().unwrap();
            client.handle_touch_input(TouchAction::Cancel, 1, 0.0, 0.0, 0.0);
        }
        assert!(!client.refresh_input_intent(token), "a dropped Cancel must invalidate the old gesture");
        assert!(!client.touch_tracker.lock().unwrap().has_active_pointers());
        client.handle_touch_input(TouchAction::Down, 1, 0.1, 0.1, 1.0);
        grant_manual(&client, &sender, &mut rx, 2);
        client.handle_touch_input(TouchAction::Up, 1, 0.1, 0.1, 0.0);
        assert!(!client.touch_tracker.lock().unwrap().has_active_pointers());
        assert!(client.manual_input_intent.lock().unwrap().is_none());
        // Inspect the real outbound input stream: no delayed left/right click.
        client.runtime().block_on(async {
            let until = tokio::time::Instant::now() + Duration::from_millis(120);
            while let Ok(packet) = tokio::time::timeout_at(until, next_input(&mut rx, true)).await {
                assert!(!matches!(packet.payload, protocol::realtime_input::InputPayload::Event(protocol::InputEvent::MouseDown(_) | protocol::InputEvent::MouseUp(_))));
            }
        });
        client.disconnect();
    }

    #[test]
    fn manual_touch_binds_drag_and_pause_discards_cancelled_tail() {
        use protocol::realtime_input::InputPayload;
        let client = RemoteBridgeClient::new();
        let (sender, mut rx, address) = test_host(&client);
        client.connect("touch".into(), address.to_string());
        let binding = grant_manual(&client, &sender, &mut rx, 1);
        *client
            .live
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .video_activity
            .lock()
            .unwrap() = Some(Instant::now());
        client.handle_touch_input(TouchAction::Down, 1, 0.1, 0.1, 1.0);
        client.handle_touch_input(TouchAction::Move, 1, 0.8, 0.8, 1.0);
        let drag = loop {
            let p = client.runtime().block_on(next_input(&mut rx, true));
            if matches!(
                p.payload,
                InputPayload::Event(protocol::InputEvent::MouseDown(0))
            ) {
                break p;
            }
        };
        assert_eq!(drag.binding, binding);
        let intent=client.input_intent_token();assert_ne!(intent,0);
        assert!(!client.refresh_input_intent(intent^1),"wrong session cannot renew a gesture");
        for index in 0..5 {
            std::thread::sleep(Duration::from_millis(90));
            assert!(client.refresh_input_intent(intent),"a healthy manual viewport must preserve long drag");
            if index==2 {grant_manual(&client,&sender,&mut rx,1);}
        }
        std::thread::sleep(Duration::from_millis(330));
        grant_manual(&client,&sender,&mut rx,1);
        assert!(!client.refresh_input_intent(intent),"late UI wake must clear its old drag");
        assert!(!client.touch_tracker.lock().unwrap().has_active_pointers());
        client.set_media_paused(true);
        assert!(
            !client
                .live
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .connection
                .input_ready(binding.subscription_id, 0)
        );
        client.set_media_paused(false);
        let resumed = grant_manual(&client, &sender, &mut rx, 2);
        *client
            .live
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .video_activity
            .lock()
            .unwrap() = Some(Instant::now());
        client.handle_touch_input(TouchAction::Up, 1, 0.8, 0.8, 0.0);
        let held = loop {
            let p = client.runtime().block_on(next_input(&mut rx, false));
            if p.binding == resumed {
                break p;
            }
        };
        assert!(matches!(held.payload,InputPayload::Held(h) if h.is_empty()));
        client.disconnect();
    }
}
