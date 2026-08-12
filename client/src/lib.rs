pub mod audio_player;
mod host_config;
#[allow(dead_code)]
mod host_list;
mod mesh_pairing;
mod render;
pub mod session;
mod talkback;
mod transfer_center;
mod video_decode;

use crate::audio_player::{AudioPlayerEvent, AudioPlayerSettings};
use protocol::DataEnvelope;
use remote_core::clipboard_file_runtime::{ClipboardFileSyncConfig, run_clipboard_file_sync};
use remote_core::clipboard_runtime::{ClipboardSyncRunnerConfig, run_clipboard_sync};
use remote_core::discovery::{
    DEFAULT_PEER_TTL, DiscoveryAnnouncement, DiscoveryCapabilities, DiscoveryEvent,
    DiscoveryPeerSnapshot, DiscoveryRuntimeConfig, DiscoveryScope, REMOTE_PLAY_DISCOVERY_ENV,
    discovery_port_from_env, run_discovery_runtime,
};
use remote_core::file_transfer::FileReceivePolicy;
use remote_core::file_transfer_runtime::{
    FileTransferCommand, FileTransferEvent, FileTransferGroupFile, FileTransferRuntimeConfig,
    run_file_transfer_runtime,
};
use remote_core::mesh::{
    AppPrivateMeshConfigStore, EASYTIER_SIDECAR_LOG_FILE_NAME, EasyTierBinaryLocator,
    EasyTierCliProbeConfig, EasyTierHealthMonitorConfig, EasyTierHealthMonitorHandle,
    EasyTierHealthSnapshot, EasyTierProcessState, EasyTierSidecarManager, REMOTE_PLAY_MESH_ENV,
    default_app_private_mesh_dir, spawn_easytier_health_monitor,
};
use remote_core::net::UdpMultiplexer;
use remote_core::scheduled_sender::{ScheduledDataSender, ScheduledDataSenderConfig};
use remote_core::stats::Statistics;
use std::error::Error;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::spawn;
use tokio::sync::{broadcast, mpsc, watch};
use transfer_center::{TransferCenterState, TransferEntrySnapshot};

#[cfg(target_os = "macos")]
use remote_platform::MacClipboardProvider;

pub use mesh_pairing::{MeshPairingControl, MeshPairingMessageKind, MeshPairingSnapshot};

#[derive(Default, Clone)]
pub struct HostStats {
    pub fps: f32,
    pub latency: f32,
    pub jitter: f32,
    pub bitrate_kbps: u32,
}

pub struct DiscoveryRuntimeHandle {
    cancel_tx: broadcast::Sender<()>,
    pub snapshot_rx: watch::Receiver<DiscoveryPeerSnapshot>,
}

pub struct MeshRuntimeHandle {
    _monitor: Arc<Mutex<Option<EasyTierHealthMonitorHandle>>>,
    health_rx: watch::Receiver<EasyTierHealthSnapshot>,
    reload_tx: mpsc::UnboundedSender<()>,
    virtual_ip: Option<IpAddr>,
}

impl Drop for DiscoveryRuntimeHandle {
    fn drop(&mut self) {
        let _ = self.cancel_tx.send(());
    }
}

#[derive(Clone)]
pub struct AudioPlaybackControl {
    audio_tx: mpsc::Sender<AudioPlayerEvent>,
}

