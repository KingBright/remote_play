use protocol::{FrameTimingCheckpoints, RtpHeader, RtpPacket, StageId};
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use remote_core::jitter_buffer::JitterBuffer;
use remote_core::timing::{
    ClientFrameTracker, ClockSynchronizer, HighPrecisionClock, HostFrameTracker, quanta_now,
    quanta_now_us,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

fn make_packet(sequence_number: u16, timestamp_us: u64) -> RtpPacket {
    RtpPacket {
        header: RtpHeader {
            version: 2,
            payload_type: 96,
            sequence_number,
            timestamp: (timestamp_us / 1000) as u32,
            ssrc: 0x12345678,
        },
        payload: sequence_number.to_be_bytes().to_vec(),
    }
}

// -----------------------------------------------------------------------------
// Part 1: HighPrecisionClock & Timing Probe Microbenchmarks
// -----------------------------------------------------------------------------

fn run_single_probe_benchmark(iters: usize) {
    println!("\n=== [1.1] HighPrecisionClock & Single Probe Overhead Benchmark ===");
    let clock = HighPrecisionClock::new();

    // Warmup
    for _ in 0..100_000 {
        std::hint::black_box(clock.now());
        std::hint::black_box(clock.now_epoch_us());
        std::hint::black_box(quanta_now_us());
    }

    // 1. clock.now() (quanta Instant / TSC)
    let start = Instant::now();
    for _ in 0..iters {
        let t = clock.now();
        std::hint::black_box(t);
    }
    let elapsed = start.elapsed();
    let ns_per_now = elapsed.as_nanos() as f64 / iters as f64;
    println!(
        "clock.now() (TSC Instant): total={:?}, iters={}, avg={:.2} ns/op",
        elapsed, iters, ns_per_now
    );

    // 2. clock.now_epoch_us() (Microsecond epoch computation)
    let start = Instant::now();
    for _ in 0..iters {
        let us = clock.now_epoch_us();
        std::hint::black_box(us);
    }
    let elapsed = start.elapsed();
    let ns_per_epoch_us = elapsed.as_nanos() as f64 / iters as f64;
    println!(
        "clock.now_epoch_us(): total={:?}, iters={}, avg={:.2} ns/op",
        elapsed, iters, ns_per_epoch_us
    );

    // 3. global_clock() / quanta_now_us() (Global static OnceLock lookup + epoch us)
    let start = Instant::now();
    for _ in 0..iters {
        let us = quanta_now_us();
        std::hint::black_box(us);
    }
    let elapsed = start.elapsed();
    let ns_per_global_us = elapsed.as_nanos() as f64 / iters as f64;
    println!(
        "quanta_now_us() (Global): total={:?}, iters={}, avg={:.2} ns/op",
        elapsed, iters, ns_per_global_us
    );

    // 4. quanta_now() (Global static Instant)
    let start = Instant::now();
    for _ in 0..iters {
        let inst = quanta_now();
        std::hint::black_box(inst);
    }
    let elapsed = start.elapsed();
    let ns_per_global_inst = elapsed.as_nanos() as f64 / iters as f64;
    println!(
        "quanta_now() (Global TSC): total={:?}, iters={}, avg={:.2} ns/op",
        elapsed, iters, ns_per_global_inst
    );

    // Latency Distribution Sampling (Percentiles)
    let sample_count = 1_000_000;
    let mut latencies_ns = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let t0 = Instant::now();
        let _ = std::hint::black_box(quanta_now_us());
        let dt = t0.elapsed().as_nanos() as u64;
        latencies_ns.push(dt);
    }
    latencies_ns.sort_unstable();
    let p50 = latencies_ns[sample_count * 50 / 100];
    let p95 = latencies_ns[sample_count * 95 / 100];
    let p99 = latencies_ns[sample_count * 99 / 100];
    let p999 = latencies_ns[sample_count * 999 / 1000];
    let max = latencies_ns[sample_count - 1];
    let min = latencies_ns[0];

    println!(
        "quanta_now_us() Distribution (N={}): min={}ns, p50={}ns, p95={}ns, p99={}ns, p99.9={}ns, max={}ns",
        sample_count, min, p50, p95, p99, p999, max
    );

    assert!(
        ns_per_now < 100.0,
        "clock.now() overhead exceeded 100ns: {:.2}ns",
        ns_per_now
    );
    assert!(
        ns_per_epoch_us < 100.0,
        "clock.now_epoch_us() overhead exceeded 100ns: {:.2}ns",
        ns_per_epoch_us
    );
    assert!(
        ns_per_global_us < 100.0,
        "quanta_now_us() overhead exceeded 100ns: {:.2}ns",
        ns_per_global_us
    );
    println!(
        ">>> [PASS] Single probe sampling overhead strictly < 100ns (Verified TSC range: {:.2}~{:.2}ns)",
        ns_per_now, ns_per_epoch_us
    );
}

