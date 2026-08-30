#[cfg(target_os = "macos")]
mod audio_capture;
#[cfg(target_os = "macos")]
mod audio_encode;
#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
mod display_power;
#[cfg(target_os = "macos")]
mod input_injector;
#[allow(dead_code)]
mod sender;
pub mod service;
#[cfg(target_os = "macos")]
mod talkback_player;
#[cfg(target_os = "macos")]
mod video_encode;

#[cfg(target_os = "linux")]
pub mod linux_audio;
#[cfg(target_os = "linux")]
pub mod linux_capture;
#[cfg(target_os = "linux")]
pub mod linux_input;
#[cfg(target_os = "linux")]
pub mod linux_video_encode;

use protocol::{
    AudioSource, AudioStreamConfig, DataEnvelope, RtpPacket, remote_microphone_audio_stream_id,
    remote_system_audio_stream_id,
};
use remote_core::clipboard_runtime::{ClipboardSyncRunnerConfig, run_clipboard_sync};
use remote_core::discovery::{
    DEFAULT_PEER_TTL, DiscoveryAnnouncement, DiscoveryCapabilities, DiscoveryEvent,
    DiscoveryPeerSnapshot, DiscoveryRuntimeConfig, DiscoveryScope, REMOTE_PLAY_DISCOVERY_ENV,
    discovery_port_from_env, run_discovery_runtime,
};
use remote_core::file_transfer_runtime::{
    FileTransferCommand, FileTransferEvent, FileTransferRuntimeConfig, run_file_transfer_runtime,
};
use remote_core::media_plane::{audio_stream_config_to_envelope, rtp_to_realtime_data};
use remote_core::mesh::{
    AppPrivateMeshConfigStore, EasyTierBinaryLocator, EasyTierCliProbeConfig,
    EasyTierHealthMonitorConfig, EasyTierHealthMonitorHandle, EasyTierSidecarManager,
    REMOTE_PLAY_MESH_ENV, default_app_private_mesh_dir, spawn_easytier_health_monitor,
};
use remote_core::net::DEFAULT_CONTROL_PORT;
use remote_core::scheduled_sender::{
    ScheduledDataSendError, ScheduledDataSender, ScheduledDataSenderConfig,
};
use remote_core::stats::Statistics;
use remote_core::{AudioCapturer, VideoCapturer, VideoEncoder};
pub use service::{HostServiceConfig, run_host_service};
use std::error::Error;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, watch};

#[cfg(target_os = "macos")]
use capture::MacVideoCapturer;
use remote_core::net::UdpSender;
#[cfg(target_os = "macos")]
use remote_platform::MacClipboardProvider;
#[cfg(target_os = "macos")]
use video_encode::MacVideoEncoder;

#[cfg(target_os = "linux")]
use linux_capture::LinuxVideoCapturer;
#[cfg(target_os = "linux")]
use linux_video_encode::LinuxVideoEncoder;
#[cfg(target_os = "linux")]
use remote_platform::LinuxClipboardProvider;