impl AudioPlaybackControl {
    pub fn set_settings(&self, settings: AudioPlayerSettings) {
        let _ = self.audio_tx.try_send(AudioPlayerEvent::Settings(settings));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMediaRuntimeStatus {
    Ready,
    AudioUnavailable { reason: String },
}

#[derive(Clone)]
pub struct ClipboardRuntimeControl {
    command_tx: mpsc::UnboundedSender<ClipboardRuntimeCommand>,
    inbound_tx: Arc<Mutex<Option<mpsc::Sender<DataEnvelope>>>>,
}

#[derive(Debug, Clone, Copy)]
enum ClipboardRuntimeCommand {
    Start(SocketAddr),
    Stop,
}

impl ClipboardRuntimeControl {
    pub fn start(&self, target: SocketAddr) {
        let _ = self.command_tx.send(ClipboardRuntimeCommand::Start(target));
    }

    pub fn stop(&self) {
        let _ = self.command_tx.send(ClipboardRuntimeCommand::Stop);
    }

    fn route_inbound(&self, envelope: DataEnvelope) {
        let Some(tx) = self.inbound_tx.lock().unwrap().clone() else {
            return;
        };

        if let Err(err) = tx.try_send(envelope) {
            eprintln!("Clipboard inbound queue rejected packet: {}", err);
        }
    }
}

#[derive(Clone)]
pub struct FileTransferRuntimeControl {
    command_tx: mpsc::UnboundedSender<FileTransferRuntimeCommand>,
    inbound_tx: Arc<Mutex<Option<mpsc::Sender<DataEnvelope>>>>,
    transfer_state: Arc<Mutex<TransferCenterState>>,
    settings: Arc<Mutex<FileTransferRuntimeSettings>>,
}

#[derive(Debug, Clone)]
enum FileTransferRuntimeCommand {
    Start(SocketAddr),
    Stop,
    SendFile {
        path: PathBuf,
        mime_type: Option<String>,
    },
    SendFileGroup {
        paths: Vec<PathBuf>,
    },
    CancelTransfer {
        transfer_id: u64,
    },
    CancelGroup {
        group_id: u64,
    },
    SetReceiveSettings(FileTransferRuntimeSettings),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransferRuntimeSettings {
    receive_dir: PathBuf,
    allow_overwrite: bool,
}

impl FileTransferRuntimeSettings {
    fn from_env() -> Self {
        Self {
            receive_dir: env_path_or_temp(
                "REMOTE_PLAY_FILE_RECEIVE_DIR",
                "remote-play-client-received-files",
            ),
            allow_overwrite: env_flag_enabled("REMOTE_PLAY_FILE_ALLOW_OVERWRITE"),
        }
    }

    fn receive_policy(&self) -> FileReceivePolicy {
        FileReceivePolicy {
            allow_overwrite: self.allow_overwrite,
            ..FileReceivePolicy::default()
        }
    }

    fn runtime_config(&self) -> FileTransferRuntimeConfig {
        FileTransferRuntimeConfig {
            receive_dir: self.receive_dir.clone(),
            receive_policy: self.receive_policy(),
            ..FileTransferRuntimeConfig::default()
        }
    }
}

impl FileTransferRuntimeControl {
    pub fn start(&self, target: SocketAddr) {
        let _ = self
            .command_tx
            .send(FileTransferRuntimeCommand::Start(target));
    }

    pub fn stop(&self) {
        let _ = self.command_tx.send(FileTransferRuntimeCommand::Stop);
    }

    pub fn send_file(&self, path: PathBuf, mime_type: Option<String>) {
        let _ = self
            .command_tx
            .send(FileTransferRuntimeCommand::SendFile { path, mime_type });
    }

    pub fn send_file_group(&self, paths: Vec<PathBuf>) {
        let _ = self
            .command_tx
            .send(FileTransferRuntimeCommand::SendFileGroup { paths });
    }

    pub fn cancel_transfer(&self, transfer_id: u64) {
        let _ = self
            .command_tx
            .send(FileTransferRuntimeCommand::CancelTransfer { transfer_id });
    }

    pub fn cancel_group(&self, group_id: u64) {
        let _ = self
            .command_tx
            .send(FileTransferRuntimeCommand::CancelGroup { group_id });
    }

    pub fn receive_dir(&self) -> PathBuf {
        self.settings.lock().unwrap().receive_dir.clone()
    }

    pub fn set_receive_dir(&self, receive_dir: PathBuf) {
        let settings = {
            let mut settings = self.settings.lock().unwrap();
            settings.receive_dir = receive_dir;
            settings.clone()
        };
        let _ = self
            .command_tx
            .send(FileTransferRuntimeCommand::SetReceiveSettings(settings));
    }

    pub fn allow_overwrite(&self) -> bool {
        self.settings.lock().unwrap().allow_overwrite
    }

    pub fn set_allow_overwrite(&self, allow_overwrite: bool) {
        let settings = {
            let mut settings = self.settings.lock().unwrap();
            settings.allow_overwrite = allow_overwrite;
            settings.clone()
        };
        let _ = self
            .command_tx
            .send(FileTransferRuntimeCommand::SetReceiveSettings(settings));
    }

    pub fn transfer_snapshot(&self) -> Vec<TransferEntrySnapshot> {
        self.transfer_state.lock().unwrap().snapshots()
    }

    fn route_inbound(&self, envelope: DataEnvelope) {
        let Some(tx) = self.inbound_tx.lock().unwrap().clone() else {
            return;
        };

        if let Err(err) = tx.try_send(envelope) {
            eprintln!("File transfer inbound queue rejected packet: {}", err);
        }
    }
}

#[derive(Clone)]
pub struct TalkbackRuntimeControl {
    command_tx: mpsc::UnboundedSender<TalkbackRuntimeCommand>,
}

#[derive(Debug, Clone, Copy)]
enum TalkbackRuntimeCommand {
    Start { target: SocketAddr, session_id: u32 },
    SetSettings(talkback::TalkbackCaptureSettings),
    Stop,
}

impl TalkbackRuntimeControl {
    pub fn start(&self, target: SocketAddr, session_id: u32) {
        let _ = self
            .command_tx
            .send(TalkbackRuntimeCommand::Start { target, session_id });
    }

    pub fn stop(&self) {
        let _ = self.command_tx.send(TalkbackRuntimeCommand::Stop);
    }

    pub fn set_settings(&self, settings: talkback::TalkbackCaptureSettings) {
        let _ = self
            .command_tx
            .send(TalkbackRuntimeCommand::SetSettings(settings));
    }
}

pub use session::{ClientSessionEvent, ClientSessionReceiverConfig, spawn_client_session_receiver};
pub use video_decode::{MacDecodedVideoFrame, decoded_video_frame_surface};

pub struct ClientMediaRuntime {
    pub audio_playback: AudioPlaybackControl,
    pub audio_tx: mpsc::Sender<AudioPlayerEvent>,
    pub decode_tx: mpsc::Sender<(protocol::RtpPacket, u32)>,
    pub status: ClientMediaRuntimeStatus,
    shared_frame: Arc<Mutex<Option<MacDecodedVideoFrame>>>,
    _audio_player: Option<audio_player::AudioPlayer>,
    _decode_task: tokio::task::JoinHandle<()>,
}

impl ClientMediaRuntime {
    pub fn start(stats: Arc<Statistics>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let (audio_tx, audio_rx) = mpsc::channel(100);
        let audio_playback = AudioPlaybackControl {
            audio_tx: audio_tx.clone(),
        };
        let (audio_player, status) = match audio_player::AudioPlayer::new(audio_rx) {
            Ok(player) => (Some(player), ClientMediaRuntimeStatus::Ready),
            Err(err) => {
                let reason = err.to_string();
                eprintln!("Audio output unavailable; remote audio playback is disabled: {reason}");
                (None, ClientMediaRuntimeStatus::AudioUnavailable { reason })
            }
        };
        let shared_frame = Arc::new(Mutex::new(None));
        let decode_shared_frame = shared_frame.clone();
        let (decode_tx, mut decode_rx) = mpsc::channel::<(protocol::RtpPacket, u32)>(200);
        let stats_decode = stats.clone();

        let decode_task = spawn(async move {
            let mut video_decoder = match video_decode::MacVideoDecoder::new() {
                Ok(decoder) => decoder,
                Err(err) => {
                    eprintln!("Failed to initialize video decoder: {}", err);
                    return;
                }
            };

            while let Some((ordered_pkt, recv_time)) = decode_rx.recv().await {
                use remote_core::VideoDecoder;
                match video_decoder.decode(&ordered_pkt.payload).await {
                    Ok(mut frame) => {
                        frame.timestamp = ordered_pkt.header.timestamp;
                        frame.recv_time = recv_time;
                        stats_decode.video_frames_decoded.fetch_add(1, Relaxed);
                        *decode_shared_frame.lock().unwrap() = Some(frame);
                    }
                    Err(err) => {
                        if err.to_string() != "No frame data or session not ready" {
                            eprintln!("Decode error: {}", err);
                        }
                    }
                }
            }
        });

        Ok(Self {
            audio_playback,
            audio_tx,
            decode_tx,
            status,
            shared_frame,
            _audio_player: audio_player,
            _decode_task: decode_task,
        })
    }

    pub fn shared_frame(&self) -> Arc<Mutex<Option<MacDecodedVideoFrame>>> {
        self.shared_frame.clone()
    }
}

pub fn start_clipboard_runtime_control(
    udp_sender: remote_core::net::UdpSender,
) -> ClipboardRuntimeControl {
    start_clipboard_runtime_controller(udp_sender)
}

pub fn start_file_transfer_runtime_control(
    udp_sender: remote_core::net::UdpSender,
) -> FileTransferRuntimeControl {
    start_file_transfer_runtime_controller(udp_sender)
}

pub fn start_talkback_runtime_control(
    udp_sender: remote_core::net::UdpSender,
) -> TalkbackRuntimeControl {
    start_talkback_runtime_controller(udp_sender)
}

pub async fn run_client_binary() -> Result<(), Box<dyn Error + Send + Sync>> {
    println!("Client starting...");
    let _mesh_runtime = maybe_start_mesh_sidecar("RemotePlay Viewer").await;
    let mesh_pairing = match MeshPairingControl::load_or_create(
        default_app_private_mesh_dir(),
        "RemotePlay Viewer",
    ) {
        Ok(control) => Some(if let Some(runtime) = &_mesh_runtime {
            control.with_mesh_reload_tx(runtime.reload_tx.clone())
        } else {
            control
        }),
        Err(err) => {
            eprintln!("Mesh pairing setup is unavailable: {err}");
            None
        }
    };
    let discovery_virtual_ip = _mesh_runtime
        .as_ref()
        .and_then(|runtime| runtime.virtual_ip);
    let discovery = maybe_start_discovery_runtime(
        "RemotePlay Viewer",
        0,
        discovery_virtual_ip,
        DiscoveryCapabilities {
            can_view: true,
            can_stream: false,
            file_transfer: env_flag_enabled("REMOTE_PLAY_FILE_TRANSFER")
                || env_flag_enabled("REMOTE_PLAY_FILE_CLIPBOARD"),
            clipboard_sync: env_flag_enabled("REMOTE_PLAY_CLIPBOARD_SYNC"),
            talkback: env_flag_enabled("REMOTE_PLAY_TALKBACK"),
        },
    )
    .await;

    let stats = Statistics::new();
    Statistics::start_reporter(stats.clone(), "Client", 1);

    let host_stats = Arc::new(std::sync::RwLock::new(HostStats::default()));
    let host_stats_udp = host_stats.clone();

    // 1. Setup Network (Multiplexer & Receiver)
    let bind_addr: SocketAddr = "0.0.0.0:0".parse()?;
    let multiplexer = UdpMultiplexer::bind(&bind_addr.to_string()).await?;
    let (udp_sender, udp_receiver) = multiplexer.split();
    let clipboard_control = if env_flag_enabled("REMOTE_PLAY_CLIPBOARD_SYNC") {
        println!("Clipboard data-plane sync enabled.");
        Some(start_clipboard_runtime_controller(udp_sender.clone()))
    } else {
        None
    };
    let file_transfer_control = if env_flag_enabled("REMOTE_PLAY_FILE_TRANSFER")
        || env_flag_enabled("REMOTE_PLAY_FILE_CLIPBOARD")
        || std::env::var_os("REMOTE_PLAY_SEND_FILE").is_some()
    {
        println!("File transfer data-plane runtime enabled.");
        Some(start_file_transfer_runtime_controller(udp_sender.clone()))
    } else {
        None
    };
    let talkback_control = if env_flag_enabled("REMOTE_PLAY_TALKBACK") {
        println!("Viewer microphone talkback enabled.");
        Some(start_talkback_runtime_controller(udp_sender.clone()))
    } else {
        None
    };

    // 2. Start Receiver Loop
    let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(100);
    let audio_playback_control = AudioPlaybackControl {
        audio_tx: audio_tx.clone(),
    };

    // Initialize AudioPlayer
    let _audio_player = crate::audio_player::AudioPlayer::new(audio_rx)?;

    let stats_decode = stats.clone();

    let shared_frame: Arc<Mutex<Option<MacDecodedVideoFrame>>> = Arc::new(Mutex::new(None));
    let render_shared_frame = shared_frame.clone();

    // Decouple decoding from receiving
    let (decode_tx, mut decode_rx) = tokio::sync::mpsc::channel::<(protocol::RtpPacket, u32)>(200);

    // Decode Task
    spawn(async move {
        let mut video_decoder = match crate::video_decode::MacVideoDecoder::new() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Failed to initialize video decoder: {}", e);
                return;
            }
        };

        while let Some((ordered_pkt, recv_time)) = decode_rx.recv().await {
            use remote_core::VideoDecoder;
            match video_decoder.decode(&ordered_pkt.payload).await {
                Ok(mut frame) => {
                    frame.timestamp = ordered_pkt.header.timestamp;
                    frame.recv_time = recv_time;
                    stats_decode.video_frames_decoded.fetch_add(1, Relaxed);
                    *shared_frame.lock().unwrap() = Some(frame);
                }
                Err(e) => {
                    if e.to_string() != "No frame data or session not ready" {
                        eprintln!("Decode error: {}", e);
                    }
                }
            }
        }
    });

    let active_session_id = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let _receiver_task =
        session::spawn_client_session_receiver(session::ClientSessionReceiverConfig {
            bind_addr,
            udp_receiver,
            stats: stats.clone(),
            active_session_id: active_session_id.clone(),
            host_stats: host_stats_udp,
            audio_tx,
            decode_tx,
            clipboard_control: clipboard_control.clone(),
            file_transfer_control: file_transfer_control.clone(),
            session_event_tx: None,
        });

    let _stats_task = tokio::spawn({
        let stats_print = stats.clone();
        async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                println!("=== [Client] Telemetry (Last 1s) ===");
                println!(
                    " Net   | Recv: {:4} pkts | Bandwidth: {:.2} KB/s",
                    stats_print.udp_packets_recv.swap(0, Relaxed),
                    stats_print.udp_bytes_recv.swap(0, Relaxed) as f64 / 1024.0
                );
                println!(
                    " JBuf  | Push: {:4} pkt/s | Pop: {:4} pkt/s",
                    stats_print.video_jitter_buffer_push.swap(0, Relaxed),
                    stats_print.video_jitter_buffer_pop.swap(0, Relaxed)
                );
                println!(
                    " Video | Decode: {:2} fps",
                    stats_print.video_frames_decoded.swap(0, Relaxed)
                );
                println!("========================================");
            }
        }
    });

    // 3. Start Render Loop (Blocks Main Thread)
    render::run_client(
        udp_sender,
        render_shared_frame,
        active_session_id,
        host_stats,
        render::ClientRuntimeControls {
            audio_playback: audio_playback_control,
            clipboard: clipboard_control,
            file_transfer: file_transfer_control,
            talkback: talkback_control,
            discovery: discovery.as_ref().map(|handle| handle.snapshot_rx.clone()),
            mesh_health: _mesh_runtime
                .as_ref()
                .map(|runtime| runtime.health_rx.clone()),
            mesh_pairing,
        },
    )
    .await?;

    Ok(())
}

