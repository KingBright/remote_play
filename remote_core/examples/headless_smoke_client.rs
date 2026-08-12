use protocol::{AudioSource, ContentKind, ControlMessage, PayloadType};
use remote_core::file_transfer::FileReceivePolicy;
use remote_core::file_transfer_runtime::{
    FileTransferCommand, FileTransferEvent, FileTransferRuntimeConfig, run_file_transfer_runtime,
};
use remote_core::media_plane::{audio_stream_config_from_envelope, realtime_data_to_rtp};
use remote_core::net::{DEFAULT_CONTROL_PORT, MultiplexedPacket, UdpMultiplexer};
use remote_core::scheduled_sender::{ScheduledDataSender, ScheduledDataSenderConfig};
use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

#[derive(Debug, Default)]
struct SmokeStats {
    legacy_video: u64,
    legacy_audio: u64,
    data_video: u64,
    data_audio: u64,
    data_audio_configs: u64,
    data_remote_microphone_configs: u64,
    data_remote_system_configs: u64,
    telemetry: u64,
    file_completed: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let host_addr = env_socket_addr(
        "REMOTE_PLAY_SMOKE_HOST_ADDR",
        SocketAddr::from(([127, 0, 0, 1], DEFAULT_CONTROL_PORT)),
    )?;
    let duration = Duration::from_secs(env_u64("REMOTE_PLAY_SMOKE_SECONDS", 8));
    let receive_dir = env_path_or_temp(
        "REMOTE_PLAY_SMOKE_RECEIVE_DIR",
        "remote-play-headless-smoke-received-files",
    );
    let expect_data_plane_media = env_flag_enabled("REMOTE_PLAY_EXPECT_DATA_PLANE_MEDIA");
    let expect_audio = env_flag_enabled("REMOTE_PLAY_EXPECT_AUDIO");
    let expect_system_audio = env_flag_enabled("REMOTE_PLAY_EXPECT_SYSTEM_AUDIO");
    let expect_file_name = std::env::var("REMOTE_PLAY_EXPECT_FILE_NAME").ok();

    tokio::fs::create_dir_all(&receive_dir).await?;

    let multiplexer = UdpMultiplexer::bind("0.0.0.0:0").await?;
    let local_addr = multiplexer.local_addr()?;
    let (udp_sender, udp_receiver) = multiplexer.split();
    println!("Headless smoke client listening on {local_addr}, host={host_addr}");

    let (scheduled_sender, _scheduled_worker) = ScheduledDataSender::spawn(
        udp_sender.clone(),
        host_addr,
        ScheduledDataSenderConfig::default(),
    );
    let (_file_command_tx, file_command_rx) = mpsc::channel::<FileTransferCommand>(4);
    let (file_inbound_tx, file_inbound_rx) = mpsc::channel(1024);
    let (file_event_tx, mut file_event_rx) = mpsc::unbounded_channel();
    let (cancel_tx, _) = broadcast::channel(4);

    let file_runtime = tokio::spawn(run_file_transfer_runtime(
        scheduled_sender,
        file_command_rx,
        file_inbound_rx,
        file_event_tx,
        cancel_tx.subscribe(),
        FileTransferRuntimeConfig {
            receive_dir: receive_dir.clone(),
            receive_policy: FileReceivePolicy {
                allow_overwrite: true,
                ..FileReceivePolicy::default()
            },
            ..FileTransferRuntimeConfig::default()
        },
    ));

    let heartbeat = tokio::spawn({
        let udp_sender = udp_sender.clone();
        let mut cancel_rx = cancel_tx.subscribe();
        async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    _ = cancel_rx.recv() => break,
                    _ = interval.tick() => {
                        let _ = udp_sender.send_control(&ControlMessage::Heartbeat, host_addr).await;
                    }
                }
            }
        }
    });

    udp_sender
        .send_control(
            &ControlMessage::StartStream {
                width: env_u32("REMOTE_PLAY_SMOKE_WIDTH", 1280),
                height: env_u32("REMOTE_PLAY_SMOKE_HEIGHT", 720),
                fps: env_u32("REMOTE_PLAY_SMOKE_FPS", 30),
                bitrate_kbps: env_u32("REMOTE_PLAY_SMOKE_BITRATE_KBPS", 4_000),
                session_id: rand::random::<u32>(),
            },
            host_addr,
        )
        .await?;

    let mut stats = SmokeStats::default();
    let deadline = tokio::time::Instant::now() + duration;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            packet = udp_receiver.recv() => {
                match packet {
                    Ok(packet) => handle_packet(packet, &file_inbound_tx, &mut stats).await?,
                    Err(err) => eprintln!("Smoke client receive error: {err}"),
                }
            }
            maybe_event = file_event_rx.recv() => {
                if let Some(event) = maybe_event {
                    handle_file_event(event, &mut stats);
                }
            }
        }
    }

    let _ = udp_sender
        .send_control(&ControlMessage::StopStream, host_addr)
        .await;
    let _ = cancel_tx.send(());
    heartbeat.abort();
    file_runtime
        .await
        .expect("file runtime task should join")
        .expect("file runtime should stop cleanly");

    println!(
        "Smoke summary: legacy_video={} legacy_audio={} data_video={} data_audio={} data_audio_configs={} remote_mic_configs={} remote_system_configs={} telemetry={} file_completed={}",
        stats.legacy_video,
        stats.legacy_audio,
        stats.data_video,
        stats.data_audio,
        stats.data_audio_configs,
        stats.data_remote_microphone_configs,
        stats.data_remote_system_configs,
        stats.telemetry,
        stats
            .file_completed
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "none".to_string())
    );

    validate_stats(
        &stats,
        expect_data_plane_media,
        expect_audio,
        expect_system_audio,
        expect_file_name.as_deref(),
    )?;
    Ok(())
}