pub async fn run_host_binary() -> Result<(), Box<dyn Error + Send + Sync>> {
    println!("Host starting in Standby Mode...");
    let _mesh_runtime = maybe_start_mesh_sidecar("RemotePlay Host").await;
    let discovery_virtual_ip = _mesh_runtime
        .as_ref()
        .and_then(|runtime| runtime.virtual_ip);
    let _discovery = maybe_start_discovery_runtime(
        "RemotePlay Host",
        DEFAULT_CONTROL_PORT,
        discovery_virtual_ip,
        DiscoveryCapabilities {
            can_stream: true,
            can_view: false,
            file_transfer: env_flag_enabled("REMOTE_PLAY_FILE_TRANSFER")
                || env_flag_enabled("REMOTE_PLAY_FILE_CLIPBOARD")
                || std::env::var_os("REMOTE_PLAY_HOST_SEND_FILE").is_some(),
            clipboard_sync: env_flag_enabled("REMOTE_PLAY_CLIPBOARD_SYNC"),
            talkback: env_flag_enabled("REMOTE_PLAY_TALKBACK"),
        },
    )
    .await;

    let stats = Statistics::new();
    Statistics::start_reporter(stats.clone(), "Host", 1);

    run_host_service(HostServiceConfig {
        bind_addr: SocketAddr::from(([0, 0, 0, 0], DEFAULT_CONTROL_PORT)),
        stats,
    })
    .await
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

struct DiscoveryRuntimeHandle {
    cancel_tx: broadcast::Sender<()>,
}

struct MeshRuntimeHandle {
    _monitor: EasyTierHealthMonitorHandle,
    virtual_ip: Option<IpAddr>,
}

impl Drop for DiscoveryRuntimeHandle {
    fn drop(&mut self) {
        let _ = self.cancel_tx.send(());
    }
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
    let (snapshot_tx, _snapshot_rx) = watch::channel(DiscoveryPeerSnapshot::default());
    let (cancel_tx, cancel_rx) = broadcast::channel(1);

    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            log_discovery_event("Host", event);
        }
    });

    tokio::spawn(async move {
        if let Err(err) = run_discovery_runtime(config, events_tx, snapshot_tx, cancel_rx).await {
            eprintln!("Host discovery runtime stopped: {err}");
        }
    });

    println!("Host discovery enabled on UDP port {discovery_port}.");
    Some(DiscoveryRuntimeHandle { cancel_tx })
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