async fn maybe_start_discovery_runtime(
    display_name: &str,
    control_port: u16,
    virtual_ip: Option<std::net::IpAddr>,
    capabilities: DiscoveryCapabilities,
) -> Option<DiscoveryRuntimeHandle> {
    if !env_flag_enabled(REMOTE_PLAY_DISCOVERY_ENV) {
        return None;
    }

    let mesh_dir = default_app_private_mesh_dir();
    let store = AppPrivateMeshConfigStore::new(&mesh_dir);
    let mesh_config = match store.load_or_generate(display_name) {
        Ok(config) => config,
        Err(err) => {
            eprintln!(
                "Discovery enabled, but mesh config initialization failed in {}: {}",
                mesh_dir.display(),
                err
            );
            return None;
        }
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
    let discovery_port = match discovery_port_from_env() {
        Ok(port) => port,
        Err(err) => {
            eprintln!("Discovery enabled, but port configuration is invalid: {err}");
            return None;
        }
    };
    let config = DiscoveryRuntimeConfig::lan_on_port(announcement, discovery_port);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let (snapshot_tx, snapshot_rx) = watch::channel(DiscoveryPeerSnapshot::default());
    let (cancel_tx, cancel_rx) = broadcast::channel(1);

    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            log_discovery_event("Client", event);
        }
    });

    tokio::spawn(async move {
        if let Err(err) = run_discovery_runtime(config, events_tx, snapshot_tx, cancel_rx).await {
            eprintln!("Client discovery runtime stopped: {err}");
        }
    });

    println!("Client discovery enabled on UDP port {discovery_port}.");
    Some(DiscoveryRuntimeHandle {
        cancel_tx,
        snapshot_rx,
    })
}