fn run_multithreaded_probe_contention_benchmark(num_threads: usize, iters_per_thread: usize) {
    println!(
        "\n=== [1.2] Multi-Threaded Probe Contention Stress ({} Threads, {} ops/thread) ===",
        num_threads, iters_per_thread
    );

    let running = Arc::new(AtomicBool::new(false));
    let total_ops = Arc::new(AtomicU64::new(0));

    let mut handles = Vec::new();
    for thread_idx in 0..num_threads {
        let r = Arc::clone(&running);
        let ops = Arc::clone(&total_ops);
        handles.push(thread::spawn(move || {
            while !r.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            let mut count = 0u64;
            for _ in 0..iters_per_thread {
                let us = quanta_now_us();
                std::hint::black_box(us);
                count += 1;
            }
            ops.fetch_add(count, Ordering::Relaxed);
            thread_idx
        }));
    }

    let start = Instant::now();
    running.store(true, Ordering::Release);

    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed();
    let ops_done = total_ops.load(Ordering::Relaxed);
    let throughput_mops = (ops_done as f64 / elapsed.as_secs_f64()) / 1_000_000.0;
    let avg_ns = (elapsed.as_nanos() as f64) / (ops_done as f64);

    println!(
        "Concurrent Probes: total_ops={}, elapsed={:?}, throughput={:.2} Mops/sec, per-probe={:.2} ns",
        ops_done, elapsed, throughput_mops, avg_ns
    );
    assert!(
        avg_ns < 100.0,
        "Concurrent probe cost exceeded 100ns: {:.2}ns",
        avg_ns
    );
    println!(">>> [PASS] High-concurrency probe contention test passed");
}