async fn maybe_start_mesh_sidecar(display_name: &str) -> Option<MeshRuntimeHandle> {
    if !env_flag_enabled(REMOTE_PLAY_MESH_ENV) {
        return None;
    }

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
            Some(MeshRuntimeHandle {
                _monitor: monitor,
                virtual_ip,
            })
        }
        Err(err) => {
            eprintln!("EasyTier sidecar failed to start: {err}");
            None
        }
    }
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct StreamingRunConfig {
    session_id: u32,
    client_addr: SocketAddr,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    udp_sender: UdpSender,
    stats: Arc<Statistics>,
    cancel_rx: broadcast::Receiver<()>,
    clipboard_inbound_rx: Option<mpsc::Receiver<DataEnvelope>>,
    file_inbound_rx: Option<mpsc::Receiver<DataEnvelope>>,
    talkback_inbound_rx: Option<mpsc::Receiver<DataEnvelope>>,
    #[cfg(target_os = "macos")]
    talkback_settings_rx: Option<watch::Receiver<crate::talkback_player::TalkbackPlaybackSettings>>,
    #[cfg(not(target_os = "macos"))]
    talkback_settings_rx: Option<watch::Receiver<()>>,
    stream_settings_rx: Option<watch::Receiver<StreamSettings>>,
    host_send_file: Option<PathBuf>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn send_media_packet(
    udp_sender: &UdpSender,
    scheduled_sender: Option<&ScheduledDataSender>,
    packet: &RtpPacket,
    client_addr: SocketAddr,
    use_data_plane_media: bool,
) -> Result<u64, Box<dyn Error + Send + Sync>> {
    if use_data_plane_media {
        let envelope = rtp_to_realtime_data(packet)?;
        let packet_size =
            envelope.payload.len() as u64 + protocol::COMPACT_REALTIME_HEADER_LEN as u64;
        if let Some(scheduled_sender) = scheduled_sender {
            match scheduled_sender.try_send(envelope) {
                Ok(()) => {}
                Err(ScheduledDataSendError::Full) => return Ok(0),
                Err(err @ ScheduledDataSendError::Closed) => return Err(Box::new(err)),
            }
        } else {
            udp_sender.send_data(&envelope, client_addr).await?;
        }
        Ok(packet_size)
    } else {
        let packet_size = packet.payload.len() as u64 + 12;
        udp_sender.send_rtp(packet, client_addr).await?;
        Ok(packet_size)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn send_audio_stream_config(
    udp_sender: &UdpSender,
    scheduled_sender: Option<&ScheduledDataSender>,
    config: &AudioStreamConfig,
    client_addr: SocketAddr,
    use_data_plane_media: bool,
) -> Result<u64, Box<dyn Error + Send + Sync>> {
    if !use_data_plane_media {
        return Ok(0);
    }

    let envelope = audio_stream_config_to_envelope(config, 0, now_ms())?;
    let packet_size = envelope.payload.len() as u64 + protocol::COMPACT_REALTIME_HEADER_LEN as u64;
    if let Some(scheduled_sender) = scheduled_sender {
        scheduled_sender.send(envelope).await?;
    } else {
        udp_sender.send_data(&envelope, client_addr).await?;
    }
    Ok(packet_size)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn host_audio_stream_config(
    stream_id: u32,
    source: AudioSource,
    sample_rate: u32,
    channels: u16,
    frame_duration_ms: u16,
) -> AudioStreamConfig {
    match source {
        AudioSource::RemoteSystem => {
            AudioStreamConfig::remote_system(stream_id, sample_rate, channels, frame_duration_ms)
        }
        AudioSource::RemoteMicrophone => AudioStreamConfig::remote_microphone(
            stream_id,
            sample_rate,
            channels,
            frame_duration_ms,
        ),
        AudioSource::RemoteMixed => {
            AudioStreamConfig::remote_mixed(stream_id, sample_rate, channels, frame_duration_ms)
        }
        AudioSource::ViewerMicrophoneTalkback => AudioStreamConfig::viewer_microphone_talkback(
            stream_id,
            sample_rate,
            channels,
            frame_duration_ms,
        ),
    }
}

#[cfg(target_os = "macos")]
struct AudioCaptureTaskRuntime {
    stream_id: u32,
    udp_sender: UdpSender,
    stats: Arc<Statistics>,
    cancel_rx: broadcast::Receiver<()>,
    client_addr: SocketAddr,
    use_data_plane_media: bool,
    scheduled_sender: Option<ScheduledDataSender>,
}

#[cfg(target_os = "macos")]
fn spawn_audio_capture_task<C>(
    label: &'static str,
    mut audio_capturer: C,
    runtime: AudioCaptureTaskRuntime,
) -> tokio::task::JoinHandle<()>
where
    C: AudioCapturer<Frame = crate::audio_capture::MacAudioFrame> + Send + 'static,
{
    tokio::spawn(async move {
        use crate::audio_encode::OpusAudioEncoder;
        use remote_core::AudioFrame;

        let AudioCaptureTaskRuntime {
            stream_id,
            udp_sender,
            stats,
            mut cancel_rx,
            client_addr,
            use_data_plane_media,
            scheduled_sender,
        } = runtime;

        if let Err(err) = audio_capturer.start().await {
            eprintln!("{label} audio capture failed to start: {}", err);
            return;
        }
        println!("{label} audio capture started.");

        let first_frame = match audio_capturer.capture_frame().await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Failed to capture first {label} audio frame: {}", e);
                let _ = audio_capturer.stop().await;
                return;
            }
        };
        stats.audio_frames_captured.fetch_add(1, Relaxed);

        let sample_rate = first_frame.sample_rate();
        let channels = first_frame.channels();

        let mut audio_encoder = match OpusAudioEncoder::new(sample_rate, channels) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Failed to initialize {label} Opus encoder: {}", e);
                let _ = audio_capturer.stop().await;
                return;
            }
        };

        let mut seq_num: u16 = 0;
        let timestamp_step = u32::try_from(audio_encoder.samples_per_packet_per_channel())
            .unwrap_or(sample_rate / 50);
        let audio_stream_config = host_audio_stream_config(
            stream_id,
            first_frame.source(),
            sample_rate,
            channels,
            audio_encoder.frame_duration_ms(),
        );
        match send_audio_stream_config(
            &udp_sender,
            scheduled_sender.as_ref(),
            &audio_stream_config,
            client_addr,
            use_data_plane_media,
        )
        .await
        {
            Ok(packet_size) if packet_size > 0 => {
                stats.udp_packets_sent.fetch_add(1, Relaxed);
                stats.udp_bytes_sent.fetch_add(packet_size, Relaxed);
            }
            Ok(_) => {}
            Err(err) => eprintln!("Failed to send {label} audio stream config: {}", err),
        }
        let audio_send_context = AudioSendContext {
            udp_sender: &udp_sender,
            scheduled_sender: scheduled_sender.as_ref(),
            client_addr,
            use_data_plane_media,
            stats: &stats,
            stream_id,
            timestamp_step,
        };

        let first_packets = match audio_encoder.encode_packets(&first_frame) {
            Ok(packets) => packets,
            Err(err) => {
                eprintln!("Failed to encode first {label} audio frame: {}", err);
                Vec::new()
            }
        };
        for encoded in first_packets {
            if let Err(err) = audio_send_context.send(&mut seq_num, encoded).await {
                eprintln!("Failed to send first {label} audio media packet: {}", err);
            }
        }

        loop {
            tokio::select! {
                _ = cancel_rx.recv() => {
                    break;
                }
                frame_res = audio_capturer.capture_frame() => {
                    let frame = match frame_res {
                        Ok(f) => f,
                        Err(_) => break,
                    };
                    stats.audio_frames_captured.fetch_add(1, Relaxed);

                    match audio_encoder.encode_packets(&frame) {
                        Ok(packets) => {
                            if packets.is_empty() {
                                continue;
                            }
                            for encoded in packets {
                                if let Err(err) =
                                    audio_send_context.send(&mut seq_num, encoded).await
                                {
                                    eprintln!("Failed to send {label} audio media packet: {}", err);
                                }
                            }
                        }
                        Err(err) => eprintln!("Failed to encode {label} audio frame: {}", err),
                    }
                }
            }
        }

        let _ = audio_capturer.stop().await;
    })
}

