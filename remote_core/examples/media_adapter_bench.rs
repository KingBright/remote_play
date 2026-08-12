use protocol::{DataEnvelope, PayloadType, RtpHeader, RtpPacket};
use remote_core::media_plane::{realtime_data_to_rtp, rtp_to_realtime_data};
use std::hint::black_box;
use std::time::{Duration, Instant};

const DEFAULT_ITERS: usize = 50_000;

fn payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

fn rtp_packet(payload: &[u8]) -> RtpPacket {
    RtpPacket {
        header: RtpHeader {
            version: 2,
            payload_type: PayloadType::VideoH265 as u8,
            sequence_number: 42,
            timestamp: 123_456,
            ssrc: 99,
        },
        payload: payload.to_vec(),
    }
}

fn time_it(mut f: impl FnMut(), iters: usize) -> Duration {
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    start.elapsed()
}

fn ns_per_iter(duration: Duration, iters: usize) -> f64 {
    duration.as_nanos() as f64 / iters as f64
}

fn run_case(payload_size: usize, iters: usize) {
    let payload = payload(payload_size);
    let rtp = rtp_packet(&payload);
    let rtp_encoded = rtp.encode().expect("rtp should encode");
    let envelope = rtp_to_realtime_data(&rtp).expect("rtp should adapt");
    let compact_encoded = envelope
        .encode_compact_realtime()
        .expect("compact realtime should encode");

    let legacy_roundtrip = time_it(
        || {
            let encoded = black_box(&rtp).encode().expect("rtp should encode");
            let decoded = RtpPacket::decode(black_box(&encoded)).expect("rtp should decode");
            black_box(decoded);
        },
        iters,
    );

    let data_plane_roundtrip = time_it(
        || {
            let envelope = rtp_to_realtime_data(black_box(&rtp)).expect("rtp should adapt");
            let encoded = envelope
                .encode_compact_realtime()
                .expect("compact realtime should encode");
            let decoded_envelope = DataEnvelope::decode_compact_realtime(black_box(&encoded))
                .expect("compact realtime should decode");
            let decoded_rtp =
                realtime_data_to_rtp(decoded_envelope).expect("data should adapt back to rtp");
            black_box(decoded_rtp);
        },
        iters,
    );

    println!("payload_size={payload_size} bytes");
    println!(
        "  encoded_size: rtp={} compact_data={}",
        rtp_encoded.len(),
        compact_encoded.len()
    );
    println!(
        "  full_roundtrip_ns: legacy_rtp={:.1} data_plane_adapter={:.1}",
        ns_per_iter(legacy_roundtrip, iters),
        ns_per_iter(data_plane_roundtrip, iters)
    );
}

fn main() {
    let iters = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse::<usize>().ok())
        .unwrap_or(DEFAULT_ITERS);

    println!("media adapter bench iterations={iters}");
    for payload_size in [64, 512, 1_400, 8_000, 64_000] {
        run_case(payload_size, iters);
    }
}