fn run_full_12_probes_frame_lifecycle_benchmark(frames: usize) {
    println!(
        "\n=== [1.3] Full 12 Checkpoints Lifecycle & Serialization Benchmark ({} frames) ===",
        frames
    );
    let clock = HighPrecisionClock::new();

    let start = Instant::now();

    for frame_id in 0..frames {
        // --- 1. Host side: S1 ~ S3 (5 probes) ---
        let mut host_tracker = HostFrameTracker::start(clock.clone()); // Probe 1: S1 Capture
        host_tracker.mark_encode_queue(); // Probe 2: S2a Encode Queue
        host_tracker.mark_encode_done(); // Probe 3: S2b Encode Done
        host_tracker.mark_packetize(); // Probe 4: S3a Packetize
        host_tracker.mark_send(); // Probe 5: S3b Send
        let host_cp = host_tracker.finish();

        // Wire serialization (Host 24B & Full 52B)
        let host_wire = host_cp.to_host_wire_bytes();
        let client_init_cp = FrameTimingCheckpoints::from_host_wire_bytes(&host_wire).unwrap();

        // --- 2. Client side: S4 ~ S8 (7 probes) ---
        let mut client_tracker = ClientFrameTracker::start(client_init_cp, clock.clone(), 500); // Probe 6: S4/S5 Recv
        client_tracker.mark_jitter_enter(); // Probe 7: S5->S6 Jitter Enter
        client_tracker.mark_jitter_exit(); // Probe 8: S6 Jitter Exit
        client_tracker.mark_decode_enter(); // Probe 9: S7a Decode Enter
        client_tracker.mark_decode_done(); // Probe 10: S7b Decode Done
        client_tracker.mark_render_submit(); // Probe 11: S8a Render Submit
        client_tracker.mark_render_done(); // Probe 12: S8b Render Done
        let final_cp = client_tracker.finish();

        // Full wire roundtrip
        let full_wire = final_cp.to_wire_bytes();
        let decoded_cp = FrameTimingCheckpoints::from_wire_bytes(&full_wire).unwrap();

        // Stage durations & Chrome TraceSpan generation
        let _dur1 = decoded_cp.stage_duration_us(StageId::Capture);
        let _dur2 = decoded_cp.stage_duration_us(StageId::HardwareEncode);
        let _e2e = decoded_cp.total_e2e_duration_us();
        let spans = decoded_cp.to_trace_spans(frame_id as u64, 1);
        std::hint::black_box(spans);
    }

    let elapsed = start.elapsed();
    let us_per_frame = (elapsed.as_micros() as f64) / (frames as f64);
    let ns_per_frame = (elapsed.as_nanos() as f64) / (frames as f64);

    println!(
        "Total 12-Probe Full Lifecycle + Codec + Spans: total={:?}, frames={}, avg={:.3} µs/frame ({:.1} ns/frame)",
        elapsed, frames, us_per_frame, ns_per_frame
    );

    assert!(
        us_per_frame < 50.0,
        "Single frame 12-probe lifecycle overhead exceeded 50µs: {:.3}µs",
        us_per_frame
    );
    println!(
        ">>> [PASS] Full 12-probe lifecycle cost strictly < 50µs/frame (Achieved: {:.3} µs/frame)",
        us_per_frame
    );
}

// -----------------------------------------------------------------------------
// Part 2: JitterBuffer Stress, Reordering, Packet Loss & Queue Squeeze
// -----------------------------------------------------------------------------

fn run_jitter_buffer_reordering_stress(num_packets: usize, max_jitter_window: usize) {
    println!(
        "\n=== [2.1] JitterBuffer Reordering & Out-of-Order Stress Test (N={}, Window={}) ===",
        num_packets, max_jitter_window
    );
    let mut jb = JitterBuffer::new(0);

    // Create packets in order
    let base_time = quanta_now_us();
    let mut packets: Vec<(RtpPacket, FrameTimingCheckpoints)> = (0..num_packets as u16)
        .map(|seq| {
            let ts = base_time + seq as u64 * 16_666; // ~60fps (16.6ms)
            let pkt = make_packet(seq, ts);
            let mut timing = FrameTimingCheckpoints::new(ts);
            timing.recv_ts_us = (seq as u32) * 50;
            (pkt, timing)
        })
        .collect();

    // Keep seq 0 as stream init, shuffle remaining packets within sliding window
    let mut rng = StdRng::seed_from_u64(42);
    for chunk in packets[1..].chunks_mut(max_jitter_window) {
        chunk.shuffle(&mut rng);
    }

    let mut popped_seqs = Vec::new();
    let mut total_residency_us = 0u64;

    for (pkt, timing) in packets {
        jb.push_with_timing(pkt, timing);

        while let Some((popped_pkt, popped_timing)) = jb.pop_with_timing() {
            let seq = popped_pkt.header.sequence_number;
            popped_seqs.push(seq);

            // Verify residency timing
            assert!(
                popped_timing.jitter_exit_ts_us >= popped_timing.jitter_enter_ts_us,
                "Jitter exit timestamp ({}) must be >= jitter enter timestamp ({})",
                popped_timing.jitter_exit_ts_us,
                popped_timing.jitter_enter_ts_us
            );
            let residency = popped_timing
                .jitter_pacing_duration_us()
                .unwrap_or_default();
            total_residency_us += residency as u64;
        }
    }

    // Drain remainder
    while let Some((popped_pkt, popped_timing)) = jb.pop_with_timing() {
        let seq = popped_pkt.header.sequence_number;
        popped_seqs.push(seq);
        let residency = popped_timing
            .jitter_pacing_duration_us()
            .unwrap_or_default();
        total_residency_us += residency as u64;
    }

    // Verify ordering
    for i in 1..popped_seqs.len() {
        assert!(
            popped_seqs[i] > popped_seqs[i - 1],
            "JitterBuffer released out-of-order sequence: seq[{}]={} <= seq[{}]={}",
            i,
            popped_seqs[i],
            i - 1,
            popped_seqs[i - 1]
        );
    }

    println!(
        "Reordering test finished: pushed={}, popped={}, late_dropped={}, queue_full_dropped={}, avg_residency={:.1} µs",
        num_packets,
        popped_seqs.len(),
        jb.late_frames_dropped(),
        jb.queue_full_dropped(),
        total_residency_us as f64 / popped_seqs.len() as f64
    );
    assert_eq!(
        popped_seqs.len(),
        num_packets,
        "All packets should be reordered and released without loss under window={}",
        max_jitter_window
    );
    println!(
        ">>> [PASS] JitterBuffer successfully reordered out-of-order stream without loss or corruption"
    );
}