#[cfg(target_os = "macos")]
struct AudioSendContext<'a> {
    udp_sender: &'a UdpSender,
    scheduled_sender: Option<&'a ScheduledDataSender>,
    client_addr: SocketAddr,
    use_data_plane_media: bool,
    stats: &'a Statistics,
    stream_id: u32,
    timestamp_step: u32,
}

#[cfg(target_os = "macos")]
impl AudioSendContext<'_> {
    async fn send(
        &self,
        seq_num: &mut u16,
        encoded: Vec<u8>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.stats.audio_frames_encoded.fetch_add(1, Relaxed);
        self.stats
            .audio_bytes_encoded
            .fetch_add(encoded.len() as u64, Relaxed);

        let packet = RtpPacket {
            header: protocol::RtpHeader {
                version: 2,
                payload_type: 97,
                sequence_number: *seq_num,
                timestamp: (*seq_num as u32) * self.timestamp_step,
                ssrc: self.stream_id,
            },
            payload: encoded,
        };
        *seq_num = (*seq_num).wrapping_add(1);

        let packet_size = send_media_packet(
            self.udp_sender,
            self.scheduled_sender,
            &packet,
            self.client_addr,
            self.use_data_plane_media,
        )
        .await?;

        if packet_size > 0 {
            self.stats.udp_packets_sent.fetch_add(1, Relaxed);
            self.stats.udp_bytes_sent.fetch_add(packet_size, Relaxed);
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct ScheduledStatsReporterGuard {
    handle: tokio::task::JoinHandle<()>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for ScheduledStatsReporterGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn start_scheduled_sender_reporter(
    scheduled_sender: ScheduledDataSender,
    mut cancel_rx: broadcast::Receiver<()>,
) -> ScheduledStatsReporterGuard {
    let handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));

        // Tokio intervals tick immediately; skip that so the first sample is meaningful.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = cancel_rx.recv() => {
                    break;
                }
                _ = interval.tick() => {
                    let stats = scheduled_sender.stats();
                    println!(
                        " DataP | Queue: {:>4} | Enqueued: {:>6} | Full/Closed: {:>4}/{:>3}",
                        stats.entrance_queued,
                        stats.entrance_enqueued,
                        stats.entrance_full,
                        stats.entrance_closed
                    );
                    println!(
                        " DataP | Drop stale/cap: {:>4}/{:>4} | Reject: {:>4} | Sent rt/rel: {:>6}/{:>6} | Errors: {:>3}",
                        stats.scheduler_dropped_stale_realtime,
                        stats.scheduler_dropped_realtime_capacity,
                        stats.scheduler_rejected_reliable_capacity,
                        stats.sent_realtime,
                        stats.sent_reliable,
                        stats.send_errors
                    );
                }
            }
        }
    });

    ScheduledStatsReporterGuard { handle }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn run_streaming(config: StreamingRunConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    let StreamingRunConfig {
        session_id,
        client_addr,
        width,
        height,
        fps,
        bitrate_kbps,
        udp_sender,
        stats,
        mut cancel_rx,
        clipboard_inbound_rx,
        file_inbound_rx,
        talkback_inbound_rx,
        talkback_settings_rx,
        mut stream_settings_rx,
        host_send_file,
    } = config;

    #[cfg(target_os = "macos")]
    let _display_power = display_power::DisplayPowerGuard::activate();

    // 2. Setup A/V Pipeline
    #[cfg(target_os = "macos")]
    let mut video_capturer = MacVideoCapturer::new(width, height, fps);
    #[cfg(target_os = "macos")]
    let mut video_encoder = MacVideoEncoder::new(width, height, fps, bitrate_kbps)?;

    #[cfg(target_os = "linux")]
    let mut video_capturer = LinuxVideoCapturer::new(width, height, fps)?;
    #[cfg(target_os = "linux")]
    let mut video_encoder = LinuxVideoEncoder::new(width, height, fps, bitrate_kbps)?;

    video_capturer.start().await?;
    println!(
        "Video Capture and Encoding started. Streaming to {}...",
        client_addr
    );

    let use_data_plane_media = env_flag_enabled("REMOTE_PLAY_DATA_PLANE_MEDIA");
    if use_data_plane_media {
        println!("Media data-plane adapter enabled.");
    }
    let use_clipboard_sync = clipboard_inbound_rx.is_some();
    if use_clipboard_sync {
        println!("Clipboard data-plane sync enabled.");
    }
    let use_file_transfer = file_inbound_rx.is_some();
    if use_file_transfer {
        println!("File transfer data-plane runtime enabled.");
    }
    let use_talkback = talkback_inbound_rx.is_some();
    if use_talkback {
        println!("Viewer talkback playback enabled.");
    }
    let use_scheduled_sender = use_data_plane_media || use_clipboard_sync || use_file_transfer;
    let (scheduled_media_sender, _scheduled_media_worker) = if use_scheduled_sender {
        let (sender, worker) = ScheduledDataSender::spawn(
            udp_sender.clone(),
            client_addr,
            ScheduledDataSenderConfig {
                send_budget_per_tick: 64,
                tick_interval: Duration::from_millis(1),
                ..ScheduledDataSenderConfig::default()
            },
        );
        (Some(sender), Some(worker))
    } else {
        (None, None)
    };
    let _scheduled_stats_reporter = scheduled_media_sender
        .clone()
        .map(|sender| start_scheduled_sender_reporter(sender, cancel_rx.resubscribe()));
    let _clipboard_sync_task = if let (Some(inbound_rx), Some(sender)) =
        (clipboard_inbound_rx, scheduled_media_sender.clone())
    {
        let clipboard_cancel_rx = cancel_rx.resubscribe();
        Some(tokio::spawn(async move {
            #[cfg(target_os = "macos")]
            let provider = MacClipboardProvider::new();
            #[cfg(target_os = "linux")]
            let provider = LinuxClipboardProvider::new();

            if let Err(err) = run_clipboard_sync(
                provider,
                sender,
                inbound_rx,
                clipboard_cancel_rx,
                ClipboardSyncRunnerConfig::default(),
            )
            .await
            {
                eprintln!("Clipboard sync task error: {}", err);
            }
        }))
    } else {
        None
    };
    let (_file_command_tx_guard, _file_transfer_task, _file_event_logger) =
        if let (Some(inbound_rx), Some(sender)) = (file_inbound_rx, scheduled_media_sender.clone())
        {
            let (command_tx, command_rx) = mpsc::channel(16);
            let (event_tx, mut event_rx) = mpsc::unbounded_channel();
            let file_clipboard_enabled = env_flag_enabled("REMOTE_PLAY_FILE_CLIPBOARD");
            let receive_dir = env_path_or_temp(
                "REMOTE_PLAY_FILE_RECEIVE_DIR",
                "remote-play-host-received-files",
            );
            let runtime_cancel_rx = cancel_rx.resubscribe();
            let runtime_task = tokio::spawn(async move {
                if let Err(err) = run_file_transfer_runtime(
                    sender,
                    command_rx,
                    inbound_rx,
                    event_tx,
                    runtime_cancel_rx,
                    FileTransferRuntimeConfig {
                        receive_dir,
                        ..FileTransferRuntimeConfig::default()
                    },
                )
                .await
                {
                    eprintln!("File transfer task error: {}", err);
                }
            });
            let logger_cancel_rx = cancel_rx.resubscribe();
            let event_logger = tokio::spawn(async move {
                let mut cancel_rx = logger_cancel_rx;
                loop {
                    tokio::select! {
                        _ = cancel_rx.recv() => break,
                        event = event_rx.recv() => {
                            let Some(event) = event else { break };
                            log_file_transfer_event("Host", event);
                        }
                    }
                }
            });
            let command_tx_guard = if let Some(send_file) = host_send_file {
                let auto_send_tx = command_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    println!(
                        "Host auto-enqueueing send file on session startup: {}",
                        send_file.display()
                    );
                    if let Err(err) = auto_send_tx
                        .send(FileTransferCommand::SendFile {
                            path: send_file,
                            mime_type: None,
                        })
                        .await
                    {
                        eprintln!("Failed to enqueue initial host send file: {}", err);
                    }
                });
                Some(command_tx)
            } else if file_clipboard_enabled {
                Some(command_tx)
            } else {
                Some(command_tx)
            };
            (command_tx_guard, Some(runtime_task), Some(event_logger))
        } else {
            (None, None, None)
        };

    #[cfg(target_os = "macos")]
    let _talkback_player = if let (Some(inbound_rx), Some(settings_rx)) =
        (talkback_inbound_rx, talkback_settings_rx)
    {
        match crate::talkback_player::TalkbackPlayer::new(session_id, inbound_rx, settings_rx) {
            Ok(player) => Some(player),
            Err(err) => {
                eprintln!("Failed to initialize viewer talkback playback: {}", err);
                None
            }
        }
    } else {
        None
    };

    #[cfg(target_os = "macos")]
    let _microphone_audio_task = spawn_audio_capture_task(
        "Remote microphone",
        crate::audio_capture::MacAudioCapturer::microphone(),
        AudioCaptureTaskRuntime {
            stream_id: remote_microphone_audio_stream_id(session_id),
            udp_sender: udp_sender.clone(),
            stats: stats.clone(),
            cancel_rx: cancel_rx.resubscribe(),
            client_addr,
            use_data_plane_media,
            scheduled_sender: scheduled_media_sender.clone(),
        },
    );

    #[cfg(target_os = "macos")]
    let _system_audio_task = if env_flag_enabled("REMOTE_PLAY_SYSTEM_AUDIO") {
        if use_data_plane_media {
            Some(spawn_audio_capture_task(
                "Remote system",
                crate::audio_capture::MacSystemAudioCapturer::new(),
                AudioCaptureTaskRuntime {
                    stream_id: remote_system_audio_stream_id(session_id),
                    udp_sender: udp_sender.clone(),
                    stats: stats.clone(),
                    cancel_rx: cancel_rx.resubscribe(),
                    client_addr,
                    use_data_plane_media,
                    scheduled_sender: scheduled_media_sender.clone(),
                },
            ))
        } else {
            eprintln!(
                "REMOTE_PLAY_SYSTEM_AUDIO requires REMOTE_PLAY_DATA_PLANE_MEDIA=1 so the stream config can be delivered."
            );
            None
        }
    } else {
        None
    };

    let mut seq_num: u16 = 0;
    let stats_video = stats.clone();

    let mut last_fps_update = std::time::Instant::now();
    let mut frames_since_update = 0;
    let mut bytes_since_update: u64 = 0;
    let mut current_latency_ms = 0.0_f32;
    let mut current_jitter_ms = 0.0_f32;

    let telemetry_udp_sender = udp_sender.clone();

    loop {
        tokio::select! {
            _ = cancel_rx.recv() => {
                break;
            }
            settings_changed = async {
                if let Some(rx) = &mut stream_settings_rx {
                    rx.changed().await.map(|_| rx.borrow().clone())
                } else {
                    std::future::pending().await
                }
            } => {
                if let Ok(new_settings) = settings_changed {
                    println!("Applying updated stream settings: {}x{}@{}fps ({} kbps)", new_settings.width, new_settings.height, new_settings.fps, new_settings.bitrate_kbps);
                    let _ = video_capturer.update_resolution_and_fps(new_settings.height, new_settings.fps);
                    video_encoder.update_settings(new_settings.width, new_settings.height, new_settings.fps, new_settings.bitrate_kbps);
                }
            }
            frame_res = video_capturer.capture_frame() => {
                let frame = frame_res?;
                stats_video.video_frames_captured.fetch_add(1, Relaxed);
                video_encoder.submit_frame(frame).await?;
            }
            chunk_res = video_encoder.pull_encoded_chunk() => {
                let chunk = chunk_res?;
                if chunk.nalu.is_empty() {
                    continue;
                }
                stats_video.video_frames_encoded.fetch_add(1, Relaxed);
                stats_video.video_bytes_encoded.fetch_add(chunk.nalu.len() as u64, Relaxed);
                bytes_since_update += chunk.nalu.len() as u64;

                if chunk.is_keyframe {
                    stats_video.video_keyframes_encoded.fetch_add(1, Relaxed);
                }

                // 真实的单帧平滑耗时（EMA指数平滑，初始值为首帧耗时）
                let cost = chunk.encode_cost_ms.clamp(0.1, 100.0);
                if current_latency_ms <= 0.01 {
                    current_latency_ms = cost;
                } else {
                    current_latency_ms = current_latency_ms * 0.9 + cost * 0.1;
                }
                let d = (cost - current_latency_ms).abs();
                current_jitter_ms = current_jitter_ms * 0.9 + d * 0.1;
                frames_since_update += 1;

                let now_instant = std::time::Instant::now();
                let elapsed = now_instant.duration_since(last_fps_update).as_secs_f32();
                if elapsed >= 1.0 {
                    let current_fps = frames_since_update as f32 / elapsed;
                    let current_bitrate_kbps = ((bytes_since_update as f32 * 8.0) / 1000.0 / elapsed) as u32;
                    frames_since_update = 0;
                    bytes_since_update = 0;
                    last_fps_update = now_instant;

                    let msg = protocol::ControlMessage::HostTelemetry {
                        fps: current_fps,
                        encode_latency_ms: current_latency_ms,
                        jitter_ms: current_jitter_ms,
                        bitrate_kbps: current_bitrate_kbps,
                    };
                    let _ = telemetry_udp_sender.send_control(&msg, client_addr).await;
                }

                let packet = RtpPacket {
                    header: protocol::RtpHeader {
                        version: 2,
                        payload_type: 96,
                        sequence_number: seq_num,
                        timestamp: chunk.capture_time_ms,
                        ssrc: session_id,
                    },
                    payload: chunk.nalu,
                };
                seq_num = seq_num.wrapping_add(1);

                let packet_size =
                    send_media_packet(
                        &udp_sender,
                        scheduled_media_sender.as_ref(),
                        &packet,
                        client_addr,
                        use_data_plane_media,
                    )
                    .await?;

                if packet_size > 0 {
                    stats_video.udp_packets_sent.fetch_add(1, Relaxed);
                    stats_video.udp_bytes_sent.fetch_add(packet_size, Relaxed);
                }
            }
        }
    }
    Ok(())
}
