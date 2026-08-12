use protocol::{DataEnvelope, PayloadType, RtpHeader, RtpPacket};
use std::hint::black_box;
use std::time::{Duration, Instant};

const DEFAULT_ITERS: usize = 50_000;

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

fn envelope(payload: &[u8]) -> DataEnvelope {
    DataEnvelope::realtime_video(99, 42, 123_456, 123_472, payload.to_vec())
}

fn payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
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
    let data = envelope(&payload);

    let rtp_encoded = rtp.encode().expect("rtp should encode");
    let envelope_encoded = data.encode().expect("envelope should encode");
    let compact_encoded = data
        .encode_compact_realtime()
        .expect("compact realtime should encode");
    let envelope_overhead_bytes = envelope_encoded.len() as isize - rtp_encoded.len() as isize;
    let compact_overhead_bytes = compact_encoded.len() as isize - rtp_encoded.len() as isize;
    let envelope_overhead_pct = if rtp_encoded.is_empty() {
        0.0
    } else {
        envelope_overhead_bytes as f64 / rtp_encoded.len() as f64 * 100.0
    };
    let compact_overhead_pct = if rtp_encoded.is_empty() {
        0.0
    } else {
        compact_overhead_bytes as f64 / rtp_encoded.len() as f64 * 100.0
    };

    let rtp_encode = time_it(
        || {
            let encoded = black_box(&rtp).encode().expect("rtp should encode");
            black_box(encoded);
        },
        iters,
    );
    let envelope_encode = time_it(
        || {
            let encoded = black_box(&data).encode().expect("envelope should encode");
            black_box(encoded);
        },
        iters,
    );
    let compact_encode = time_it(
        || {
            let encoded = black_box(&data)
                .encode_compact_realtime()
                .expect("compact realtime should encode");
            black_box(encoded);
        },
        iters,
    );
    let rtp_decode = time_it(
        || {
            let decoded = RtpPacket::decode(black_box(&rtp_encoded)).expect("rtp should decode");
            black_box(decoded);
        },
        iters,
    );
    let envelope_decode = time_it(
        || {
            let decoded =
                DataEnvelope::decode(black_box(&envelope_encoded)).expect("envelope should decode");
            black_box(decoded);
        },
        iters,
    );
    let compact_decode = time_it(
        || {
            let decoded = DataEnvelope::decode_compact_realtime(black_box(&compact_encoded))
                .expect("compact realtime should decode");
            black_box(decoded);
        },
        iters,
    );

    println!("payload_size={payload_size} bytes");
    println!(
        "  encoded_size: rtp={} envelope={} overhead={} ({:.2}%) compact={} overhead={} ({:.2}%)",
        rtp_encoded.len(),
        envelope_encoded.len(),
        envelope_overhead_bytes,
        envelope_overhead_pct,
        compact_encoded.len(),
        compact_overhead_bytes,
        compact_overhead_pct
    );
    println!(
        "  encode_ns:    rtp={:.1} envelope={:.1} compact={:.1}",
        ns_per_iter(rtp_encode, iters),
        ns_per_iter(envelope_encode, iters),
        ns_per_iter(compact_encode, iters)
    );
    println!(
        "  decode_ns:    rtp={:.1} envelope={:.1} compact={:.1}",
        ns_per_iter(rtp_decode, iters),
        ns_per_iter(envelope_decode, iters),
        ns_per_iter(compact_decode, iters)
    );
}

fn main() {
    let iters = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse::<usize>().ok())
        .unwrap_or(DEFAULT_ITERS);

    println!("transport bench iterations={iters}");
    for payload_size in [64, 512, 1_400, 8_000, 64_000] {
        run_case(payload_size, iters);
    }
}
