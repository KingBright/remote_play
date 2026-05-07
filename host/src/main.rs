#[cfg(target_os = "macos")]
mod audio_capture;
#[cfg(target_os = "macos")]
mod audio_encode;
#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
mod input_injector;
mod sender;
#[cfg(target_os = "macos")]
mod video_encode;

use protocol::{ControlMessage, RtpPacket};
use remote_core::stats::Statistics;
use remote_core::{AudioCapturer, VideoCapturer, VideoEncoder, net::UdpMultiplexer};
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::Instant;

#[cfg(target_os = "macos")]
use capture::MacVideoCapturer;
use remote_core::net::UdpSender;
#[cfg(target_os = "macos")]
use video_encode::MacVideoEncoder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    println!("Host starting in Standby Mode...");

    let stats = Statistics::new();
    Statistics::start_reporter(stats.clone(), "Host", 1);

    let bind_addr: SocketAddr = "0.0.0.0:8000".parse()?;
    let multiplexer = UdpMultiplexer::bind(&bind_addr.to_string()).await?;
    let (udp_sender, mut udp_receiver) = multiplexer.split();

    #[cfg(target_os = "macos")]
    {
        use input_injector::MacInputInjector;
        use remote_core::net::MultiplexedPacket;

        let input_injector = Arc::new(MacInputInjector::new()?);
        let mut active_cancel_tx: Option<broadcast::Sender<()>> = None;
        let mut last_heartbeat = Instant::now();

        println!("Listening for ControlMessages on port 8000...");

        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    // Check heartbeat timeout
                    if active_cancel_tx.is_some() && last_heartbeat.elapsed() > Duration::from_secs(3) {
                        println!("Heartbeat timeout! Stopping stream...");
                        if let Some(tx) = active_cancel_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                }
                recv_res = udp_receiver.recv() => {
                    match recv_res {
                        Ok(MultiplexedPacket::Control(msg, client_addr)) => {
                            match msg {
                                ControlMessage::Input(input_event) => {
                                    input_injector.inject(input_event);
                                }
                                ControlMessage::Heartbeat => {
                                    last_heartbeat = Instant::now();
                                }
                                ControlMessage::StopStream => {
                                    println!("Received StopStream from client. Stopping...");
                                    if let Some(tx) = active_cancel_tx.take() {
                                        let _ = tx.send(());
                                    }
                                }
                                ControlMessage::StartStream { width, height, fps, bitrate_kbps, session_id } => {
                                    println!("Received StartStream from {} with {}x{}@{}fps ({} kbps)", client_addr, width, height, fps, bitrate_kbps);

                                    // Stop existing stream if any
                                    if let Some(tx) = active_cancel_tx.take() {
                                        let _ = tx.send(());
                                    }

                                    last_heartbeat = Instant::now();

                                    let (cancel_tx, cancel_rx) = broadcast::channel(1);
                                    active_cancel_tx = Some(cancel_tx);

                                    let sender_clone = udp_sender.clone();
                                    let stats_clone = stats.clone();

                                    tokio::spawn(async move {
                                        if let Err(e) = run_streaming(
                                            session_id,
                                            client_addr,
                                            width,
                                            height,
                                            fps,
                                            bitrate_kbps,
                                            sender_clone,
                                            stats_clone,
                                            cancel_rx
                                        ).await {
                                            eprintln!("Streaming task error: {}", e);
                                        }
                                        println!("Streaming task stopped.");
                                    });
                                }
                                _ => {}
                            }
                        }
                        Ok(_) => {} // Ignore RTP
                        Err(e) => {
                            eprintln!("UDP Receive Error: {}", e);
                        }
                    }
                }
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        println!("Host implementation is currently macOS only.");
    }

    Ok(())
}