fn run_jitter_buffer_packet_loss_and_late_arrival_stress() {
    println!("\n=== [2.2] JitterBuffer Packet Loss & Late Packet Ingress Stress ===");

    // Case A: Delay within buffer depth (delay = 10 <= 15). All packets recovered in order!
    {
        let mut jb = JitterBuffer::new(0);
        jb.push_with_timing(
            make_packet(0, quanta_now_us()),
            FrameTimingCheckpoints::new(quanta_now_us()),
        );
        assert_eq!(jb.pop().map(|p| p.header.sequence_number), Some(0));

        let mut delayed_packets = Vec::new();
        let mut delivered = Vec::new();

        for seq in 1..=200u16 {
            let is_delayed = seq % 12 == 3;
            let pkt = make_packet(seq, quanta_now_us());
            let timing = FrameTimingCheckpoints::new(quanta_now_us());

            if is_delayed {
                delayed_packets.push((pkt, timing));
            } else {
                jb.push_with_timing(pkt, timing);
                while let Some((p, _)) = jb.pop_with_timing() {
                    delivered.push(p.header.sequence_number);
                }
            }

            // Inject delayed packet after 8 slots (<= 15)
            if seq % 8 == 0 && !delayed_packets.is_empty() {
                let (late_pkt, late_timing) = delayed_packets.remove(0);
                jb.push_with_timing(late_pkt, late_timing);
                while let Some((p, _)) = jb.pop_with_timing() {
                    delivered.push(p.header.sequence_number);
                }
            }
        }
        while let Some((p, _)) = jb.pop_with_timing() {
            delivered.push(p.header.sequence_number);
        }
        println!(
            "Case A (Delay <= 15): delivered={}, late_dropped={}, queue_full_dropped={}",
            delivered.len(),
            jb.late_frames_dropped(),
            jb.queue_full_dropped()
        );
        assert_eq!(delivered.len(), 200);
        assert_eq!(jb.late_frames_dropped(), 0);
        assert_eq!(jb.queue_full_dropped(), 0);
        println!(
            ">>> [PASS] JitterBuffer successfully held and recovered all packets when delay <= 15"
        );
    }

    // Case B: Delay exceeds buffer depth (delay = 30 > 15). Buffer skips gap (queue_full) and drops late packet when it arrives!
    {
        let mut jb = JitterBuffer::new(0);
        jb.push_with_timing(
            make_packet(0, quanta_now_us()),
            FrameTimingCheckpoints::new(quanta_now_us()),
        );
        assert_eq!(jb.pop().map(|p| p.header.sequence_number), Some(0));

        let mut delayed_packets = Vec::new();
        let mut delivered = Vec::new();

        for seq in 1..=300u16 {
            let is_delayed = seq % 40 == 5;
            let pkt = make_packet(seq, quanta_now_us());
            let timing = FrameTimingCheckpoints::new(quanta_now_us());

            if is_delayed {
                delayed_packets.push((pkt, timing));
            } else {
                jb.push_with_timing(pkt, timing);
                while let Some((p, _)) = jb.pop_with_timing() {
                    delivered.push(p.header.sequence_number);
                }
            }

            // Inject delayed packet after 30 slots (> 15 buffer capacity)
            if seq % 30 == 0 && !delayed_packets.is_empty() {
                let (late_pkt, late_timing) = delayed_packets.remove(0);
                jb.push_with_timing(late_pkt, late_timing);
                while let Some((p, _)) = jb.pop_with_timing() {
                    delivered.push(p.header.sequence_number);
                }
            }
        }
        for (late_pkt, late_timing) in delayed_packets {
            jb.push_with_timing(late_pkt, late_timing);
            while let Some((p, _)) = jb.pop_with_timing() {
                delivered.push(p.header.sequence_number);
            }
        }
        while let Some((p, _)) = jb.pop_with_timing() {
            delivered.push(p.header.sequence_number);
        }

        println!(
            "Case B (Delay > 15): delivered={}, late_dropped={}, queue_full_dropped={}",
            delivered.len(),
            jb.late_frames_dropped(),
            jb.queue_full_dropped()
        );
        assert!(
            jb.late_frames_dropped() > 0,
            "Stale packets arriving after gap skip must increment late_frames_dropped"
        );
        assert!(
            jb.queue_full_dropped() > 0,
            "Skipped gaps exceeding buffer depth must increment queue_full_dropped"
        );
        println!(
            ">>> [PASS] JitterBuffer correctly skipped overflow gaps and categorized late dropped frames"
        );
    }
}