fn log_discovery_event(label: &str, event: DiscoveryEvent) {
    match event {
        DiscoveryEvent::PeerSeen(peer) => println!(
            "{label} Discovery | peer {} at {} ({})",
            peer.announcement.display_name, peer.endpoint, peer.announcement.device_id
        ),
        DiscoveryEvent::PeerExpired(peer) => println!(
            "{label} Discovery | peer expired {} ({})",
            peer.announcement.display_name, peer.announcement.device_id
        ),
        DiscoveryEvent::Snapshot(snapshot) => {
            println!("{label} Discovery | {} peer(s)", snapshot.len());
        }
        DiscoveryEvent::Error(err) => eprintln!("{label} Discovery | {err}"),
    }
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

fn env_path_or_temp(name: &str, fallback_dir_name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(fallback_dir_name))
}

async fn maybe_start_mesh_sidecar(display_name: &str) -> Option<MeshRuntimeHandle> {
    if !env_flag_enabled(REMOTE_PLAY_MESH_ENV) {
        return None;
    }

    let (monitor, virtual_ip) = start_mesh_monitor(display_name).await?;
    let initial_snapshot = monitor.snapshot_rx.borrow().clone();
    let (health_tx, health_rx) = watch::channel(initial_snapshot);
    spawn_mesh_health_bridge(monitor.snapshot_rx.clone(), health_tx.clone());

    let monitor = Arc::new(Mutex::new(Some(monitor)));
    let (reload_tx, mut reload_rx) = mpsc::unbounded_channel();
    let reload_monitor = monitor.clone();
    let reload_health_tx = health_tx.clone();
    let display_name = display_name.to_string();
    tokio::spawn(async move {
        while reload_rx.recv().await.is_some() {
            let old_monitor = reload_monitor.lock().unwrap().take();
            drop(old_monitor);

            match start_mesh_monitor(&display_name).await {
                Some((monitor, _virtual_ip)) => {
                    spawn_mesh_health_bridge(monitor.snapshot_rx.clone(), reload_health_tx.clone());
                    *reload_monitor.lock().unwrap() = Some(monitor);
                }
                None => {
                    let _ = reload_health_tx.send(EasyTierHealthSnapshot::degraded(
                        EasyTierProcessState::NotStarted,
                        None,
                        "EasyTier mesh reload failed",
                    ));
                }
            }
        }
    });

    Some(MeshRuntimeHandle {
        _monitor: monitor,
        health_rx,
        reload_tx,
        virtual_ip,
    })
}

