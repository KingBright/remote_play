pub mod touch_mapper;

#[cfg(feature = "android")]
pub mod jni_bridge;

#[cfg(feature = "wasm")]
pub mod wasm_bridge;

use protocol::{ControlMessage, TouchAction};
use remote_core::client_session::{
    AudioIngressEvent, ClientSessionReceiverConfig, SharedHostStats, spawn_client_session_receiver,
};
use remote_core::discovery::{
    DEFAULT_PEER_TTL, DiscoveryAnnouncement, DiscoveryCapabilities, DiscoveryPeerSnapshot,
    DiscoveryRuntimeConfig, DiscoveryScope, run_discovery_runtime,
};
use remote_core::mesh::{AppPrivateMeshConfigStore, MeshConfig, default_app_private_mesh_dir};
use remote_core::net::{UdpMultiplexer, UdpSender};
use remote_core::pairing_qr::parse_pairing_qr;
use remote_core::session_crypto::{
    SessionCrypto, load_session_psk, mac_session_hello, now_unix_ms, random_bytes_16,
};
use remote_core::stats::Statistics;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::{broadcast, mpsc, watch};
use touch_mapper::{TouchMode, TouchStateTracker};

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
    control_tx: mpsc::UnboundedSender<ControlMessage>,
    target: SocketAddr,
    udp_sender: UdpSender,
    video_activity: Arc<Mutex<Option<Instant>>>,
    _receiver: tokio::task::JoinHandle<()>,
    _egress: tokio::task::JoinHandle<()>,
    _decode_pump: tokio::task::JoinHandle<()>,
    _audio_pump: tokio::task::JoinHandle<()>,
    _heartbeat: tokio::task::JoinHandle<()>,
    _telemetry: tokio::task::JoinHandle<()>,
}

impl LiveSession {
    async fn stop(self) {
        let tasks = [
            self._receiver,
            self._egress,
            self._decode_pump,
            self._audio_pump,
            self._heartbeat,
            self._telemetry,
        ];
        for task in &tasks {
            task.abort();
        }
        // Wait for in-flight queue/stat writes before another session can start.
        for task in tasks {
            let _ = task.await;
        }
        // Send directly after egress has stopped; enqueue-then-abort can lose StopStream.
        let _ = tokio::time::timeout(
            Duration::from_millis(250),
            self.udp_sender
                .send_control(&ControlMessage::StopStream, self.target),
        )
        .await;
    }
}

