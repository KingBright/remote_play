#[cfg(target_os = "macos")]
mod audio_capture;
mod audio_encode;
#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "windows")]
mod capture_readiness;
pub mod capture_sources;
mod connection_services;
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
mod video_color;
#[cfg(target_os = "macos")]
mod video_encode;
#[cfg(target_os = "macos")]
mod window_input;

#[cfg(any(test, target_os = "linux", target_os = "windows"))]
mod capture_backend;
#[cfg(any(test, target_os = "linux"))]
mod linux_capture_geometry;
#[cfg(all(test, not(target_os = "linux")))]
mod linux_frame;
#[cfg(target_os = "linux")]
pub mod linux_frame;
#[cfg(all(unix, any(test, target_os = "linux")))]
mod linux_portal;
#[cfg(target_os = "linux")]
pub use linux_portal::{
    SourceKind as PortalSourceKind,
    runtime::{
        PickerSources, PortalCallError, PortalCaptureLease, PreparedPortalCapture,
        SelectedPortalSource, request_local_portal_capture,
    },
};
#[cfg(any(test, target_os = "linux", target_os = "windows"))]
mod ffmpeg_hevc;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub mod linux_audio;
#[cfg(any(test, target_os = "linux"))]
pub mod linux_capture;
#[cfg(target_os = "linux")]
pub mod linux_input;
#[cfg(any(test, target_os = "linux"))]
mod linux_raw_encode;
#[cfg(any(test, target_os = "linux"))]
pub mod linux_video_encode;
#[cfg(target_os = "windows")]
mod windows_capture;
#[cfg(target_os = "windows")]
mod windows_input;
#[cfg(any(target_os = "windows", test))]
mod windows_keymap;
#[cfg(target_os = "windows")]
mod windows_video_encode;

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
use remote_core::mesh::{AppPrivateMeshConfigStore, default_app_private_mesh_dir};
use remote_core::net::DEFAULT_CONTROL_PORT;
use remote_core::scheduled_sender::ScheduledDataSender;
use remote_core::stats::Statistics;
use remote_core::{AudioCapturer, VideoCapturer, VideoEncoder};
pub use service::{HostServiceConfig, run_host_service};

pub fn video_capture_readiness() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        crate::capture_readiness::ffmpeg_program().map(|_| ())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(())
    }
}

use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
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
#[cfg(target_os = "windows")]
use remote_platform::WindowsClipboardProvider;

pub async fn run_host_binary() -> Result<(), Box<dyn Error + Send + Sync>> {
    println!("Host starting in Standby Mode...");
    let _discovery = maybe_start_discovery_runtime(
        "RemotePlay Host",
        DEFAULT_CONTROL_PORT,
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
        enable_clipboard_sync: env_flag_enabled("REMOTE_PLAY_CLIPBOARD_SYNC"),
        enable_file_transfer: env_flag_enabled("REMOTE_PLAY_FILE_TRANSFER"),
        enable_talkback: env_flag_enabled("REMOTE_PLAY_TALKBACK"),
    })
    .await
}

fn env_flag_enabled(name: &str) -> bool {
    env_flag_or(name, false)
}

fn env_flag_or(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"),
        Err(_) => default,
    }
}

fn env_path_or_temp(name: &str, fallback_dir_name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(fallback_dir_name))
}

struct DiscoveryRuntimeHandle {
    cancel_tx: broadcast::Sender<()>,
}

impl Drop for DiscoveryRuntimeHandle {
    fn drop(&mut self) {
        let _ = self.cancel_tx.send(());
    }
}