fn run_jitter_buffer_massive_queue_squeeze_stress() {
    println!("\n=== [2.3] JitterBuffer Massive Queue Squeeze & Buffer Explosion Stress ===");

    // Subcase A: Burst with gap within discontinuity threshold (500 packets, head 0..9 missing)
    {
        let mut jb = JitterBuffer::new(0);
        jb.push(make_packet(0, quanta_now_us()));
        assert_eq!(jb.pop().map(|p| p.header.sequence_number), Some(0));

        println!("Subcase A: Injecting 500-packet burst with missing gap [1..19 missing]...");
        for seq in 20..520u16 {
            let pkt = make_packet(seq, quanta_now_us());
            let timing = FrameTimingCheckpoints::new(quanta_now_us());
            jb.push_with_timing(pkt, timing);
        }

        assert_eq!(jb.len(), 500, "Buffer should hold 500 packets before pop");

        let mut popped_count = 0;
        while let Some((p, timing)) = jb.pop_with_timing() {
            popped_count += 1;
            assert!(
                timing.jitter_exit_ts_us >= timing.jitter_enter_ts_us,
                "Residency calculation must be non-negative"
            );
            std::hint::black_box(p);
        }

        println!(
            "Subcase A Drained: popped={}, queue_full_dropped={}, late_dropped={}",
            popped_count,
            jb.queue_full_dropped(),
            jb.late_frames_dropped()
        );
        assert_eq!(popped_count, 500);
        assert!(
            jb.queue_full_dropped() > 0,
            "Gap skip must trigger queue_full_dropped"
        );
        assert_eq!(jb.len(), 0, "Buffer must be empty after complete drain");
    }

    // Subcase B: Discontinuity threshold stress (>1000 packets jump)
    {
        let mut jb = JitterBuffer::new(0);
        jb.push(make_packet(0, quanta_now_us()));
        assert_eq!(jb.pop().map(|p| p.header.sequence_number), Some(0));

        // Inject sudden jump to seq 2500 (> 1000 diff)
        println!("Subcase B: Injecting sudden sequence jump from 0 to 2500...");
        jb.push(make_packet(2500, quanta_now_us()));
        let popped = jb.pop().map(|p| p.header.sequence_number);
        assert_eq!(
            popped,
            Some(2500),
            "Discontinuity must reset expected_seq to 2500"
        );
    }

    println!(">>> [PASS] Massive queue squeeze and discontinuity stress handled gracefully");
}

