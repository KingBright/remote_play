mod audio_player;
mod host_list;
mod jitter_buffer;
mod render;
mod video_decode;

use crate::video_decode::MacDecodedVideoFrame;
use jitter_buffer::JitterBuffer;
use remote_core::net::{MultiplexedPacket, UdpMultiplexer};
use remote_core::stats::Statistics;
use std::env;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex, RwLock};
use tokio::spawn;

#[derive(Default, Clone)]
pub struct HostStats {
    pub fps: f32,
    pub latency: f32,
    pub jitter: f32,
    pub bitrate_kbps: u32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    println!("Client starting...");

    let stats = Statistics::new();
    Statistics::start_reporter(stats.clone(), "Client", 1);

    let host_stats = Arc::new(std::sync::RwLock::new(HostStats::default()));
    let host_stats_udp = host_stats.clone();

    // 1. Setup Network (Multiplexer & Receiver)
    let bind_addr: SocketAddr = "0.0.0.0:0".parse()?;
    let multiplexer = UdpMultiplexer::bind(&bind_addr.to_string()).await?;
    let udp_receiver = multiplexer.split().1;

    // 2. Start Receiver Loop
    let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(100);

    // Initialize AudioPlayer
    let _audio_player = crate::audio_player::AudioPlayer::new(audio_rx)?;

    let stats_net = stats.clone();
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
    let receiver_session_id = active_session_id.clone();

    spawn(async move {
        println!("Client listening on {}", bind_addr);
        let mut video_jitter_buffer = JitterBuffer::new(0);
        let mut video_expected_seq_init = false;
        let mut last_video_ssrc = 0;

        loop {
            match udp_receiver.recv().await {
                Ok(MultiplexedPacket::Rtp(packet, _addr)) => {
                    let packet_size = packet.payload.len() as u64 + 12;
                    stats_net.udp_packets_recv.fetch_add(1, Relaxed);
                    stats_net.udp_bytes_recv.fetch_add(packet_size, Relaxed);

                    let current_session = receiver_session_id.load(Relaxed);
                    if current_session != 0 {
                        if packet.header.payload_type == 97 && packet.header.ssrc != current_session.wrapping_add(1) {
                            continue;
                        }
                        if packet.header.payload_type == 96 && packet.header.ssrc != current_session {
                            continue;
                        }
                    }

                    if packet.header.payload_type == 97 {
                        let _ = audio_tx.send(packet).await;
                    } else if packet.header.payload_type == 96 {
                        if packet.header.ssrc != last_video_ssrc {
                            last_video_ssrc = packet.header.ssrc;
                            video_expected_seq_init = false;
                        }

                        if !video_expected_seq_init {
                            video_jitter_buffer = JitterBuffer::new(packet.header.sequence_number);
                            video_expected_seq_init = true;
                        }
                        video_jitter_buffer.push(packet);
                        stats_net.video_jitter_buffer_push.fetch_add(1, Relaxed);

                        while let Some(ordered_pkt) = video_jitter_buffer.pop() {
                            let recv_time = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u32;
                            stats_net.video_jitter_buffer_pop.fetch_add(1, Relaxed);
                            // Send to decode loop without blocking
                            let _ = decode_tx.try_send((ordered_pkt, recv_time));
                        }
                    }
                }
                Ok(MultiplexedPacket::Control(msg, _addr)) => {
                    if let protocol::ControlMessage::HostTelemetry {
                        fps,
                        encode_latency_ms,
                        jitter_ms,
                        bitrate_kbps,
                    } = msg
                    {
                        let mut hs = host_stats_udp.write().unwrap();
                        hs.fps = fps;
                        hs.latency = encode_latency_ms;
                        hs.jitter = jitter_ms;
                        hs.bitrate_kbps = bitrate_kbps;
                    }
                }
                Err(e) => {
                    eprintln!("UDP Receive Error: {}", e);
                }
            }
        }
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
    let udp_sender = multiplexer.split().0;
    render::run_client(udp_sender, render_shared_frame, active_session_id, host_stats).await?;

    Ok(())
}