async fn maybe_start_discovery_runtime(
    display_name: &str,
    control_port: u16,
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
        virtual_ip: None,
        capabilities,
        scope: DiscoveryScope::Lan,
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn log_file_transfer_event(label: &str, event: FileTransferEvent) {
    match event {
        FileTransferEvent::IncomingClipboardReady { .. }
        | FileTransferEvent::SharedResponse { .. } => {}
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
    pub paused: bool,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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
    scheduled_sender: ScheduledDataSender,
    source: protocol::session::CaptureSource,
    stream_settings_rx: Option<watch::Receiver<StreamSettings>>,
    keyframe_requested: Arc<std::sync::atomic::AtomicBool>,
    video_sequence: Arc<std::sync::atomic::AtomicU16>,
    #[cfg(all(unix, any(test, target_os = "linux")))]
    native_capture: Option<linux_portal::runtime::PreparedPortalCapture>,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
async fn send_media_packet(
    udp_sender: &UdpSender,
    scheduled_sender: Option<&ScheduledDataSender>,
    packet: &RtpPacket,
    timing: Option<protocol::FrameTimingCheckpoints>,
    client_addr: SocketAddr,
    use_data_plane_media: bool,
) -> Result<u64, Box<dyn Error + Send + Sync>> {
    if use_data_plane_media || timing.is_some() {
        let envelope = rtp_to_realtime_data(packet)?;
        let extra_len = if timing.is_some() {
            protocol::HOST_TIMING_WIRE_LEN
        } else {
            0
        };
        let packet_size = envelope.payload.len() as u64
            + protocol::COMPACT_REALTIME_HEADER_LEN as u64
            + extra_len as u64;
        let envelope = envelope.with_transport_timing(timing);
        if let Some(scheduled_sender) = scheduled_sender {
            scheduled_sender.send(envelope).await?;
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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

struct AudioCaptureTaskRuntime {
    stream_id: u32,
    udp_sender: UdpSender,
    stats: Arc<Statistics>,
    cancel_rx: broadcast::Receiver<()>,
    client_addr: SocketAddr,
    use_data_plane_media: bool,
    scheduled_sender: Option<ScheduledDataSender>,
    media_settings_rx: Option<watch::Receiver<StreamSettings>>,
}

fn spawn_audio_capture_task<C>(
    label: &'static str,
    mut audio_capturer: C,
    runtime: AudioCaptureTaskRuntime,
) -> tokio::task::JoinHandle<()>
where
    C: AudioCapturer + Send + 'static,
    C::Frame: remote_core::AudioFrame + Send,
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
            mut media_settings_rx,
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

        let mut paused = false;
        loop {
            tokio::select! {
                _ = cancel_rx.recv() => {
                    break;
                }
                changed = async {
                    if let Some(rx) = &mut media_settings_rx {
                        rx.changed().await.map(|_| rx.borrow().paused)
                    } else { std::future::pending().await }
                } => {
                    let Ok(next_paused) = changed else { break };
                    if next_paused != paused {
                        let result = if next_paused { audio_capturer.pause().await }
                            else { audio_capturer.resume().await };
                        if let Err(err) = result {
                            eprintln!("{label} audio pause/resume failed: {err}");
                            break;
                        }
                        // Discard a partial pre-pause packet, preserving stream/sequence IDs.
                        if let Ok(encoder) = OpusAudioEncoder::new(sample_rate, channels) {
                            audio_encoder = encoder;
                        }
                        paused = next_paused;
                    }
                }
                frame_res = audio_capturer.capture_frame(), if !paused => {
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

struct AudioSendContext<'a> {
    udp_sender: &'a UdpSender,
    scheduled_sender: Option<&'a ScheduledDataSender>,
    client_addr: SocketAddr,
    use_data_plane_media: bool,
    stats: &'a Statistics,
    stream_id: u32,
    timestamp_step: u32,
}

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
            None,
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
struct ScheduledStatsReporterGuard {
    handle: tokio::task::JoinHandle<()>,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
impl Drop for ScheduledStatsReporterGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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

struct SubscriptionAudioConfig {
    session_id: u32,
    source: protocol::session::CaptureSource,
    client_addr: SocketAddr,
    udp_sender: UdpSender,
    scheduled_sender: ScheduledDataSender,
    stats: Arc<Statistics>,
    cancel_rx: broadcast::Receiver<()>,
    stream_settings_rx: Option<watch::Receiver<StreamSettings>>,
    include_audio: bool,
    include_microphone: bool,
}

fn start_subscription_audio(config: SubscriptionAudioConfig) -> Vec<tokio::task::JoinHandle<()>> {
    let SubscriptionAudioConfig {
        session_id,
        source,
        client_addr,
        udp_sender,
        scheduled_sender,
        stats,
        cancel_rx,
        stream_settings_rx,
        include_audio,
        include_microphone,
    } = config;
    let scheduled_media_sender = Some(scheduled_sender);
    let use_data_plane_media = true;
    #[cfg(not(target_os = "macos"))]
    let _ = (source, include_audio);
    #[cfg(target_os = "macos")]
    let microphone_audio_task = if include_microphone {
        Some(spawn_audio_capture_task(
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
                media_settings_rx: stream_settings_rx.clone(),
            },
        ))
    } else {
        None
    };

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let microphone_audio_task = if include_microphone {
        Some(spawn_audio_capture_task(
            "Remote microphone",
            crate::linux_audio::LinuxAudioCapturer::microphone(),
            AudioCaptureTaskRuntime {
                stream_id: remote_microphone_audio_stream_id(session_id),
                udp_sender: udp_sender.clone(),
                stats: stats.clone(),
                cancel_rx: cancel_rx.resubscribe(),
                client_addr,
                use_data_plane_media,
                scheduled_sender: scheduled_media_sender.clone(),
                media_settings_rx: stream_settings_rx.clone(),
            },
        ))
    } else {
        None
    };

    #[cfg(target_os = "macos")]
    let system_audio_task = if include_audio {
        if use_data_plane_media {
            Some(spawn_audio_capture_task(
                "Remote system",
                crate::audio_capture::MacSystemAudioCapturer::new().for_source(source),
                AudioCaptureTaskRuntime {
                    stream_id: remote_system_audio_stream_id(session_id),
                    udp_sender: udp_sender.clone(),
                    stats: stats.clone(),
                    cancel_rx: cancel_rx.resubscribe(),
                    client_addr,
                    use_data_plane_media,
                    scheduled_sender: scheduled_media_sender.clone(),
                    media_settings_rx: stream_settings_rx.clone(),
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

    let mut tasks = Vec::new();
    if let Some(task) = microphone_audio_task {
        tasks.push(task);
    }
    #[cfg(target_os = "macos")]
    if let Some(task) = system_audio_task {
        tasks.push(task);
    }
    tasks
}

/// Wait before acquiring any native capture/encoder resource. A paused source
/// switch still reports liveness, but never briefly captures a frame first.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
async fn wait_for_capture_start<F, Fut>(
    settings: &mut Option<watch::Receiver<StreamSettings>>,
    cancel: &mut broadcast::Receiver<()>,
    fallback: StreamSettings,
    mut report_paused: F,
) -> Option<StreamSettings>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if !matches!(
            cancel.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ) {
            return None;
        }
        let Some(rx) = settings.as_mut() else {
            return Some(fallback);
        };
        let current = rx.borrow_and_update().clone();
        if !current.paused {
            return Some(current);
        }
        tokio::select! {
            biased;
            _ = cancel.recv() => return None,
            changed = rx.changed() => { if changed.is_err() { return None; } },
            _ = heartbeat.tick() => report_paused().await,
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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
        scheduled_sender,
        source,
        mut stream_settings_rx,
        keyframe_requested,
        video_sequence,
        #[cfg(all(unix, any(test, target_os = "linux")))]
        native_capture,
    } = config;
    #[cfg(all(unix, test, not(target_os = "linux")))]
    if native_capture.is_some() {
        return Err("native Linux streaming requires a Linux runtime".into());
    }

    let Some(initial) = wait_for_capture_start(
        &mut stream_settings_rx,
        &mut cancel_rx,
        StreamSettings {
            width,
            height,
            fps,
            bitrate_kbps,
            paused: false,
        },
        || async {
            let _ = udp_sender
                .send_control(
                    &protocol::ControlMessage::HostTelemetry {
                        fps: 0.0,
                        encode_latency_ms: 0.0,
                        jitter_ms: 0.0,
                        bitrate_kbps: 0,
                    },
                    client_addr,
                )
                .await;
        },
    )
    .await
    else {
        return Ok(());
    };
    let StreamSettings {
        width,
        height,
        fps,
        bitrate_kbps,
        ..
    } = initial;

    #[cfg(target_os = "macos")]
    let _display_power = display_power::DisplayPowerGuard::activate();

    // 2. Setup A/V Pipeline
    #[cfg(target_os = "macos")]
    let mut video_capturer = MacVideoCapturer::new(width, height, fps).with_source(source);
    #[cfg(target_os = "macos")]
    let mut video_encoder = MacVideoEncoder::new(width, height, fps, bitrate_kbps)?;

    #[cfg(target_os = "linux")]
    let mut video_capturer = match &native_capture {
        Some(capture) if capture.source_info().source == source => {
            LinuxVideoCapturer::from_owned_mailbox(capture.mailbox(), fps)?
        }
        Some(_) => return Err("native capture source does not match the subscription".into()),
        None => LinuxVideoCapturer::new(width, height, fps)?,
    };
    #[cfg(target_os = "linux")]
    let mut video_encoder = match &native_capture {
        Some(capture) => LinuxVideoEncoder::from_owned_frames(
            capture.generation(),
            width,
            height,
            fps,
            bitrate_kbps,
        )?,
        None => LinuxVideoEncoder::new(width, height, fps, bitrate_kbps)?,
    };
    #[cfg(target_os = "linux")]
    let mut native_revoked = native_capture.as_ref().map(|capture| capture.revocation());

    #[cfg(target_os = "windows")]
    let mut video_capturer = crate::windows_capture::WindowsVideoCapturer::new(width, height, fps)?;
    #[cfg(target_os = "windows")]
    let mut video_encoder =
        crate::windows_video_encode::WindowsVideoEncoder::new(width, height, fps, bitrate_kbps)?;

    video_capturer.start().await?;
    println!(
        "Video Capture and Encoding started. Streaming to {}...",
        client_addr
    );

    let use_data_plane_media = true;
    let scheduled_media_sender = Some(scheduled_sender);
    #[cfg(not(target_os = "macos"))]
    let _ = source;

    // Sequence state belongs to the subscription, not to this capture run.
    let stats_video = stats.clone();

    let mut last_fps_update = std::time::Instant::now();
    let mut frames_since_update = 0;
    let mut bytes_since_update: u64 = 0;
    let mut current_latency_ms = 0.0_f32;
    let mut current_jitter_ms = 0.0_f32;

    let telemetry_udp_sender = udp_sender.clone();
    let mut paused = false;
    let mut paused_telemetry = tokio::time::interval(Duration::from_secs(5));
    paused_telemetry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = async {
                #[cfg(target_os = "linux")]
                if let Some(revoked) = &mut native_revoked { loop { if *revoked.borrow() || revoked.changed().await.is_err() { return; } } }
                std::future::pending::<()>().await;
            } => break,
            _ = cancel_rx.recv() => {
                break;
            }
            _ = paused_telemetry.tick(), if paused => {
                let _ = telemetry_udp_sender.send_control(&protocol::ControlMessage::HostTelemetry {
                    fps: 0.0, encode_latency_ms: 0.0, jitter_ms: 0.0, bitrate_kbps: 0,
                }, client_addr).await;
            }
            settings_changed = async {
                if let Some(rx) = &mut stream_settings_rx {
                    rx.changed().await.map(|_| rx.borrow().clone())
                } else {
                    std::future::pending().await
                }
            } => {
                if let Ok(new_settings) = settings_changed {
                    #[cfg(any(target_os = "linux", target_os = "windows"))]
                    {
                        if new_settings.paused { video_encoder.set_paused(true)?; }
                        video_encoder.update_settings(
                            new_settings.width, new_settings.height,
                            new_settings.fps, new_settings.bitrate_kbps,
                        )?;
                        if !new_settings.paused { video_encoder.set_paused(false)?; }
                    }
                    if new_settings.paused != paused {
                        if new_settings.paused { video_capturer.pause().await?; }
                        else {
                            video_capturer.resume().await?;
                            video_encoder.request_keyframe();
                        }
                        paused = new_settings.paused;
                        last_fps_update = std::time::Instant::now();
                        frames_since_update = 0;
                        bytes_since_update = 0;
                    }
                    println!("Applying updated stream settings: {}x{}@{}fps ({} kbps)", new_settings.width, new_settings.height, new_settings.fps, new_settings.bitrate_kbps);
                    #[cfg(target_os = "macos")]
                    {
                        video_capturer.update_resolution_and_fps(new_settings.width, new_settings.height, new_settings.fps)?;
                        video_encoder.update_rate_settings(new_settings.fps, new_settings.bitrate_kbps);
                    }
                    #[cfg(target_os = "linux")]
                    video_capturer.update_resolution_and_fps(new_settings.height, new_settings.fps)?;
                    #[cfg(target_os = "windows")]
                    video_capturer.update_resolution_and_fps(
                        new_settings.width,
                        new_settings.height,
                        new_settings.fps,
                    )?;
                }
            }
            frame_res = video_capturer.capture_frame(), if !paused => {
                let frame = frame_res?;
                stats_video.video_frames_captured.fetch_add(1, Relaxed);
                if keyframe_requested.swap(false, Relaxed) {
                    video_encoder.request_keyframe();
                }
                video_encoder.submit_frame(frame).await?;
            }
            chunk_res = video_encoder.pull_encoded_chunk() => {
                let chunk = chunk_res?;
                #[cfg(target_os = "linux")]
                if native_capture.as_ref().is_some_and(|capture| *capture.revocation().borrow()) { break; }
                if paused || chunk.nalu.is_empty() {
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

                let mut timing = chunk.timing;
                if timing.capture_ts_us == 0 {
                    timing.capture_ts_us = (chunk.capture_time_ms as u64) * 1000;
                }
                let now_us = remote_core::timing::quanta_now_us();
                timing.packetize_ts_us = (now_us.saturating_sub(timing.capture_ts_us)) as u32;
                timing.send_ts_us = timing.packetize_ts_us;

                let packet = RtpPacket {
                    header: protocol::RtpHeader {
                        version: 2,
                        payload_type: 96,
                        sequence_number: video_sequence.fetch_add(1, Relaxed),
                        timestamp: chunk.capture_time_ms,
                        ssrc: session_id,
                    },
                    payload: chunk.nalu,
                };

                let packet_size =
                    send_media_packet(
                        &udp_sender,
                        scheduled_media_sender.as_ref(),
                        &packet,
                        Some(timing),
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

#[cfg(test)]
mod capture_start_lifecycle_tests {
    use super::*;
    fn settings(paused: bool) -> StreamSettings {
        StreamSettings {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 8000,
            paused,
        }
    }
    #[tokio::test]
    async fn initially_paused_capture_waits_for_resume_and_uses_latest_settings() {
        let (tx, rx) = watch::channel(settings(true));
        let (_cancel_tx, mut cancel_rx) = broadcast::channel(1);
        let task = tokio::spawn(async move {
            wait_for_capture_start(&mut Some(rx), &mut cancel_rx, settings(false), || async {})
                .await
        });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        let mut next = settings(false);
        next.width = 1280;
        next.height = 720;
        next.fps = 30;
        tx.send_replace(next.clone());
        assert_eq!(task.await.unwrap(), Some(next));
    }
    #[tokio::test]
    async fn initially_paused_capture_can_be_cancelled_without_starting() {
        let (_tx, rx) = watch::channel(settings(true));
        let (cancel_tx, mut cancel_rx) = broadcast::channel(1);
        let task = tokio::spawn(async move {
            wait_for_capture_start(&mut Some(rx), &mut cancel_rx, settings(false), || async {})
                .await
        });
        tokio::task::yield_now().await;
        cancel_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), None);
    }
    #[tokio::test]
    async fn closed_settings_channel_does_not_start_paused_capture() {
        let (tx, rx) = watch::channel(settings(true));
        drop(tx);
        let (_cancel_tx, mut cancel_rx) = broadcast::channel(1);
        assert_eq!(
            wait_for_capture_start(&mut Some(rx), &mut cancel_rx, settings(false), || async {})
                .await,
            None
        );
    }
}