async fn start_mesh_monitor(
    display_name: &str,
) -> Option<(EasyTierHealthMonitorHandle, Option<IpAddr>)> {
    let mesh_dir = default_app_private_mesh_dir();
    let store = AppPrivateMeshConfigStore::new(&mesh_dir);
    let mesh_config = match store.load_or_generate(display_name) {
        Ok(config) => config,
        Err(err) => {
            eprintln!(
                "EasyTier mesh enabled, but mesh config initialization failed in {}: {}",
                mesh_dir.display(),
                err
            );
            return None;
        }
    };

    let locator = EasyTierBinaryLocator::from_environment();
    let mut manager = match EasyTierSidecarManager::from_locator(mesh_config, &locator) {
        Ok(manager) => manager,
        Err(err) => {
            eprintln!("EasyTier mesh enabled, but sidecar setup failed: {err}");
            return None;
        }
    };
    manager.set_log_file_path(mesh_dir.join(EASYTIER_SIDECAR_LOG_FILE_NAME));

    let launch_plan = manager.launch_plan();
    println!(
        "EasyTier mesh enabled. Starting sidecar: {} {}",
        launch_plan.binary_path.display(),
        launch_plan.redacted_args.join(" ")
    );

    match manager.start().await {
        Ok(()) => {
            println!("EasyTier sidecar started.");
            let virtual_ip = probe_mesh_virtual_ip(&launch_plan.binary_path).await;
            match manager.health_snapshot(virtual_ip) {
                Ok(snapshot) => println!(
                    "EasyTier mesh status: {:?}; virtual_ip={}",
                    snapshot.state,
                    snapshot
                        .virtual_ip
                        .map(|ip| ip.to_string())
                        .unwrap_or_else(|| "pending".to_string())
                ),
                Err(err) => eprintln!("EasyTier mesh status check failed: {err}"),
            }
            let monitor = spawn_easytier_health_monitor(
                manager,
                EasyTierHealthMonitorConfig::from_sidecar_binary(&launch_plan.binary_path),
                virtual_ip,
            );
            Some((monitor, virtual_ip))
        }
        Err(err) => {
            eprintln!("EasyTier sidecar failed to start: {err}");
            None
        }
    }
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

async fn probe_mesh_virtual_ip(sidecar_binary_path: &Path) -> Option<std::net::IpAddr> {
    let probe = EasyTierCliProbeConfig::from_sidecar_binary(sidecar_binary_path);
    match probe.run().await {
        Ok(result) => {
            if let Some(ip) = result.virtual_ip {
                println!("EasyTier virtual IP detected: {ip}");
                Some(ip)
            } else {
                eprintln!("EasyTier node probe completed, but no virtual IP was reported yet.");
                None
            }
        }
        Err(err) => {
            eprintln!("EasyTier virtual IP probe is pending: {err}");
            None
        }
    }
}

fn log_file_transfer_event(label: &str, event: FileTransferEvent) {
    match event {
        FileTransferEvent::OutgoingGroupStarted {
            group_id,
            file_count,
            total_size_bytes,
        } => println!(
            "{label} FileTx | outgoing group #{group_id} started: {file_count} files, {total_size_bytes} bytes"
        ),
        FileTransferEvent::OutgoingStarted {
            transfer_id,
            name,
            size_bytes,
            total_chunks,
            ..
        } => println!(
            "{label} FileTx | outgoing #{transfer_id} started: {name} ({size_bytes} bytes, {total_chunks} chunks)"
        ),
        FileTransferEvent::OutgoingProgress {
            transfer_id,
            sent_chunks,
            total_chunks,
            sent_bytes,
            total_size,
            ..
        } => println!(
            "{label} FileTx | outgoing #{transfer_id}: {sent_chunks}/{total_chunks} chunks, {sent_bytes}/{total_size} bytes"
        ),
        FileTransferEvent::OutgoingCompleted { transfer_id, .. } => {
            println!("{label} FileTx | outgoing #{transfer_id} completed");
        }
        FileTransferEvent::OutgoingCancelled { transfer_id, .. } => {
            println!("{label} FileTx | outgoing #{transfer_id} cancelled");
        }
        FileTransferEvent::OutgoingGroupCompleted {
            group_id,
            file_count,
            total_size_bytes,
        } => println!(
            "{label} FileTx | outgoing group #{group_id} completed: {file_count} files, {total_size_bytes} bytes"
        ),
        FileTransferEvent::OutgoingGroupCancelled { group_id } => {
            println!("{label} FileTx | outgoing group #{group_id} cancelled");
        }
        FileTransferEvent::IncomingStarted {
            transfer_id,
            name,
            size_bytes,
            path,
            ..
        } => println!(
            "{label} FileTx | incoming #{transfer_id} started: {name} ({size_bytes} bytes) -> {}",
            path.display()
        ),
        FileTransferEvent::IncomingProgress {
            transfer_id,
            received_chunks,
            total_chunks,
            received_bytes,
            total_size,
            ..
        } => println!(
            "{label} FileTx | incoming #{transfer_id}: {received_chunks}/{total_chunks} chunks, {received_bytes}/{total_size} bytes"
        ),
        FileTransferEvent::IncomingCompleted {
            transfer_id,
            path,
            size_bytes,
            ..
        } => println!(
            "{label} FileTx | incoming #{transfer_id} completed: {} ({size_bytes} bytes)",
            path.display()
        ),
        FileTransferEvent::IncomingCancelled {
            transfer_id, path, ..
        } => println!(
            "{label} FileTx | incoming #{transfer_id} cancelled and cleaned: {}",
            path.display()
        ),
        FileTransferEvent::IncomingGroupCompleted {
            group_id,
            paths,
            total_size_bytes,
        } => println!(
            "{label} FileTx | incoming group #{group_id} completed: {} files, {total_size_bytes} bytes",
            paths.len()
        ),
        FileTransferEvent::IncomingGroupCancelled { group_id } => {
            println!("{label} FileTx | incoming group #{group_id} cancelled");
        }
        FileTransferEvent::Error {
            transfer_id,
            message,
        } => eprintln!("{label} FileTx | error {:?}: {}", transfer_id, message),
    }
}

fn record_and_log_file_transfer_event(
    label: &str,
    state: &Arc<Mutex<TransferCenterState>>,
    event: FileTransferEvent,
) {
    state.lock().unwrap().apply_event(&event);
    log_file_transfer_event(label, event);
}

fn start_talkback_runtime_controller(
    udp_sender: remote_core::net::UdpSender,
) -> TalkbackRuntimeControl {
    let (command_tx, mut command_rx) = mpsc::unbounded_channel();

    spawn(async move {
        let mut active_cancel_tx: Option<broadcast::Sender<()>> = None;
        let mut active_settings_tx: Option<
            tokio::sync::watch::Sender<talkback::TalkbackCaptureSettings>,
        > = None;
        let mut current_settings = talkback::TalkbackCaptureSettings::default();

        while let Some(command) = command_rx.recv().await {
            match command {
                TalkbackRuntimeCommand::Start { target, session_id } => {
                    if let Some(cancel_tx) = active_cancel_tx.take() {
                        let _ = cancel_tx.send(());
                    }

                    let (cancel_tx, cancel_rx) = broadcast::channel(1);
                    active_cancel_tx = Some(cancel_tx);
                    let (settings_tx, settings_rx) = tokio::sync::watch::channel(current_settings);
                    active_settings_tx = Some(settings_tx);
                    let udp_sender = udp_sender.clone();

                    spawn(async move {
                        if let Err(err) =
                            talkback::run_talkback_capture(talkback::TalkbackRuntimeConfig {
                                udp_sender,
                                target,
                                session_id,
                                cancel_rx,
                                settings_rx,
                            })
                            .await
                        {
                            eprintln!("Talkback runtime stopped with error: {}", err);
                        }
                    });
                }
                TalkbackRuntimeCommand::SetSettings(settings) => {
                    current_settings = settings;
                    if let Some(settings_tx) = &active_settings_tx {
                        let _ = settings_tx.send(settings);
                    }
                }
                TalkbackRuntimeCommand::Stop => {
                    if let Some(cancel_tx) = active_cancel_tx.take() {
                        let _ = cancel_tx.send(());
                    }
                    active_settings_tx = None;
                }
            }
        }
    });

    TalkbackRuntimeControl { command_tx }
}

#[cfg(target_os = "macos")]
fn start_clipboard_runtime_controller(
    udp_sender: remote_core::net::UdpSender,
) -> ClipboardRuntimeControl {
    let (command_tx, mut command_rx) = mpsc::unbounded_channel();
    let inbound_tx: Arc<Mutex<Option<mpsc::Sender<DataEnvelope>>>> = Arc::new(Mutex::new(None));
    let controller_inbound_tx = inbound_tx.clone();

    spawn(async move {
        let mut active_addr: Option<SocketAddr> = None;
        let mut active_cancel_tx: Option<broadcast::Sender<()>> = None;

        while let Some(command) = command_rx.recv().await {
            match command {
                ClipboardRuntimeCommand::Start(target) if active_addr == Some(target) => {}
                ClipboardRuntimeCommand::Start(target) => {
                    if let Some(cancel_tx) = active_cancel_tx.take() {
                        let _ = cancel_tx.send(());
                    }
                    *controller_inbound_tx.lock().unwrap() = None;

                    let (inbound_tx, inbound_rx) = mpsc::channel(1024);
                    *controller_inbound_tx.lock().unwrap() = Some(inbound_tx);

                    let (cancel_tx, cancel_rx) = broadcast::channel(1);
                    active_cancel_tx = Some(cancel_tx);
                    active_addr = Some(target);

                    let (scheduled_sender, _worker) = ScheduledDataSender::spawn(
                        udp_sender.clone(),
                        target,
                        ScheduledDataSenderConfig {
                            tick_interval: Duration::from_millis(1),
                            send_budget_per_tick: 16,
                            ..ScheduledDataSenderConfig::default()
                        },
                    );

                    spawn(async move {
                        if let Err(err) = run_clipboard_sync(
                            MacClipboardProvider::new(),
                            scheduled_sender,
                            inbound_rx,
                            cancel_rx,
                            ClipboardSyncRunnerConfig::default(),
                        )
                        .await
                        {
                            eprintln!("Clipboard sync task error: {}", err);
                        }
                    });
                }
                ClipboardRuntimeCommand::Stop => {
                    if let Some(cancel_tx) = active_cancel_tx.take() {
                        let _ = cancel_tx.send(());
                    }
                    active_addr = None;
                    *controller_inbound_tx.lock().unwrap() = None;
                }
            }
        }
    });

    ClipboardRuntimeControl {
        command_tx,
        inbound_tx,
    }
}

fn start_file_transfer_runtime_controller(
    udp_sender: remote_core::net::UdpSender,
) -> FileTransferRuntimeControl {
    let (command_tx, mut command_rx) = mpsc::unbounded_channel();
    let inbound_tx: Arc<Mutex<Option<mpsc::Sender<DataEnvelope>>>> = Arc::new(Mutex::new(None));
    let controller_inbound_tx = inbound_tx.clone();
    let transfer_state = Arc::new(Mutex::new(TransferCenterState::default()));
    let controller_transfer_state = transfer_state.clone();
    let settings = Arc::new(Mutex::new(FileTransferRuntimeSettings::from_env()));
    let controller_settings = settings.clone();

    spawn(async move {
        let mut active_addr: Option<SocketAddr> = None;
        let mut active_cancel_tx: Option<broadcast::Sender<()>> = None;
        let mut active_file_command_tx: Option<mpsc::Sender<FileTransferCommand>> = None;

        while let Some(command) = command_rx.recv().await {
            match command {
                FileTransferRuntimeCommand::Start(target) if active_addr == Some(target) => {}
                FileTransferRuntimeCommand::Start(target) => {
                    if let Some(cancel_tx) = active_cancel_tx.take() {
                        let _ = cancel_tx.send(());
                    }
                    *controller_inbound_tx.lock().unwrap() = None;

                    let (inbound_tx, inbound_rx) = mpsc::channel(1024);
                    *controller_inbound_tx.lock().unwrap() = Some(inbound_tx);

                    let (file_command_tx, file_command_rx) = mpsc::channel(16);
                    active_file_command_tx = Some(file_command_tx.clone());
                    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
                    let file_clipboard_enabled = env_flag_enabled("REMOTE_PLAY_FILE_CLIPBOARD");
                    let (cancel_tx, cancel_rx) = broadcast::channel(1);
                    active_cancel_tx = Some(cancel_tx);
                    active_addr = Some(target);

                    let (scheduled_sender, _worker) = ScheduledDataSender::spawn(
                        udp_sender.clone(),
                        target,
                        ScheduledDataSenderConfig {
                            tick_interval: Duration::from_millis(1),
                            send_budget_per_tick: 16,
                            ..ScheduledDataSenderConfig::default()
                        },
                    );
                    let runtime_config = controller_settings.lock().unwrap().runtime_config();

                    spawn(async move {
                        if let Err(err) = run_file_transfer_runtime(
                            scheduled_sender,
                            file_command_rx,
                            inbound_rx,
                            event_tx,
                            cancel_rx,
                            runtime_config,
                        )
                        .await
                        {
                            eprintln!("File transfer runtime error: {}", err);
                        }
                    });
                    if file_clipboard_enabled {
                        let (log_event_tx, mut log_event_rx) = mpsc::unbounded_channel();
                        let bridge_command_tx = file_command_tx.clone();
                        let bridge_cancel_rx = active_cancel_tx
                            .as_ref()
                            .expect("file cancel tx should be active")
                            .subscribe();
                        spawn(async move {
                            if let Err(err) = run_clipboard_file_sync(
                                MacClipboardProvider::new(),
                                bridge_command_tx,
                                event_rx,
                                Some(log_event_tx),
                                bridge_cancel_rx,
                                ClipboardFileSyncConfig::default(),
                            )
                            .await
                            {
                                eprintln!("File clipboard sync error: {}", err);
                            }
                        });
                        let log_transfer_state = controller_transfer_state.clone();
                        spawn(async move {
                            while let Some(event) = log_event_rx.recv().await {
                                record_and_log_file_transfer_event(
                                    "Client",
                                    &log_transfer_state,
                                    event,
                                );
                            }
                        });
                    } else {
                        let log_transfer_state = controller_transfer_state.clone();
                        spawn(async move {
                            while let Some(event) = event_rx.recv().await {
                                record_and_log_file_transfer_event(
                                    "Client",
                                    &log_transfer_state,
                                    event,
                                );
                            }
                        });
                    }

                    if let Some(path) = std::env::var_os("REMOTE_PLAY_SEND_FILE").map(PathBuf::from)
                    {
                        let _ = file_command_tx
                            .send(FileTransferCommand::SendFile {
                                path,
                                mime_type: None,
                            })
                            .await;
                    }
                }
                FileTransferRuntimeCommand::Stop => {
                    if let Some(cancel_tx) = active_cancel_tx.take() {
                        let _ = cancel_tx.send(());
                    }
                    active_addr = None;
                    active_file_command_tx = None;
                    *controller_inbound_tx.lock().unwrap() = None;
                }
                FileTransferRuntimeCommand::SendFile { path, mime_type } => {
                    if let Some(tx) = &active_file_command_tx {
                        let _ = tx
                            .send(FileTransferCommand::SendFile { path, mime_type })
                            .await;
                    } else {
                        eprintln!(
                            "File transfer send requested before an active target was selected"
                        );
                    }
                }
                FileTransferRuntimeCommand::SendFileGroup { paths } => {
                    if let Some(tx) = &active_file_command_tx {
                        let files = paths
                            .into_iter()
                            .map(|path| FileTransferGroupFile {
                                path,
                                mime_type: None,
                            })
                            .collect();
                        let _ = tx.send(FileTransferCommand::SendFileGroup { files }).await;
                    } else {
                        eprintln!(
                            "File transfer group send requested before an active target was selected"
                        );
                    }
                }
                FileTransferRuntimeCommand::CancelTransfer { transfer_id } => {
                    if let Some(tx) = &active_file_command_tx {
                        let _ = tx
                            .send(FileTransferCommand::CancelTransfer { transfer_id })
                            .await;
                    }
                }
                FileTransferRuntimeCommand::CancelGroup { group_id } => {
                    if let Some(tx) = &active_file_command_tx {
                        let _ = tx.send(FileTransferCommand::CancelGroup { group_id }).await;
                    }
                }
                FileTransferRuntimeCommand::SetReceiveSettings(settings) => {
                    *controller_settings.lock().unwrap() = settings.clone();
                    if let Some(tx) = &active_file_command_tx {
                        let _ = tx
                            .send(FileTransferCommand::SetReceiveConfig {
                                receive_dir: settings.receive_dir.clone(),
                                receive_policy: settings.receive_policy(),
                            })
                            .await;
                    }
                }
            }
        }
    });

    FileTransferRuntimeControl {
        command_tx,
        inbound_tx,
        transfer_state,
        settings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_transfer_runtime_settings_build_receive_config() {
        let settings = FileTransferRuntimeSettings {
            receive_dir: PathBuf::from("/tmp/remote-play-receive"),
            allow_overwrite: true,
        };

        let config = settings.runtime_config();

        assert_eq!(
            config.receive_dir,
            PathBuf::from("/tmp/remote-play-receive")
        );
        assert!(config.receive_policy.allow_overwrite);
    }
}

#[cfg(not(target_os = "macos"))]
fn start_clipboard_runtime_controller(
    _udp_sender: remote_core::net::UdpSender,
) -> ClipboardRuntimeControl {
    let (command_tx, _command_rx) = mpsc::unbounded_channel();
    ClipboardRuntimeControl {
        command_tx,
        inbound_tx: Arc::new(Mutex::new(None)),
    }
}