fn run_jitter_buffer_u16_wraparound_stress() {
    println!("\n=== [2.4] JitterBuffer Sequence Wraparound Stress (u16::MAX -> 0) ===");
    let start_seq = u16::MAX - 20;
    let mut jb = JitterBuffer::new(start_seq);

    let mut pushed_seqs = Vec::new();
    let mut cur_seq = start_seq;
    for _ in 0..100 {
        pushed_seqs.push(cur_seq);
        cur_seq = cur_seq.wrapping_add(1);
    }

    // Keep initial start_seq as stream head, shuffle subsequent packets across boundary
    let mut shuffled = pushed_seqs.clone();
    let mut rng = StdRng::seed_from_u64(999);
    for chunk in shuffled[1..].chunks_mut(4) {
        chunk.shuffle(&mut rng);
    }

    for seq in shuffled {
        let pkt = make_packet(seq, quanta_now_us());
        let timing = FrameTimingCheckpoints::new(quanta_now_us());
        jb.push_with_timing(pkt, timing);
    }

    let mut popped = Vec::new();
    while let Some((p, _)) = jb.pop_with_timing() {
        popped.push(p.header.sequence_number);
    }

    assert_eq!(
        popped, pushed_seqs,
        "Wraparound sequences must be popped in exact order"
    );
    println!(
        "Popped {} packets across u16 boundary cleanly: first={}, last={}",
        popped.len(),
        popped[0],
        popped.last().unwrap()
    );
    println!(">>> [PASS] JitterBuffer u16 sequence number wraparound verified");
}

