use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::Duration;
use tokio::time;

#[derive(Default)]
pub struct Statistics {
    // Video Host
    pub video_frames_captured: AtomicUsize,
    pub video_frames_encoded: AtomicUsize,
    pub video_bytes_encoded: AtomicU64,
    pub video_keyframes_encoded: AtomicUsize,

    // Audio Host
    pub audio_frames_captured: AtomicUsize,
    pub audio_frames_encoded: AtomicUsize,
    pub audio_bytes_encoded: AtomicU64,

    // Network (Host/Client)
    pub udp_packets_sent: AtomicUsize,
    pub udp_bytes_sent: AtomicU64,
    pub udp_packets_recv: AtomicUsize,
    pub udp_bytes_recv: AtomicU64,

    // Video Client
    pub video_jitter_buffer_push: AtomicUsize,
    pub video_jitter_buffer_pop: AtomicUsize,
    pub video_frames_decoded: AtomicUsize,
    pub video_frames_rendered: AtomicUsize,
}

impl Statistics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn start_reporter(stats: Arc<Self>, role: &'static str, interval_secs: u64) {
        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(interval_secs));
            loop {
                interval.tick().await;

                // Take snapshots and reset
                let v_cap = stats.video_frames_captured.swap(0, Relaxed);
                let v_enc = stats.video_frames_encoded.swap(0, Relaxed);
                let v_enc_b = stats.video_bytes_encoded.swap(0, Relaxed);
                let v_k_enc = stats.video_keyframes_encoded.swap(0, Relaxed);

                let a_cap = stats.audio_frames_captured.swap(0, Relaxed);
                let a_enc = stats.audio_frames_encoded.swap(0, Relaxed);
                let a_enc_b = stats.audio_bytes_encoded.swap(0, Relaxed);

                let u_pkt_s = stats.udp_packets_sent.swap(0, Relaxed);
                let u_byte_s = stats.udp_bytes_sent.swap(0, Relaxed);
                let u_pkt_r = stats.udp_packets_recv.swap(0, Relaxed);
                let u_byte_r = stats.udp_bytes_recv.swap(0, Relaxed);

                let v_jb_push = stats.video_jitter_buffer_push.swap(0, Relaxed);
                let v_jb_pop = stats.video_jitter_buffer_pop.swap(0, Relaxed);
                let v_dec = stats.video_frames_decoded.swap(0, Relaxed);
                let v_ren = stats.video_frames_rendered.swap(0, Relaxed);

                println!("=== [{}] Telemetry (Last {}s) ===", role, interval_secs);
                if role == "Host" {
                    println!(
                        " Video | Capture: {:>3} fps | Encode: {:>3} fps | I-Frames: {:>2} | Bandwidth: {:>6.2} KB/s",
                        v_cap,
                        v_enc,
                        v_k_enc,
                        (v_enc_b as f64) / 1024.0 / (interval_secs as f64)
                    );
                    println!(
                        " Audio | Capture: {:>3} fps | Encode: {:>3} fps | Bandwidth: {:>6.2} KB/s",
                        a_cap,
                        a_enc,
                        (a_enc_b as f64) / 1024.0 / (interval_secs as f64)
                    );
                    println!(
                        " Net   | Send: {:>5} pkts | Bandwidth: {:>6.2} KB/s",
                        u_pkt_s,
                        (u_byte_s as f64) / 1024.0 / (interval_secs as f64)
                    );
                } else if role == "Client" {
                    println!(
                        " Net   | Recv: {:>5} pkts | Bandwidth: {:>6.2} KB/s",
                        u_pkt_r,
                        (u_byte_r as f64) / 1024.0 / (interval_secs as f64)
                    );
                    println!(
                        " JBuf  | Push: {:>5} pkt/s | Pop: {:>5} pkt/s",
                        v_jb_push, v_jb_pop
                    );
                    println!(
                        " Video | Decode: {:>3} fps | Render: {:>3} fps",
                        v_dec, v_ren
                    );
                }
                println!("========================================");
            }
        });
    }
}
