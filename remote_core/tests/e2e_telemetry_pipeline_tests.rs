use protocol::{FrameTimingCheckpoints, PipelineTelemetryReport, StageId, StageLatencyStats};
use remote_core::telemetry::{
    DropReason, PipelineTelemetryEngine, RollingBitrateCalculator, RollingFpsCalculator,
    RollingQuantileAggregator,
};
use remote_core::timing::{
    ClientFrameTracker, ClockSynchronizer, HighPrecisionClock, HostFrameTracker,
};
use remote_core::trace::{
    BottleneckAnalyzer, BottleneckSeverity, ChromeTraceExporter, TraceSpikeTrigger,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

// =============================================================================
// TIER 1: Feature Functionality Verification (F10 ~ F18)
// =============================================================================

#[test]
fn test_tier1_rolling_quantile_statistical_accuracy() {
    let mut agg = RollingQuantileAggregator::new(1000);
    // Insert Gaussian-like distribution centered around 15,000 µs (15ms)
    for i in 1..=1000 {
        let val_us = (10_000 + (i * 10)) as u32; // 10,010 to 20,000 µs
        agg.record(val_us);
    }

    let stats = agg.snapshot();
    assert_eq!(stats.sample_count, 1000);
    assert_eq!(stats.min_us, 10_010);
    assert_eq!(stats.max_us, 20_000);
    assert_eq!(stats.p50_us, 15_000);
    assert_eq!(stats.p95_us, 19_500);
    assert_eq!(stats.p99_us, 19_900);
    assert_eq!(stats.avg_us, 15_005);
    assert!(stats.stddev_us > 2800 && stats.stddev_us < 3000);
}

#[test]
fn test_tier1_stream_health_vitals_tracking() {
    let mut engine = PipelineTelemetryEngine::new(42, 200);

    // Record drops
    engine.record_drop(DropReason::Late);
    engine.record_drop(DropReason::Late);
    engine.record_drop(DropReason::QueueFull);
    engine.record_drop(DropReason::Corrupt);
    engine.record_drop(DropReason::PacketLost(15));
    engine.set_jitter_buffer_depth(5);

    let report = engine.generate_report();
    assert_eq!(report.session_id, 42);
    assert_eq!(report.late_frames_dropped, 2);
    assert_eq!(report.queue_full_dropped, 1);
    assert_eq!(report.corrupt_frames_dropped, 1);
    assert_eq!(report.packets_lost, 15);
    assert_eq!(report.jitter_buffer_depth, 5);
}

#[test]
fn test_tier1_chrome_trace_json_schema_compliance() {
    let mut cp = FrameTimingCheckpoints::new(1_700_000_000_000_000);
    cp.encode_queue_ts_us = 1_000;
    cp.encode_done_ts_us = 4_000;
    cp.packetize_ts_us = 4_500;
    cp.send_ts_us = 5_000;
    cp.recv_ts_us = 12_000;
    cp.jitter_enter_ts_us = 12_500;
    cp.jitter_exit_ts_us = 14_500;
    cp.decode_enter_ts_us = 15_000;
    cp.decode_done_ts_us = 17_500;
    cp.render_submit_ts_us = 18_000;
    cp.render_done_ts_us = 20_000;

    let trace_json = ChromeTraceExporter::export_frames_trace(&[(1, cp), (2, cp)], 99);
    let parsed: serde_json::Value = serde_json::from_str(&trace_json).expect("valid JSON");

    assert!(parsed.is_object());
    let events = parsed["traceEvents"].as_array().expect("traceEvents array");
    assert_eq!(events.len(), 16);

    for event in events {
        assert!(event["name"].is_string());
        assert!(event["cat"].is_string());
        assert_eq!(event["ph"].as_str(), Some("X"));
        assert!(event["ts"].is_number());
        assert!(event["dur"].is_number());
        assert_eq!(event["pid"].as_u64(), Some(99));
        assert_eq!(event["tid"].as_u64(), Some(1));
    }
}

#[test]
fn test_tier1_bottleneck_analyzer_rules() {
    // 1. Test Hardware Encode bottleneck rule
    let mut report_enc = PipelineTelemetryReport {
        session_id: 1,
        timestamp_ms: 1000,
        fps: 60.0,
        bitrate_kbps: 8000,
        stage_stats: [StageLatencyStats::default(); StageId::STAGE_COUNT],
        e2e_stats: StageLatencyStats {
            avg_us: 35_000,
            p50_us: 32_000,
            p99_us: 60_000,
            sample_count: 100,
            ..Default::default()
        },
        jitter_buffer_depth: 2,
        packets_lost: 0,
        late_frames_dropped: 0,
        queue_full_dropped: 0,
        corrupt_frames_dropped: 0,
    };
    report_enc.stage_stats[StageId::HardwareEncode as usize] = StageLatencyStats {
        avg_us: 25_000,
        p50_us: 22_000,
        p99_us: 50_000,
        sample_count: 100,
        ..Default::default()
    };

    let diag_enc = BottleneckAnalyzer::analyze(&report_enc);
    assert_eq!(diag_enc.primary_stage, StageId::HardwareEncode);
    assert!(diag_enc.root_cause.contains("硬件编码阶段占比"));
    assert!(diag_enc.recommendation.contains("编码 Preset"));

    // 2. Test Render Presentation bottleneck rule
    let mut report_ren = PipelineTelemetryReport {
        session_id: 2,
        timestamp_ms: 1000,
        fps: 60.0,
        bitrate_kbps: 8000,
        stage_stats: [StageLatencyStats::default(); StageId::STAGE_COUNT],
        e2e_stats: StageLatencyStats {
            avg_us: 30_000,
            p50_us: 28_000,
            p99_us: 55_000,
            sample_count: 100,
            ..Default::default()
        },
        jitter_buffer_depth: 2,
        packets_lost: 0,
        late_frames_dropped: 0,
        queue_full_dropped: 0,
        corrupt_frames_dropped: 0,
    };
    report_ren.stage_stats[StageId::RenderPresentation as usize] = StageLatencyStats {
        avg_us: 20_000,
        p50_us: 18_000,
        p99_us: 40_000,
        sample_count: 100,
        ..Default::default()
    };

    let diag_ren = BottleneckAnalyzer::analyze(&report_ren);
    assert_eq!(diag_ren.primary_stage, StageId::RenderPresentation);
    assert!(diag_ren.root_cause.contains("渲染呈现阶段占比"));
    assert!(diag_ren.recommendation.contains("VSync"));
}

// =============================================================================
// TIER 2: Boundary & Corner Case Tests
// =============================================================================

#[test]
fn test_tier2_extreme_window_and_overflow_boundaries() {
    let mut agg = RollingQuantileAggregator::new(5);
    // Window capacity 5
    for i in 1..=5 {
        agg.record(i * 1000);
    }
    assert_eq!(agg.len(), 5);
    assert_eq!(agg.snapshot().p50_us, 3000);

    // Overwrite with large numbers
    for i in 6..=10 {
        agg.record(i * 1000);
    }
    assert_eq!(agg.len(), 5);
    assert_eq!(agg.snapshot().min_us, 6000);
    assert_eq!(agg.snapshot().max_us, 10000);
    assert_eq!(agg.snapshot().p50_us, 8000);

    // Test with u32::MAX saturation
    agg.record(u32::MAX);
    assert_eq!(agg.snapshot().max_us, u32::MAX);
}

#[test]
fn test_tier2_fps_and_bitrate_calculators_idle_and_burst() {
    let mut fps = RollingFpsCalculator::new(Duration::from_secs(1));
    assert_eq!(fps.current_fps(), 0.0);

    let now = Instant::now();
    fps.record_tick(now);
    assert_eq!(fps.current_fps(), 1.0);

    let mut bitrate = RollingBitrateCalculator::new(Duration::from_secs(1));
    assert_eq!(bitrate.current_bitrate_kbps(), 0);

    bitrate.record_bytes(1000, now);
    assert_eq!(bitrate.current_bitrate_kbps(), 0); // Need at least 2 ticks
    bitrate.record_bytes(1000, now + Duration::from_millis(500));
    assert!(bitrate.current_bitrate_kbps() > 0);
}

#[test]
fn test_tier2_spike_trigger_threshold_edge_cases() {
    let mut trigger = TraceSpikeTrigger::new(10, 50_000, 15);

    // Exactly at threshold -> No spike
    let mut at_thresh = FrameTimingCheckpoints::new(100_000);
    at_thresh.render_done_ts_us = 50_000;
    assert!(trigger.record_frame(1, at_thresh).is_none());

    // 1 us above threshold -> Spike!
    let mut above_thresh = FrameTimingCheckpoints::new(100_000);
    above_thresh.render_done_ts_us = 50_001;
    let res = trigger.record_frame(2, above_thresh);
    assert!(res.is_some());
    assert_eq!(res.unwrap().total_e2e_us, 50_001);
}

// =============================================================================
// TIER 3: Cross-Feature Pairwise & Concurrency Tests
// =============================================================================

#[test]
fn test_tier3_clock_sync_tracker_and_telemetry_pairwise() {
    let mut syncer = ClockSynchronizer::new();
    syncer.update_pong(1_000_000, 1_005_000, 1_006_000, 1_012_000);
    assert!(syncer.is_initialized());

    let clock = HighPrecisionClock::new();
    let mut host_tracker = HostFrameTracker::start(clock.clone());
    std::thread::sleep(Duration::from_micros(200));
    host_tracker.mark_encode_queue();
    std::thread::sleep(Duration::from_micros(200));
    host_tracker.mark_encode_done();
    std::thread::sleep(Duration::from_micros(100));
    host_tracker.mark_packetize();
    host_tracker.mark_send();
    let host_cp = host_tracker.finish();

    let mut client_tracker =
        ClientFrameTracker::start(host_cp, clock.clone(), syncer.clock_offset_us());
    std::thread::sleep(Duration::from_micros(100));
    client_tracker.mark_jitter_enter();
    std::thread::sleep(Duration::from_micros(100));
    client_tracker.mark_jitter_exit();
    std::thread::sleep(Duration::from_micros(100));
    client_tracker.mark_decode_enter();
    std::thread::sleep(Duration::from_micros(100));
    client_tracker.mark_decode_done();
    std::thread::sleep(Duration::from_micros(100));
    client_tracker.mark_render_submit();
    std::thread::sleep(Duration::from_micros(100));
    client_tracker.mark_render_done();
    let complete_cp = client_tracker.finish();

    let mut engine = PipelineTelemetryEngine::new(1, 100);
    engine.record_frame(&complete_cp, 12_500);

    let report = engine.generate_report();
    assert_eq!(report.session_id, 1);
    assert!(report.stage_stats[StageId::Capture as usize].sample_count > 0);
    assert!(report.stage_stats[StageId::HardwareEncode as usize].sample_count > 0);
    assert!(report.stage_stats[StageId::HardwareDecode as usize].sample_count > 0);
    assert!(report.stage_stats[StageId::RenderPresentation as usize].sample_count > 0);
}

#[test]
fn test_tier3_multithreaded_concurrent_telemetry_updates() {
    use std::sync::Mutex;
    let engine = Arc::new(Mutex::new(PipelineTelemetryEngine::new(99, 500)));
    let mut handles = Vec::new();

    for thread_id in 0..8 {
        let engine_clone = engine.clone();
        handles.push(std::thread::spawn(move || {
            for frame in 0..100 {
                let mut cp = FrameTimingCheckpoints::new(1_000_000 + frame * 16_666);
                cp.encode_queue_ts_us = 1000 + thread_id * 10;
                cp.encode_done_ts_us = 3500 + thread_id * 10;
                cp.send_ts_us = 4000;
                cp.recv_ts_us = 10000;
                cp.render_done_ts_us = 16000;

                let mut eng = engine_clone.lock().unwrap();
                eng.record_frame(&cp, 5000);
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let eng = engine.lock().unwrap();
    let report = eng.generate_report();
    assert_eq!(report.e2e_stats.sample_count, 500); // Capped at window size 500
    assert_eq!(report.e2e_stats.min_us, 16_000);
}

// =============================================================================
// TIER 4: Real-World Scenarios & Performance Overhead Benchmark
// =============================================================================

#[test]
fn test_tier4_scenario_4k60_streaming_lifecycle() {
    let mut engine = PipelineTelemetryEngine::new(4000, 300);
    let mut trigger = TraceSpikeTrigger::new(4000, 40_000, 60);

    // Simulate 300 frames of 4K60 (16.6ms per frame interval)
    for frame_id in 1..=300 {
        let mut cp = FrameTimingCheckpoints::new(1_700_000_000_000 + (frame_id * 16_666));
        cp.encode_queue_ts_us = 800;
        cp.encode_done_ts_us = 3_200;
        cp.packetize_ts_us = 3_600;
        cp.send_ts_us = 4_000;
        cp.recv_ts_us = 9_500; // 5.5ms network
        cp.jitter_enter_ts_us = 10_000;
        cp.jitter_exit_ts_us = 12_000; // 2ms jitter buffer
        cp.decode_enter_ts_us = 12_200;
        cp.decode_done_ts_us = 14_800; // 2.6ms decode
        cp.render_submit_ts_us = 15_000;
        cp.render_done_ts_us = 16_500; // 1.5ms render (total 16.5ms E2E)

        engine.record_frame(&cp, 35_000); // 35KB per frame
        let spike = trigger.record_frame(frame_id, cp);
        assert!(spike.is_none());
    }

    let report = engine.generate_report();
    assert_eq!(report.session_id, 4000);
    assert_eq!(report.e2e_stats.sample_count, 300);
    assert_eq!(report.e2e_stats.p50_us, 16_500);

    let diag = BottleneckAnalyzer::analyze(&report);
    assert!(diag.severity <= BottleneckSeverity::Low);

    let summary_table = engine.format_ascii_summary_table();
    assert!(summary_table.contains("PIPELINE TELEMETRY REPORT [Session: 4000    ]"));
    assert!(summary_table.contains("HardwareEncode"));
    assert!(summary_table.contains("NetworkTransit"));
}

#[test]
fn test_tier4_scenario_latency_spike_and_automated_diagnosis() {
    let mut engine = PipelineTelemetryEngine::new(5000, 100);
    let mut trigger = TraceSpikeTrigger::new(5000, 35_000, 30);

    // Normal frames
    for f in 1..=20 {
        let mut cp = FrameTimingCheckpoints::new(100_000 + f * 16_000);
        cp.send_ts_us = 3_000;
        cp.render_done_ts_us = 15_000;
        engine.record_frame(&cp, 10_000);
        assert!(trigger.record_frame(f, cp).is_none());
    }

    // Injected network glitch spike on frame 21 (E2E = 75ms)
    let mut spike_cp = FrameTimingCheckpoints::new(100_000 + 21 * 16_000);
    spike_cp.encode_queue_ts_us = 500;
    spike_cp.encode_done_ts_us = 2_500;
    spike_cp.send_ts_us = 3_000;
    spike_cp.recv_ts_us = 65_000; // 62ms network congestion
    spike_cp.render_done_ts_us = 75_000;

    engine.record_frame(&spike_cp, 10_000);
    let spike_opt = trigger.record_frame(21, spike_cp);
    assert!(spike_opt.is_some());

    let spike_report = spike_opt.unwrap();
    assert_eq!(spike_report.spike_frame_id, 21);
    assert_eq!(spike_report.culprit_stage, StageId::NetworkTransit);
    assert!(spike_report.total_e2e_us >= 75_000);
    assert!(spike_report.trace_json.contains("S4_NetworkTransit (F#21)"));
}

#[test]
fn test_tier4_benchmark_instrumentation_overhead() {
    // Verify that single probe overhead is <100ns and total per-frame profiling is <50µs
    let clock = HighPrecisionClock::new();

    // 1. Single probe measurement
    let probe_iterations = 10_000;
    let start_probe = Instant::now();
    for _ in 0..probe_iterations {
        let _ = clock.now();
    }
    let elapsed_probe = start_probe.elapsed();
    let per_probe_ns = elapsed_probe.as_nanos() as f64 / probe_iterations as f64;
    println!("Measured per-probe overhead: {:.2} ns", per_probe_ns);
    assert!(
        per_probe_ns < 100.0,
        "Per probe overhead must be <100ns (was {:.2}ns)",
        per_probe_ns
    );

    // 2. Full frame lifecycle tracking & aggregation overhead
    let frame_iterations = 5_000;
    let mut engine = PipelineTelemetryEngine::new(1, 300);
    let start_frame = Instant::now();

    for i in 0..frame_iterations {
        let mut host_tracker = HostFrameTracker::start(clock.clone());
        host_tracker.mark_encode_queue();
        host_tracker.mark_encode_done();
        host_tracker.mark_packetize();
        host_tracker.mark_send();
        let host_cp = host_tracker.finish();

        let mut client_tracker = ClientFrameTracker::start(host_cp, clock.clone(), 0);
        client_tracker.mark_jitter_enter();
        client_tracker.mark_jitter_exit();
        client_tracker.mark_decode_enter();
        client_tracker.mark_decode_done();
        client_tracker.mark_render_submit();
        client_tracker.mark_render_done();
        let client_cp = client_tracker.finish();

        engine.record_frame(&client_cp, 20_000);
        if i % 60 == 0 {
            let _ = engine.generate_report();
        }
    }

    let elapsed_frame = start_frame.elapsed();
    let per_frame_us = elapsed_frame.as_micros() as f64 / frame_iterations as f64;
    println!(
        "Measured full-frame profiling & telemetry overhead: {:.2} µs",
        per_frame_us
    );
    assert!(
        per_frame_us < 50.0,
        "Per frame overhead must be <50µs (was {:.2}µs)",
        per_frame_us
    );
}