async fn handle_packet(
    packet: MultiplexedPacket,
    file_inbound_tx: &mpsc::Sender<protocol::DataEnvelope>,
    stats: &mut SmokeStats,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match packet {
        MultiplexedPacket::Rtp(packet, _) => match packet.header.payload_type {
            payload_type if payload_type == PayloadType::VideoH265 as u8 => stats.legacy_video += 1,
            payload_type if payload_type == PayloadType::AudioOpus as u8 => stats.legacy_audio += 1,
            _ => {}
        },
        MultiplexedPacket::Control(ControlMessage::HostTelemetry { .. }, _) => {
            stats.telemetry += 1;
        }
        MultiplexedPacket::Control(_, _) => {}
        MultiplexedPacket::Data(envelope, _) => match envelope.header.kind {
            ContentKind::VideoH265 => {
                let _ = realtime_data_to_rtp(envelope)?;
                stats.data_video += 1;
            }
            ContentKind::AudioOpus => {
                let _ = realtime_data_to_rtp(envelope)?;
                stats.data_audio += 1;
            }
            ContentKind::AudioStreamConfig => {
                let config = audio_stream_config_from_envelope(&envelope)?;
                println!(
                    "Smoke Audio | config stream={} source={:?} direction={:?} {}Hz {}ch {}ms",
                    config.stream_id,
                    config.source,
                    config.direction,
                    config.sample_rate_hz,
                    config.channels,
                    config.frame_duration_ms
                );
                stats.data_audio_configs += 1;
                match config.source {
                    AudioSource::RemoteMicrophone => stats.data_remote_microphone_configs += 1,
                    AudioSource::RemoteSystem => stats.data_remote_system_configs += 1,
                    _ => {}
                }
            }
            ContentKind::FileManifest | ContentKind::FileChunk | ContentKind::FileControl => {
                file_inbound_tx.send(envelope).await?;
            }
            _ => {}
        },
    }
    Ok(())
}

fn handle_file_event(event: FileTransferEvent, stats: &mut SmokeStats) {
    match event {
        FileTransferEvent::IncomingCompleted { path, .. } => {
            println!("Smoke FileTx | incoming completed: {}", path.display());
            stats.file_completed = Some(path);
        }
        FileTransferEvent::IncomingGroupCompleted { paths, .. } => {
            if let Some(path) = paths.into_iter().next() {
                println!(
                    "Smoke FileTx | incoming group completed: {}",
                    path.display()
                );
                stats.file_completed = Some(path);
            }
        }
        FileTransferEvent::Error { message, .. } => {
            eprintln!("Smoke FileTx | error: {message}");
        }
        _ => {}
    }
}

fn validate_stats(
    stats: &SmokeStats,
    expect_data_plane_media: bool,
    expect_audio: bool,
    expect_system_audio: bool,
    expect_file_name: Option<&str>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if expect_data_plane_media {
        if stats.data_video == 0 {
            return Err("expected data-plane video packets, observed none".into());
        }
    } else if stats.data_video + stats.legacy_video == 0 {
        return Err("expected video packets, observed none".into());
    }

    if expect_audio {
        if expect_data_plane_media {
            if stats.data_audio == 0 {
                return Err("expected data-plane audio packets, observed none".into());
            }
            if stats.data_audio_configs == 0 {
                return Err("expected data-plane audio stream config, observed none".into());
            }
            if stats.data_remote_microphone_configs == 0 {
                return Err("expected remote microphone audio stream config, observed none".into());
            }
        } else if stats.data_audio + stats.legacy_audio == 0 {
            return Err("expected audio packets, observed none".into());
        }
    }

    if expect_system_audio && stats.data_remote_system_configs == 0 {
        return Err("expected remote system audio stream config, observed none".into());
    }

    if let Some(expected_name) = expect_file_name {
        let Some(path) = &stats.file_completed else {
            return Err(format!("expected incoming file {expected_name}, observed none").into());
        };
        let Some(actual_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Err(format!("incoming path has no file name: {}", path.display()).into());
        };
        if actual_name != expected_name {
            return Err(format!("expected file {expected_name}, received {actual_name}").into());
        }
    }

    Ok(())
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_socket_addr(
    name: &str,
    default: SocketAddr,
) -> Result<SocketAddr, Box<dyn Error + Send + Sync>> {
    Ok(std::env::var(name)
        .map(|value| value.parse())
        .unwrap_or(Ok(default))?)
}

fn env_path_or_temp(name: &str, fallback_dir_name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(fallback_dir_name))
}
