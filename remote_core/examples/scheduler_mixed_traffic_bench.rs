use protocol::DataEnvelope;
use remote_core::data_plane::{
    LaneScheduler, LaneSchedulerConfig, ObjectChunkSpec, RealtimePacketSpec, SchedulerPushResult,
    make_realtime_envelope, object_chunk_envelope,
};

const DEFAULT_TICKS: u64 = 10_000;
const DEFAULT_BULK_PER_TICK: u64 = 4;
const DEFAULT_SEND_BUDGET_PER_TICK: u64 = 2;
const DEFAULT_REALTIME_EVERY_TICKS: u64 = 2;
const DEFAULT_REALTIME_DEADLINE_DELTA: u64 = 8;

#[derive(Debug, Default)]
struct BenchStats {
    realtime_generated: u64,
    realtime_sent: u64,
    realtime_wait_sum: u64,
    realtime_wait_max: u64,
    bulk_generated: u64,
    bulk_sent: u64,
    rejected_bulk: u64,
}

fn realtime_packet(sequence_number: u64, now_ms: u64) -> DataEnvelope {
    make_realtime_envelope(
        RealtimePacketSpec::video(
            1,
            sequence_number,
            now_ms,
            now_ms + DEFAULT_REALTIME_DEADLINE_DELTA,
        ),
        vec![0x55; 512],
    )
}

fn bulk_packet(sequence_number: u64, now_ms: u64) -> DataEnvelope {
    object_chunk_envelope(
        ObjectChunkSpec {
            object_id: sequence_number,
            stream_id: 7,
            sequence_number,
            timestamp_ms: now_ms,
            chunk_index: 0,
            total_chunks: 1,
            offset: 0,
            total_size: 1_024,
            checksum_crc32: None,
        },
        vec![0xaa; 1_024],
    )
}

fn parse_arg(index: usize, default: u64) -> u64 {
    std::env::args()
        .nth(index)
        .and_then(|arg| arg.parse::<u64>().ok())
        .unwrap_or(default)
}

fn main() {
    let ticks = parse_arg(1, DEFAULT_TICKS);
    let bulk_per_tick = parse_arg(2, DEFAULT_BULK_PER_TICK);
    let send_budget_per_tick = parse_arg(3, DEFAULT_SEND_BUDGET_PER_TICK);
    let realtime_every_ticks = parse_arg(4, DEFAULT_REALTIME_EVERY_TICKS).max(1);

    let mut scheduler = LaneScheduler::with_config(LaneSchedulerConfig {
        max_realtime_queued: 64,
        max_reliable_queued: 128,
    });
    let mut stats = BenchStats::default();
    let mut sequence_number = 1;

    for now_ms in 0..ticks {
        for _ in 0..bulk_per_tick {
            let envelope = bulk_packet(sequence_number, now_ms);
            sequence_number += 1;
            stats.bulk_generated += 1;
            if scheduler.push(envelope, now_ms) == SchedulerPushResult::RejectedReliableForCapacity
            {
                stats.rejected_bulk += 1;
            }
        }

        if now_ms % realtime_every_ticks == 0 {
            let envelope = realtime_packet(sequence_number, now_ms);
            sequence_number += 1;
            stats.realtime_generated += 1;
            let _ = scheduler.push(envelope, now_ms);
        }

        for _ in 0..send_budget_per_tick {
            let Some(envelope) = scheduler.pop_next(now_ms) else {
                break;
            };

            if envelope.header.lane.is_realtime() {
                stats.realtime_sent += 1;
                let wait = now_ms.saturating_sub(envelope.header.timestamp_ms);
                stats.realtime_wait_sum += wait;
                stats.realtime_wait_max = stats.realtime_wait_max.max(wait);
            } else {
                stats.bulk_sent += 1;
            }
        }
    }

    while let Some(envelope) = scheduler.pop_next(ticks) {
        if envelope.header.lane.is_realtime() {
            stats.realtime_sent += 1;
            let wait = ticks.saturating_sub(envelope.header.timestamp_ms);
            stats.realtime_wait_sum += wait;
            stats.realtime_wait_max = stats.realtime_wait_max.max(wait);
        } else {
            stats.bulk_sent += 1;
        }
    }

    let scheduler_stats = scheduler.stats();
    let realtime_avg_wait = if stats.realtime_sent == 0 {
        0.0
    } else {
        stats.realtime_wait_sum as f64 / stats.realtime_sent as f64
    };

    println!("scheduler mixed traffic bench");
    println!("  ticks={ticks}");
    println!("  bulk_per_tick={bulk_per_tick}");
    println!("  send_budget_per_tick={send_budget_per_tick}");
    println!("  realtime_every_ticks={realtime_every_ticks}");
    println!("  realtime_generated={}", stats.realtime_generated);
    println!("  realtime_sent={}", stats.realtime_sent);
    println!("  realtime_avg_wait_ticks={realtime_avg_wait:.3}");
    println!("  realtime_max_wait_ticks={}", stats.realtime_wait_max);
    println!("  bulk_generated={}", stats.bulk_generated);
    println!("  bulk_sent={}", stats.bulk_sent);
    println!("  bulk_rejected={}", stats.rejected_bulk);
    println!(
        "  scheduler_dropped_stale_realtime={}",
        scheduler_stats.dropped_stale_realtime
    );
    println!(
        "  scheduler_dropped_realtime_capacity={}",
        scheduler_stats.dropped_realtime_capacity
    );
    println!(
        "  scheduler_rejected_reliable_capacity={}",
        scheduler_stats.rejected_reliable_capacity
    );
}
