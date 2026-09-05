use protocol::{DataEnvelope, PayloadType, RtpHeader, RtpPacket};
use remote_core::net::{MultiplexedPacket, UdpMultiplexer};
use std::time::{Duration, Instant};
use tokio::time::timeout;

const DEFAULT_ITERS: usize = 5_000;

fn payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

fn rtp_packet(payload: &[u8], sequence_number: u16) -> RtpPacket {
    RtpPacket {
        header: RtpHeader {
            version: 2,
            payload_type: PayloadType::VideoH265 as u8,
            sequence_number,
            timestamp: sequence_number as u32,
            ssrc: 99,
        },
        payload: payload.to_vec(),
    }
}

fn data_envelope(payload: &[u8], sequence_number: u64) -> DataEnvelope {
    DataEnvelope::realtime_video(
        99,
        sequence_number,
        sequence_number,
        sequence_number + 16,
        payload.to_vec(),
    )
}

fn ns_per_iter(duration: Duration, iters: usize) -> f64 {
    duration.as_nanos() as f64 / iters as f64
}

async fn bench_rtp(payload_size: usize, iters: usize) -> Duration {
    let sender_mux = UdpMultiplexer::bind("127.0.0.1:0")
        .await
        .expect("sender should bind");
    let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
        .await
        .expect("receiver should bind");
    let target = receiver_mux
        .local_addr()
        .expect("receiver should have local addr");
    let (sender, _) = sender_mux.split();
    let (_, receiver) = receiver_mux.split();
    let payload = payload(payload_size);

    let start = Instant::now();
    for i in 0..iters {
        let packet = rtp_packet(&payload, i as u16);
        sender
            .send_rtp(&packet, target)
            .await
            .expect("rtp send should succeed");

        match timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("rtp receive should not time out")
            .expect("rtp receive should succeed")
        {
            MultiplexedPacket::Rtp(_, _) => {}
            other => panic!("unexpected packet: {other:?}"),
        }
    }
    start.elapsed()
}

async fn bench_compact_data(payload_size: usize, iters: usize) -> Duration {
    let sender_mux = UdpMultiplexer::bind("127.0.0.1:0")
        .await
        .expect("sender should bind");
    let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
        .await
        .expect("receiver should bind");
    let target = receiver_mux
        .local_addr()
        .expect("receiver should have local addr");
    let (sender, _) = sender_mux.split();
    let (_, receiver) = receiver_mux.split();
    let payload = payload(payload_size);

    let start = Instant::now();
    for i in 0..iters {
        let envelope = data_envelope(&payload, i as u64);
        sender
            .send_data(&envelope, target)
            .await
            .expect("data send should succeed");

        match timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("data receive should not time out")
            .expect("data receive should succeed")
        {
            MultiplexedPacket::Data(_, _) | MultiplexedPacket::DataWithTiming(_, _, _) => {}
            other => panic!("unexpected packet: {other:?}"),
        }
    }
    start.elapsed()
}

#[tokio::main]
async fn main() {
    let iters = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse::<usize>().ok())
        .unwrap_or(DEFAULT_ITERS);

    println!("loopback transport bench iterations={iters}");
    println!("data path uses UdpSender::send_data, which selects compact realtime wire format");
    for payload_size in [64, 512, 1_400, 8_000] {
        let rtp = bench_rtp(payload_size, iters).await;
        let data = bench_compact_data(payload_size, iters).await;
        println!("payload_size={payload_size} bytes");
        println!("  rtp_ns_per_packet={:.1}", ns_per_iter(rtp, iters));
        println!(
            "  compact_data_ns_per_packet={:.1}",
            ns_per_iter(data, iters)
        );
    }
}