fn run_concurrent_jitter_buffer_pipeline_stress(num_producers: usize, packets_per_producer: usize) {
    println!(
        "\n=== [2.5] High-Concurrency Multi-Producer JitterBuffer Stress ({} Producers, {} pkts each) ===",
        num_producers, packets_per_producer
    );

    let jb = Arc::new(Mutex::new(JitterBuffer::new(0)));
    let start_barrier = Arc::new(AtomicBool::new(false));
    let total_produced = Arc::new(AtomicU64::new(0));

    let mut handles = Vec::new();
    for prod_id in 0..num_producers {
        let jb_clone = Arc::clone(&jb);
        let barrier = Arc::clone(&start_barrier);
        let prod_count = Arc::clone(&total_produced);

        handles.push(thread::spawn(move || {
            while !barrier.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            for i in 0..packets_per_producer {
                let seq = (prod_id * packets_per_producer + i) as u16;
                let pkt = make_packet(seq, quanta_now_us());
                let timing = FrameTimingCheckpoints::new(quanta_now_us());
                {
                    let mut lock = jb_clone.lock().unwrap();
                    lock.push_with_timing(pkt, timing);
                }
                prod_count.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    let start_time = Instant::now();
    start_barrier.store(true, Ordering::Release);

    // Consumer thread
    let jb_consumer = Arc::clone(&jb);
    let consumer_handle = thread::spawn(move || {
        let mut consumed = 0usize;
        let mut wait_count = 0;
        loop {
            let item = {
                let mut lock = jb_consumer.lock().unwrap();
                lock.pop_with_timing()
            };
            if let Some((_, timing)) = item {
                consumed += 1;
                assert!(timing.jitter_exit_ts_us >= timing.jitter_enter_ts_us);
                wait_count = 0;
            } else {
                wait_count += 1;
                if wait_count > 5_000 {
                    // Check if producers finished
                    let lock = jb_consumer.lock().unwrap();
                    if lock.is_empty() {
                        break;
                    }
                }
                thread::yield_now();
            }
        }
        consumed
    });

    for h in handles {
        h.join().unwrap();
    }
    let consumed_total = consumer_handle.join().unwrap();
    let elapsed = start_time.elapsed();

    println!(
        "Concurrent Pipeline finished: produced={}, consumed={}, elapsed={:?}, rate={:.2} pkts/sec",
        total_produced.load(Ordering::Relaxed),
        consumed_total,
        elapsed,
        (consumed_total as f64) / elapsed.as_secs_f64()
    );
    println!(
        ">>> [PASS] High-concurrency JitterBuffer pipeline stress test completed successfully"
    );
}

fn run_clock_synchronizer_stress() {
    println!("\n=== [2.6] ClockSynchronizer EWMA & Outlier Stress Test ===");
    let mut syncer = ClockSynchronizer::new();

    // Initial warm up with ~10ms RTT and 500µs offset
    for i in 0..100 {
        let t1 = 1_000_000_000 + i * 50_000;
        let host_delay = 500;
        let t2 = t1 + 5_000 + 500; // t2 = t1 + one_way + offset
        let t3 = t2 + host_delay;
        let t4 = t1 + 10_000 + host_delay; // total rtt = 10_000 + host_delay

        syncer.update_pong(t1, t2, t3, t4);
    }

    println!(
        "ClockSynchronizer after 100 normal pings: RTT={}µs, Offset={}µs",
        syncer.rtt_us(),
        syncer.clock_offset_us()
    );
    assert!(
        (syncer.rtt_us() as i32 - 10_000).abs() < 500,
        "RTT should converge to ~10ms"
    );
    assert!(
        (syncer.clock_offset_us() - 500).abs() < 100,
        "Offset should converge to ~500µs"
    );

    // Inject massive latency spikes (e.g. 500ms RTT)
    for _ in 0..10 {
        let t1 = quanta_now_us();
        let t2 = t1 + 250_000;
        let t3 = t2 + 1_000;
        let t4 = t1 + 500_000;
        syncer.update_pong(t1, t2, t3, t4);
    }

    println!(
        "ClockSynchronizer after 10 huge spikes (500ms): RTT={}µs, Offset={}µs",
        syncer.rtt_us(),
        syncer.clock_offset_us()
    );
    assert!(
        syncer.rtt_us() < 50_000,
        "Spikes should be filtered out, RTT must remain bounded"
    );
    println!(">>> [PASS] ClockSynchronizer EWMA filtering and outlier rejection verified");
}

fn main() {
    println!("===============================================================");
    println!("  M1 TIMING OVERHEAD & PIPELINE STRESS TEST SUITE");
    println!("===============================================================");

    // Part 1: HighPrecisionClock & Timing Probes
    run_single_probe_benchmark(5_000_000);
    run_multithreaded_probe_contention_benchmark(8, 1_000_000);
    run_multithreaded_probe_contention_benchmark(16, 500_000);
    run_full_12_probes_frame_lifecycle_benchmark(500_000);

    // Part 2: JitterBuffer Stress, Reordering, Loss & Burst Squeeze
    run_jitter_buffer_reordering_stress(5_000, 10);
    run_jitter_buffer_packet_loss_and_late_arrival_stress();
    run_jitter_buffer_massive_queue_squeeze_stress();
    run_jitter_buffer_u16_wraparound_stress();
    run_concurrent_jitter_buffer_pipeline_stress(4, 2_500);
    run_clock_synchronizer_stress();

    println!("\n===============================================================");
    println!("  ALL EMPIRICAL STRESS TESTS & BENCHMARKS COMPLETED WITH PASS");
    println!("===============================================================");
}