pub struct RemoteBridgeClient {
    runtime: OnceLock<Runtime>,
    state_tx: watch::Sender<BridgeSessionState>,
    pub state_rx: watch::Receiver<BridgeSessionState>,
    telemetry_tx: watch::Sender<BridgeTelemetry>,
    pub telemetry_rx: watch::Receiver<BridgeTelemetry>,
    devices_tx: watch::Sender<Vec<BridgeDiscoveredDevice>>,
    pub devices_rx: watch::Receiver<Vec<BridgeDiscoveredDevice>>,
    touch_tracker: Arc<Mutex<TouchStateTracker>>,
    host_stats: Arc<SharedHostStats>,
    nalu_queue: Arc<Mutex<VecDeque<EncodedVideoNalu>>>,
    audio_queue: Arc<Mutex<VecDeque<EncodedAudioPacket>>>,
    live: Mutex<Option<LiveSession>>,
    lifecycle: Mutex<()>,
    last_connect: Mutex<Option<(String, String)>>,
    connected_at: Mutex<Option<Instant>>,
    active_target: Arc<RwLock<Option<String>>>,
    active_session_id: Arc<AtomicU32>,
    next_session_id: AtomicU32,
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
        let (devices_tx, devices_rx) = watch::channel(Vec::new());
        let client = Self {
            runtime: OnceLock::new(),
            state_tx,
            state_rx,
            telemetry_tx,
            telemetry_rx,
            devices_tx,
            devices_rx,
            touch_tracker: Arc::new(Mutex::new(TouchStateTracker::default())),
            host_stats: Arc::new(SharedHostStats::default()),
            nalu_queue: Arc::new(Mutex::new(VecDeque::with_capacity(8))),
            audio_queue: Arc::new(Mutex::new(VecDeque::with_capacity(16))),
            live: Mutex::new(None),
            lifecycle: Mutex::new(()),
            last_connect: Mutex::new(None),
            connected_at: Mutex::new(None),
            active_target: Arc::new(RwLock::new(None)),
            active_session_id: Arc::new(AtomicU32::new(0)),
            next_session_id: AtomicU32::new(1),
        };
        client.ensure_discovery();
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
        if let Ok(mut tracker) = self.touch_tracker.lock() {
            tracker.set_mode(mode);
        }
    }

    pub fn set_screen_bounds(&self, width: u16, height: u16) {
        if let Ok(mut tracker) = self.touch_tracker.lock() {
            tracker.set_bounds(width, height);
        }
    }

    pub fn handle_touch_input(
        &self,
        action: TouchAction,
        pointer_id: u32,
        norm_x: f32,
        norm_y: f32,
        pressure: f32,
    ) {
        let events = if let Ok(mut tracker) = self.touch_tracker.lock() {
            tracker.process_touch(action, pointer_id, norm_x, norm_y, pressure, Instant::now())
        } else {
            Vec::new()
        };

        if let Some(live) = self.live.lock().unwrap().as_ref() {
            for ev in events {
                let _ = live.control_tx.send(ControlMessage::Input(ev));
            }
        }
    }

    pub fn send_virtual_key(&self, key_name: &str, pressed: bool) {
        if let Some(ev) = touch_mapper::create_virtual_key_event(key_name, pressed)
            && let Some(live) = self.live.lock().unwrap().as_ref()
        {
            let _ = live.control_tx.send(ControlMessage::Input(ev));
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

    pub fn join_pairing_payload(&self, raw: &str) -> Result<String, String> {
        let payload = parse_pairing_qr(raw).map_err(|err| err.to_string())?;
        let store = AppPrivateMeshConfigStore::new(default_app_private_mesh_dir());
        let config = MeshConfig::from_invite_code(&payload.invite_code, "RemotePlay Android")
            .map_err(|err| err.to_string())?;
        store.save(&config).map_err(|err| err.to_string())?;
        Ok(format!(
            "Joined {} (control port {})",
            config.network_name, payload.control_port
        ))
    }

    fn ensure_discovery(&self) {
        let devices_tx = self.devices_tx.clone();
        self.runtime().spawn(async move {
            if let Err(err) = run_lan_discovery(devices_tx).await {
                eprintln!("Bridge discovery stopped: {err}");
            }
        });
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
        let host_stats = self.host_stats.clone();
        let nalu_queue = self.nalu_queue.clone();
        let active_session_id = self.active_session_id.clone();
        *self.last_connect.lock().unwrap() = Some((device_id.clone(), endpoint.clone()));
        *self.connected_at.lock().unwrap() = Some(Instant::now());
        let session_id = self.next_session();
        active_session_id.store(session_id, Relaxed);

        let audio_queue = self.audio_queue.clone();
        let result = self.runtime().block_on(async move {
            start_live_session(
                parsed,
                session_id,
                host_stats,
                nalu_queue,
                audio_queue,
                active_session_id,
                telemetry_tx,
                state_tx,
            )
            .await
        });

        match result {
            Ok(live) => {
                *self.live.lock().unwrap() = Some(live);
                self.devices_tx.send_replace(vec![BridgeDiscoveredDevice {
                    device_id,
                    display_name: endpoint.clone(),
                    endpoint,
                    scope: "LAN".to_string(),
                    can_stream: true,
                    online: true,
                }]);
            }
            Err(err) => {
                eprintln!("Bridge connect failed: {err}");
                self.stop_live();
                let _ = self.state_tx.send(BridgeSessionState::Error);
            }
        }
    }

    pub fn disconnect(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        self.stop_live();
        *self.last_connect.lock().unwrap() = None;
        let _ = self.state_tx.send(BridgeSessionState::Disconnected);
    }

    fn stop_live(&self) {
        if let Some(live) = self.live.lock().unwrap().take() {
            self.runtime().block_on(live.stop());
        }
        self.active_session_id.store(0, Relaxed);
        self.nalu_queue.lock().unwrap().clear();
        self.audio_queue.lock().unwrap().clear();
        self.host_stats.reset();
        self.telemetry_tx.send_replace(BridgeTelemetry::default());
        *self.active_target.write().unwrap() = None;
        *self.connected_at.lock().unwrap() = None;
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
        if last_video.is_some()
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
        let Some((device_id, endpoint)) = self.last_connect.lock().unwrap().clone() else {
            return;
        };
        let _ = self.state_tx.send(BridgeSessionState::Reconnecting);
        self.stop_live();
        self.connect_locked(device_id, endpoint);
    }

    pub fn request_keyframe(&self) {
        if let Some(live) = self.live.lock().unwrap().as_ref() {
            let _ = live.control_tx.send(ControlMessage::RequestKeyframe {
                session_id: self.active_session_id.load(Relaxed),
            });
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
        let devices: Vec<BridgeDiscoveredDevice> = snapshot
            .peers()
            .iter()
            .map(|peer| BridgeDiscoveredDevice {
                device_id: peer.announcement.device_id.clone(),
                display_name: peer.announcement.display_name.clone(),
                endpoint: peer.endpoint.to_string(),
                scope: match peer.scope {
                    DiscoveryScope::Lan => "LAN".to_string(),
                    DiscoveryScope::Mesh => "Mesh".to_string(),
                    DiscoveryScope::Relay => "Relay".to_string(),
                },
                can_stream: peer.announcement.capabilities.can_stream,
                online: true,
            })
            .collect();

        let _ = self.devices_tx.send(devices);
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_live_session(
    target: SocketAddr,
    session_id: u32,
    host_stats: Arc<SharedHostStats>,
    nalu_queue: Arc<Mutex<VecDeque<EncodedVideoNalu>>>,
    audio_queue: Arc<Mutex<VecDeque<EncodedAudioPacket>>>,
    active_session_id: Arc<AtomicU32>,
    telemetry_tx: watch::Sender<BridgeTelemetry>,
    state_tx: watch::Sender<BridgeSessionState>,
) -> Result<LiveSession, Box<dyn std::error::Error + Send + Sync>> {
    let bind_addr = if target.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    let multiplexer = UdpMultiplexer::bind(bind_addr).await?;
    let (udp_sender, udp_receiver) = multiplexer.split();
    maybe_authenticate(&udp_sender, &udp_receiver, target).await?;

    udp_sender
        .send_control(
            &ControlMessage::StartStream {
                width: 1920,
                height: 1080,
                fps: 60,
                bitrate_kbps: 20_000,
                session_id,
            },
            target,
        )
        .await?;
    let _ = udp_sender
        .send_control(&ControlMessage::RequestKeyframe { session_id }, target)
        .await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let (audio_tx, mut audio_rx) = mpsc::channel(64);
    let (decode_tx, mut decode_rx) = mpsc::channel(64);
    let stats = Statistics::new();

    let receiver = spawn_client_session_receiver(ClientSessionReceiverConfig {
        bind_addr: multiplexer.local_addr()?,
        udp_receiver,
        stats,
        active_session_id,
        host_stats: host_stats.clone(),
        audio_tx,
        decode_tx,
        clipboard_control: None,
        file_transfer_control: None,
        session_event_tx: None,
    });

    let egress_sender = udp_sender.clone();
    let egress = tokio::spawn(async move {
        while let Some(msg) = control_rx.recv().await {
            if egress_sender.send_control(&msg, target).await.is_err() {
                break;
            }
        }
    });

    let recovery_tx = control_tx.clone();
    let video_activity = Arc::new(Mutex::new(None));
    let activity = video_activity.clone();
    let decode_pump = tokio::spawn(async move {
        let mut waiting_for_keyframe = true;
        let mut received_video = false;
        while let Some((packet, timing)) = decode_rx.recv().await {
            *activity.lock().unwrap() = Some(Instant::now());
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
                    let _ = recovery_tx.send(ControlMessage::RequestKeyframe { session_id });
                }
                if waiting_for_keyframe && !keyframe {
                    continue;
                }
                waiting_for_keyframe = false;
                queue.push_back(nalu);
                if !received_video {
                    received_video = true;
                    let _ = state_tx.send(BridgeSessionState::Streaming);
                }
            }
        }
    });

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

    let heartbeat_tx = control_tx.clone();
    let heartbeat = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if heartbeat_tx.send(ControlMessage::Heartbeat).is_err() {
                break;
            }
            let now_ms = now_unix_ms();
            if heartbeat_tx
                .send(ControlMessage::Ping {
                    client_send_ts: now_ms,
                })
                .is_err()
            {
                break;
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

    let _ = control_tx.send(ControlMessage::Heartbeat);
    Ok(LiveSession {
        control_tx,
        target,
        udp_sender,
        video_activity,
        _receiver: receiver,
        _egress: egress,
        _decode_pump: decode_pump,
        _audio_pump: audio_pump,
        _heartbeat: heartbeat,
        _telemetry: telemetry,
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

async fn run_lan_discovery(
    devices_tx: watch::Sender<Vec<BridgeDiscoveredDevice>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let device_id = format!(
        "android-{}",
        u32::from_be_bytes(random_bytes_16()[..4].try_into().unwrap())
    );
    let announcement = DiscoveryAnnouncement {
        network_name: "RemotePlay".to_string(),
        device_id,
        display_name: "RemotePlay Android".to_string(),
        control_port: 0,
        virtual_ip: None,
        capabilities: DiscoveryCapabilities {
            can_view: true,
            can_stream: false,
            file_transfer: false,
            clipboard_sync: false,
            talkback: false,
        },
        scope: DiscoveryScope::Lan,
        ttl: DEFAULT_PEER_TTL,
    };
    let mut config = DiscoveryRuntimeConfig::lan_default(announcement);
    config.accept_any_network = true;
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let (snapshot_tx, mut snapshot_rx) = watch::channel(DiscoveryPeerSnapshot::default());
    let (_cancel_tx, cancel_rx) = broadcast::channel(1);
    let discovery = tokio::spawn(run_discovery_runtime(
        config,
        events_tx,
        snapshot_tx,
        cancel_rx,
    ));
    loop {
        if snapshot_rx.changed().await.is_err() {
            break;
        }
        let snapshot = snapshot_rx.borrow().clone();
        let devices: Vec<BridgeDiscoveredDevice> = snapshot
            .peers()
            .iter()
            .filter(|peer| peer.announcement.capabilities.can_stream)
            .map(|peer| BridgeDiscoveredDevice {
                device_id: peer.announcement.device_id.clone(),
                display_name: peer.announcement.display_name.clone(),
                endpoint: peer.endpoint.to_string(),
                scope: match peer.scope {
                    DiscoveryScope::Lan => "LAN".to_string(),
                    DiscoveryScope::Mesh => "Mesh".to_string(),
                    DiscoveryScope::Relay => "Relay".to_string(),
                },
                can_stream: peer.announcement.capabilities.can_stream,
                online: true,
            })
            .collect();
        let _ = devices_tx.send(devices);
    }
    discovery.abort();
    Ok(())
}

async fn maybe_authenticate(
    udp_sender: &UdpSender,
    udp_receiver: &remote_core::net::UdpReceiver,
    target: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(psk) = load_session_psk() else {
        return Ok(());
    };
    let nonce = random_bytes_16();
    let timestamp_ms = now_unix_ms();
    let mac = mac_session_hello(&psk, &nonce, timestamp_ms);
    udp_sender
        .send_control(
            &ControlMessage::SessionHello {
                nonce,
                timestamp_ms,
                mac,
            },
            target,
        )
        .await?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match tokio::time::timeout_at(deadline, udp_receiver.recv()).await {
            Ok(Ok(remote_core::net::MultiplexedPacket::Control(
                ControlMessage::SessionAccept {
                    salt,
                    timestamp_ms,
                    mac,
                },
                _,
            ))) => {
                let expected = remote_core::mac_session_accept(&psk, &salt, timestamp_ms);
                remote_core::verify_session_mac(&expected, &mac, timestamp_ms, now_unix_ms())?;
                let send_crypto = SessionCrypto::from_psk(&psk, &salt)?;
                let recv_crypto = SessionCrypto::from_psk(&psk, &salt)?;
                let _ = udp_sender.install_crypto(send_crypto);
                let _ = udp_receiver.install_crypto(recv_crypto);
                return Ok(());
            }
            Ok(Ok(remote_core::net::MultiplexedPacket::Control(
                ControlMessage::SessionReject { reason },
                _,
            ))) => {
                return Err(format!("session rejected: {reason}").into());
            }
            Ok(Ok(_)) => continue,
            Ok(Err(err)) => return Err(err),
            Err(_) => return Err("session hello timed out".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_client_starts_disconnected() {
        let client = RemoteBridgeClient::new();
        assert_eq!(*client.state_rx.borrow(), BridgeSessionState::Disconnected);
        assert_eq!(client.poll_video_nalu(), None);
    }

    #[test]
    fn telemetry_defaults_are_zero_not_marketing_numbers() {
        let telemetry = BridgeTelemetry::default();
        assert_eq!(telemetry.fps, 0.0);
        assert_eq!(telemetry.latency_ms, 0.0);
        assert_eq!(telemetry.video_bitrate_kbps, 0);
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

    async fn next_control(
        receiver: &remote_core::net::UdpReceiver,
    ) -> (ControlMessage, SocketAddr) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let remote_core::net::MultiplexedPacket::Control(message, source) =
                    receiver.recv().await.unwrap()
                {
                    return (message, source);
                }
            }
        })
        .await
        .expect("control packet deadline")
    }

    #[test]
    fn replacing_session_stops_old_tasks_and_disconnect_reaches_host() {
        let client = RemoteBridgeClient::new();
        let host = client
            .runtime()
            .block_on(UdpMultiplexer::bind("127.0.0.1:0"))
            .unwrap();
        let (_, receiver) = host.split();
        let endpoint = host.local_addr().unwrap().to_string();
        client.connect("first".into(), endpoint.clone());
        let (message, old_address) = client.runtime().block_on(next_control(&receiver));
        assert!(matches!(message, ControlMessage::StartStream { .. }));
        let old_tasks = {
            let live = client.live.lock().unwrap();
            let live = live.as_ref().unwrap();
            [
                live._receiver.abort_handle(),
                live._egress.abort_handle(),
                live._decode_pump.abort_handle(),
                live._audio_pump.abort_handle(),
                live._heartbeat.abort_handle(),
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
                    let (message, source) = next_control(&receiver).await;
                    match message {
                        ControlMessage::StopStream => {
                            assert_eq!(source, old_address);
                            saw_stop = true;
                        }
                        ControlMessage::StartStream { .. } => break source,
                        _ => {}
                    }
                }
            })
            .await
            .expect("old StopStream followed by new StartStream")
        });
        assert!(saw_stop);
        client.disconnect();
        client.runtime().block_on(async {
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let (message, source) = next_control(&receiver).await;
                    if matches!(message, ControlMessage::StopStream) {
                        assert_eq!(source, new_address);
                        break;
                    }
                }
            })
            .await
            .expect("disconnect StopStream")
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
        let host = client
            .runtime()
            .block_on(UdpMultiplexer::bind("127.0.0.1:0"))
            .unwrap();
        let (sender, receiver) = host.split();
        client.connect("test-host".into(), host.local_addr().unwrap().to_string());
        let (start, target) = client.runtime().block_on(next_control(&receiver));
        let ControlMessage::StartStream { session_id, .. } = start else {
            panic!("start expected")
        };
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
                let mut requests = 0;
                while requests < 2 {
                    // initial request and overflow recovery
                    if matches!(
                        next_control(&receiver).await.0,
                        ControlMessage::RequestKeyframe { .. }
                    ) {
                        requests += 1;
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
        let host = client
            .runtime()
            .block_on(UdpMultiplexer::bind("127.0.0.1:0"))
            .unwrap();
        client.connect("silent-host".into(), host.local_addr().unwrap().to_string());
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
        let host = client
            .runtime()
            .block_on(UdpMultiplexer::bind("127.0.0.1:0"))
            .unwrap();
        client.connect("host".into(), host.local_addr().unwrap().to_string());
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
        let host = client
            .runtime()
            .block_on(UdpMultiplexer::bind("127.0.0.1:0"))
            .unwrap();
        client.connect("idle-host".into(), host.local_addr().unwrap().to_string());
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
}