#[cfg(target_os = "macos")]
async fn run_streaming(
    session_id: u32,
    client_addr: SocketAddr,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    udp_sender: UdpSender,
    stats: Arc<Statistics>,
    mut cancel_rx: broadcast::Receiver<()>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // 2. Setup A/V Pipeline
    let mut video_capturer = MacVideoCapturer::new(width, height, fps);
    let mut video_encoder = MacVideoEncoder::new(width, height, fps, bitrate_kbps)?;

    video_capturer.start().await?;
    println!(
        "Video Capture and Encoding started. Streaming to {}...",
        client_addr
    );

    let mut audio_capturer = crate::audio_capture::MacAudioCapturer::new();
    audio_capturer.start().await?;
    println!("Audio Capture started.");

    // Audio Task
    let audio_udp_sender = udp_sender.clone();
    let stats_audio = stats.clone();
    let mut audio_cancel_rx = cancel_rx.resubscribe();

    tokio::spawn(async move {
        use crate::audio_encode::OpusAudioEncoder;
        use remote_core::{AudioCapturer, AudioEncoder, AudioFrame};

        let first_frame = match audio_capturer.capture_frame().await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Failed to capture first audio frame: {}", e);
                return;
            }
        };

        let sample_rate = first_frame.sample_rate();
        let channels = first_frame.channels();

        let mut audio_encoder = match OpusAudioEncoder::new(sample_rate, channels) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Failed to initialize Opus encoder: {}", e);
                return;
            }
        };

        let mut seq_num: u16 = 0;

        if let Ok(encoded) = audio_encoder.encode(first_frame).await {
            let packet = RtpPacket {
                header: protocol::RtpHeader {
                    version: 2,
                    payload_type: 97,
                    sequence_number: seq_num,
                    timestamp: (seq_num as u32) * (sample_rate / 100),
                    ssrc: session_id.wrapping_add(1),
                },
                payload: encoded,
            };
            seq_num = seq_num.wrapping_add(1);
            let _ = audio_udp_sender.send_rtp(&packet, client_addr).await;
        }

        loop {
            tokio::select! {
                _ = audio_cancel_rx.recv() => {
                    break;
                }
                frame_res = audio_capturer.capture_frame() => {
                    let frame = match frame_res {
                        Ok(f) => f,
                        Err(_) => break,
                    };

                    if let Ok(encoded) = audio_encoder.encode(frame).await {
                        if encoded.is_empty() { continue; }
                        stats_audio.audio_frames_encoded.fetch_add(1, Relaxed);
                        stats_audio.audio_bytes_encoded.fetch_add(encoded.len() as u64, Relaxed);

                        let packet = RtpPacket {
                            header: protocol::RtpHeader {
                                version: 2,
                                payload_type: 97,
                                sequence_number: seq_num,
                                timestamp: (seq_num as u32) * (sample_rate / 100),
                                ssrc: session_id.wrapping_add(1),
                            },
                            payload: encoded,
                        };
                        seq_num = seq_num.wrapping_add(1);

                        let packet_size = packet.payload.len() as u64 + 12;
                        if let Err(e) = audio_udp_sender.send_rtp(&packet, client_addr).await {
                            eprintln!("Failed to send audio RTP: {}", e);
                        } else {
                            stats_audio.udp_packets_sent.fetch_add(1, Relaxed);
                            stats_audio.udp_bytes_sent.fetch_add(packet_size, Relaxed);
                        }
                    }
                }
            }
        }
    });

    let mut seq_num: u16 = 0;
    let stats_video = stats.clone();
    let mut capture_timestamps = std::collections::VecDeque::new();

    let mut last_fps_update = std::time::Instant::now();
    let mut frames_since_update = 0;
    let mut bytes_since_update: u64 = 0;
    let mut current_fps = 0.0_f32;
    let mut current_latency_ms = 0.0_f32;
    let mut current_jitter_ms = 0.0_f32;

    let telemetry_udp_sender = udp_sender.clone();

    loop {
        tokio::select! {
            _ = cancel_rx.recv() => {
                break;
            }
            frame_res = video_capturer.capture_frame() => {
                let frame = frame_res?;
                let capture_time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u32;
                capture_timestamps.push_back(capture_time);

                stats_video.video_frames_captured.fetch_add(1, Relaxed);
                video_encoder.submit_frame(frame).await?;
            }
            nalu_res = video_encoder.pull_encoded() => {
                let nalu_bytes = nalu_res?;
                if nalu_bytes.is_empty() {
                    continue;
                }
                stats_video.video_frames_encoded.fetch_add(1, Relaxed);
                stats_video.video_bytes_encoded.fetch_add(nalu_bytes.len() as u64, Relaxed);
                bytes_since_update += nalu_bytes.len() as u64;

                if nalu_bytes.len() > 4 {
                    let nalu_type = (nalu_bytes[4] >> 1) & 0x3F;
                    if (16..=21).contains(&nalu_type) || nalu_type == 32 {
                        stats_video.video_keyframes_encoded.fetch_add(1, Relaxed);
                    }
                }

                let capture_time = capture_timestamps.pop_front().unwrap_or_else(|| {
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u32
                });

                let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u32;
                let latency = now_ms.wrapping_sub(capture_time) as f32;
                let d = (latency - current_latency_ms).abs();
                current_jitter_ms = current_jitter_ms + (d - current_jitter_ms) / 16.0;
                current_latency_ms = latency;
                frames_since_update += 1;

                let now_instant = std::time::Instant::now();
                let elapsed = now_instant.duration_since(last_fps_update).as_secs_f32();
                if elapsed >= 1.0 {
                    current_fps = frames_since_update as f32 / elapsed;
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
                        timestamp: capture_time,
                        ssrc: session_id,
                    },
                    payload: nalu_bytes,
                };
                seq_num = seq_num.wrapping_add(1);

                let packet_size = packet.payload.len() as u64 + 12;
                udp_sender.send_rtp(&packet, client_addr).await?;

                stats_video.udp_packets_sent.fetch_add(1, Relaxed);
                stats_video.udp_bytes_sent.fetch_add(packet_size, Relaxed);
            }
        }
    }
    Ok(())
}
