use protocol::timing::{
    FRAME_TIMING_CHECKPOINTS_WIRE_LEN, FrameTimingCheckpoints, HOST_TIMING_WIRE_LEN, StageId,
    StageLatencyStats, TimingCodecError, TraceSpan,
};
use protocol::{
    COMPACT_REALTIME_HEADER_LEN, COMPACT_REALTIME_TIMING_FLAG, CompactRealtimeError, DataEnvelope,
};

#[test]
fn test_wire_lengths_constant_invariants() {
    assert_eq!(FRAME_TIMING_CHECKPOINTS_WIRE_LEN, 52);
    assert_eq!(HOST_TIMING_WIRE_LEN, 24);
    assert_eq!(COMPACT_REALTIME_HEADER_LEN, 26);
}

#[test]
fn test_stage_id_exhaustive_wire_id_mapping() {
    // 1. All valid wire IDs 0..=8 must map to the 9 distinct stages.
    let mut mapped_stages = Vec::new();
    for id in 0..=8 {
        let stage = StageId::from_wire_id(id).expect("valid wire ID");
        assert_eq!(stage.wire_id(), id);
        assert!(!stage.name().is_empty());
        assert!(!stage.display_name().is_empty());
        mapped_stages.push(stage);
    }
    assert_eq!(mapped_stages.len(), 9);
    assert_eq!(StageId::STAGE_COUNT, 9);
    assert_eq!(StageId::ALL.len(), 9);

    // 2. Stages must be mutually exclusive in domain classification.
    for stage in StageId::ALL {
        let is_h = stage.is_host();
        let is_n = stage.is_network();
        let is_c = stage.is_client();
        let count = (is_h as u8) + (is_n as u8) + (is_c as u8);
        assert_eq!(
            count, 1,
            "Stage {:?} must belong to exactly one domain",
            stage
        );
    }

    // 3. All invalid wire IDs (9..=255) must return InvalidStageId error without panicking.
    for id in 9..=255 {
        match StageId::from_wire_id(id) {
            Err(TimingCodecError::InvalidStageId(bad_id)) => assert_eq!(bad_id, id),
            Ok(s) => panic!("Expected error for wire id {}, got {:?}", id, s),
            Err(e) => panic!("Unexpected error type: {:?}", e),
        }
    }
}

#[test]
fn test_wire_bytes_exact_field_layout_and_endianness() {
    let cp = FrameTimingCheckpoints {
        capture_ts_us: 0x0102030405060708,
        encode_queue_ts_us: 0x11121314,
        encode_done_ts_us: 0x21222324,
        packetize_ts_us: 0x31323334,
        send_ts_us: 0x41424344,
        recv_ts_us: 0x51525354,
        jitter_enter_ts_us: 0x61626364,
        jitter_exit_ts_us: 0x71727374,
        decode_enter_ts_us: 0x81828384,
        decode_done_ts_us: 0x91929394,
        render_submit_ts_us: 0xA1A2A3A4,
        render_done_ts_us: 0xB1B2B3B4,
    };

    let bytes = cp.to_wire_bytes();
    assert_eq!(bytes.len(), 52);

    // Verify big-endian byte layout
    assert_eq!(
        &bytes[0..8],
        &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
    );
    assert_eq!(&bytes[8..12], &[0x11, 0x12, 0x13, 0x14]);
    assert_eq!(&bytes[12..16], &[0x21, 0x22, 0x23, 0x24]);
    assert_eq!(&bytes[16..20], &[0x31, 0x32, 0x33, 0x34]);
    assert_eq!(&bytes[20..24], &[0x41, 0x42, 0x43, 0x44]);
    assert_eq!(&bytes[24..28], &[0x51, 0x52, 0x53, 0x54]);
    assert_eq!(&bytes[28..32], &[0x61, 0x62, 0x63, 0x64]);
    assert_eq!(&bytes[32..36], &[0x71, 0x72, 0x73, 0x74]);
    assert_eq!(&bytes[36..40], &[0x81, 0x82, 0x83, 0x84]);
    assert_eq!(&bytes[40..44], &[0x91, 0x92, 0x93, 0x94]);
    assert_eq!(&bytes[44..48], &[0xA1, 0xA2, 0xA3, 0xA4]);
    assert_eq!(&bytes[48..52], &[0xB1, 0xB2, 0xB3, 0xB4]);

    // Full roundtrip
    let recovered = FrameTimingCheckpoints::from_wire_bytes(&bytes).expect("decode full wire");
    assert_eq!(cp, recovered);

    // Host wire bytes (24 bytes)
    let host_bytes = cp.to_host_wire_bytes();
    assert_eq!(host_bytes.len(), 24);
    assert_eq!(&host_bytes[0..8], &bytes[0..8]);
    assert_eq!(&host_bytes[8..12], &bytes[8..12]);
    assert_eq!(&host_bytes[12..16], &bytes[12..16]);
    assert_eq!(&host_bytes[16..20], &bytes[16..20]);
    assert_eq!(&host_bytes[20..24], &bytes[20..24]);

    // Host wire decode must initialize client fields to 0
    let host_rec =
        FrameTimingCheckpoints::from_host_wire_bytes(&host_bytes).expect("decode host wire");
    assert_eq!(host_rec.capture_ts_us, cp.capture_ts_us);
    assert_eq!(host_rec.encode_queue_ts_us, cp.encode_queue_ts_us);
    assert_eq!(host_rec.encode_done_ts_us, cp.encode_done_ts_us);
    assert_eq!(host_rec.packetize_ts_us, cp.packetize_ts_us);
    assert_eq!(host_rec.send_ts_us, cp.send_ts_us);
    assert_eq!(host_rec.recv_ts_us, 0);
    assert_eq!(host_rec.jitter_enter_ts_us, 0);
    assert_eq!(host_rec.jitter_exit_ts_us, 0);
    assert_eq!(host_rec.decode_enter_ts_us, 0);
    assert_eq!(host_rec.decode_done_ts_us, 0);
    assert_eq!(host_rec.render_submit_ts_us, 0);
    assert_eq!(host_rec.render_done_ts_us, 0);
}

#[test]
fn test_wire_decode_buffer_bounds_and_truncation_robustness() {
    let dummy_52 = [0x5Au8; 52];

    // 1. from_wire_bytes: every length 0..51 must return BufferTooShort and NEVER panic.
    for len in 0..52 {
        match FrameTimingCheckpoints::from_wire_bytes(&dummy_52[0..len]) {
            Err(TimingCodecError::BufferTooShort { expected, actual }) => {
                assert_eq!(expected, 52);
                assert_eq!(actual, len);
            }
            Ok(_) => panic!("Expected BufferTooShort error for len {}", len),
            Err(e) => panic!("Unexpected error for len {}: {:?}", len, e),
        }
    }

    // 2. from_wire_bytes: length >= 52 must succeed and ignore trailing bytes.
    let mut large_buffer = vec![0x7Fu8; 1024];
    large_buffer[0..8].copy_from_slice(&123456789u64.to_be_bytes());
    let res = FrameTimingCheckpoints::from_wire_bytes(&large_buffer);
    assert!(res.is_ok());
    assert_eq!(res.unwrap().capture_ts_us, 123456789);

    // 3. from_host_wire_bytes: every length 0..23 must return BufferTooShort and NEVER panic.
    let dummy_24 = [0xA5u8; 24];
    for len in 0..24 {
        match FrameTimingCheckpoints::from_host_wire_bytes(&dummy_24[0..len]) {
            Err(TimingCodecError::BufferTooShort { expected, actual }) => {
                assert_eq!(expected, 24);
                assert_eq!(actual, len);
            }
            Ok(_) => panic!("Expected BufferTooShort error for host len {}", len),
            Err(e) => panic!("Unexpected error for host len {}: {:?}", len, e),
        }
    }

    // 4. from_host_wire_bytes: length >= 24 must succeed.
    let mut large_host_buffer = vec![0x33u8; 512];
    large_host_buffer[0..8].copy_from_slice(&987654321u64.to_be_bytes());
    let host_res = FrameTimingCheckpoints::from_host_wire_bytes(&large_host_buffer);
    assert!(host_res.is_ok());
    assert_eq!(host_res.unwrap().capture_ts_us, 987654321);
}

#[test]
fn test_extreme_boundary_values() {
    let test_cases = vec![
        // All zeros
        FrameTimingCheckpoints {
            capture_ts_us: 0,
            encode_queue_ts_us: 0,
            encode_done_ts_us: 0,
            packetize_ts_us: 0,
            send_ts_us: 0,
            recv_ts_us: 0,
            jitter_enter_ts_us: 0,
            jitter_exit_ts_us: 0,
            decode_enter_ts_us: 0,
            decode_done_ts_us: 0,
            render_submit_ts_us: 0,
            render_done_ts_us: 0,
        },
        // All MAX
        FrameTimingCheckpoints {
            capture_ts_us: u64::MAX,
            encode_queue_ts_us: u32::MAX,
            encode_done_ts_us: u32::MAX,
            packetize_ts_us: u32::MAX,
            send_ts_us: u32::MAX,
            recv_ts_us: u32::MAX,
            jitter_enter_ts_us: u32::MAX,
            jitter_exit_ts_us: u32::MAX,
            decode_enter_ts_us: u32::MAX,
            decode_done_ts_us: u32::MAX,
            render_submit_ts_us: u32::MAX,
            render_done_ts_us: u32::MAX,
        },
        // Alternating bit patterns
        FrameTimingCheckpoints {
            capture_ts_us: 0xAAAAAAAAAAAAAAAA,
            encode_queue_ts_us: 0x55555555,
            encode_done_ts_us: 0xAAAAAAAA,
            packetize_ts_us: 0x55555555,
            send_ts_us: 0xAAAAAAAA,
            recv_ts_us: 0x55555555,
            jitter_enter_ts_us: 0xAAAAAAAA,
            jitter_exit_ts_us: 0x55555555,
            decode_enter_ts_us: 0xAAAAAAAA,
            decode_done_ts_us: 0x55555555,
            render_submit_ts_us: 0xAAAAAAAA,
            render_done_ts_us: 0x55555555,
        },
    ];

    for (idx, cp) in test_cases.into_iter().enumerate() {
        let wire = cp.to_wire_bytes();
        let decoded = FrameTimingCheckpoints::from_wire_bytes(&wire)
            .unwrap_or_else(|e| panic!("Failed test case {}: {:?}", idx, e));
        assert_eq!(cp, decoded, "Mismatch in test case {}", idx);
    }
}

#[test]
fn test_duration_calculations_with_clock_anomalies_and_out_of_order() {
    // 1. All zero timestamps -> all durations return None
    let zero_cp = FrameTimingCheckpoints::new(0);
    assert_eq!(zero_cp.capture_duration_us(), None);
    assert_eq!(zero_cp.encode_queue_duration_us(), None);
    assert_eq!(zero_cp.hardware_encode_duration_us(), None);
    assert_eq!(zero_cp.packetize_egress_duration_us(), None);
    assert_eq!(zero_cp.network_transit_duration_us(), None);
    assert_eq!(zero_cp.ingress_reassembly_duration_us(), None);
    assert_eq!(zero_cp.jitter_pacing_duration_us(), None);
    assert_eq!(zero_cp.hardware_decode_duration_us(), None);
    assert_eq!(zero_cp.render_presentation_duration_us(), None);
    assert_eq!(zero_cp.host_pipeline_duration_us(), None);
    assert_eq!(zero_cp.client_pipeline_duration_us(), None);
    assert_eq!(zero_cp.total_e2e_duration_us(), None);

    for stage in StageId::ALL {
        assert_eq!(zero_cp.stage_duration_us(stage), None);
    }

    // 2. Backward / Inverted Timestamps (Clock jumps backward / reordering)
    // All delta calculations must saturate to 0 via saturating_sub, NEVER panic or underflow.
    let mut inverted_cp = FrameTimingCheckpoints::new(100_000);
    inverted_cp.encode_queue_ts_us = 5_000;
    inverted_cp.encode_done_ts_us = 2_000; // < encode_queue
    inverted_cp.packetize_ts_us = 1_500;
    inverted_cp.send_ts_us = 1_000; // < encode_done
    inverted_cp.recv_ts_us = 500; // < send
    inverted_cp.jitter_enter_ts_us = 400; // < recv
    inverted_cp.jitter_exit_ts_us = 300; // < jitter_enter
    inverted_cp.decode_enter_ts_us = 200;
    inverted_cp.decode_done_ts_us = 100; // < decode_enter
    inverted_cp.render_submit_ts_us = 50;
    inverted_cp.render_done_ts_us = 10; // < render_submit & recv

    // saturating_sub should return Some(0)
    assert_eq!(inverted_cp.capture_duration_us(), Some(5_000));
    assert_eq!(inverted_cp.encode_queue_duration_us(), Some(0)); // 2000.saturating_sub(5000) == 0
    assert_eq!(inverted_cp.hardware_encode_duration_us(), Some(0));
    assert_eq!(inverted_cp.packetize_egress_duration_us(), Some(0)); // 1000.saturating_sub(2000) == 0
    assert_eq!(inverted_cp.network_transit_duration_us(), Some(0)); // 500.saturating_sub(1000) == 0
    assert_eq!(inverted_cp.ingress_reassembly_duration_us(), Some(0)); // 400.saturating_sub(500) == 0
    assert_eq!(inverted_cp.jitter_pacing_duration_us(), Some(0)); // 300.saturating_sub(400) == 0
    assert_eq!(inverted_cp.hardware_decode_duration_us(), Some(0)); // 100.saturating_sub(200) == 0
    assert_eq!(inverted_cp.render_presentation_duration_us(), Some(0)); // 10.saturating_sub(50) == 0

    assert_eq!(inverted_cp.host_pipeline_duration_us(), Some(1_000));
    assert_eq!(inverted_cp.client_pipeline_duration_us(), Some(0)); // 10.saturating_sub(500) == 0
    assert_eq!(inverted_cp.total_e2e_duration_us(), Some(10));

    // 3. Hardware encode fallback when encode_queue_ts_us == 0
    let mut fallback_cp = FrameTimingCheckpoints::new(100_000);
    fallback_cp.encode_done_ts_us = 3_500;
    assert_eq!(fallback_cp.encode_queue_duration_us(), None);
    assert_eq!(fallback_cp.hardware_encode_duration_us(), Some(3_500));
}

#[test]
fn test_trace_spans_invariants() {
    let mut cp = FrameTimingCheckpoints::new(1_000_000);
    // S1: 0 -> 1000 (valid)
    cp.encode_queue_ts_us = 1_000;
    // S2: 1000 -> 3000 (valid)
    cp.encode_done_ts_us = 3_000;
    // S3: 3000 -> 3000 (0 duration: should NOT produce span)
    cp.send_ts_us = 3_000;
    // S4: 3000 -> 2000 (inverted: should NOT produce span)
    cp.recv_ts_us = 2_000;
    // S5: 2000 -> 2500 (valid)
    cp.jitter_enter_ts_us = 2_500;
    // S6: 2500 -> 4000 (valid)
    cp.jitter_exit_ts_us = 4_000;
    // S7: 4000 -> 6000 (valid)
    cp.decode_enter_ts_us = 4_000;
    cp.decode_done_ts_us = 6_000;
    // S8: 6000 -> 7000 (valid)
    cp.render_submit_ts_us = 6_000;
    cp.render_done_ts_us = 7_000;

    let spans = cp.to_trace_spans(100, 1);
    // Spans produced should only be S1, S2, S5, S6, S7, S8 (total 6, S3 and S4 omitted)
    assert_eq!(spans.len(), 6);
    assert_eq!(spans[0].name, "S1_Capture (F#100)");
    assert_eq!(spans[0].dur_us, 1_000);
    assert_eq!(spans[1].name, "S2_HardwareEncode (F#100)");
    assert_eq!(spans[1].dur_us, 2_000);
    assert_eq!(spans[2].name, "S5_IngressReassembly (F#100)");
    assert_eq!(spans[2].dur_us, 500);
    assert_eq!(spans[3].name, "S6_JitterPacing (F#100)");
    assert_eq!(spans[3].dur_us, 1_500);
    assert_eq!(spans[4].name, "S7_HardwareDecode (F#100)");
    assert_eq!(spans[4].dur_us, 2_000);
    assert_eq!(spans[5].name, "S8_RenderPresentation (F#100)");
    assert_eq!(spans[5].dur_us, 1_000);

    for span in &spans {
        assert!(span.dur_us > 0);
        assert_eq!(span.ph, 'X');
    }
}

#[test]
fn test_compact_realtime_envelope_timing_integration() {
    let payload = b"NALU_TEST_FRAME_DATA_12345";
    let envelope = DataEnvelope::realtime_video(1, 100, 10_000, 10_050, payload.to_vec());

    let mut cp = FrameTimingCheckpoints::new(1_700_000_000_000_000);
    cp.encode_queue_ts_us = 1_000;
    cp.encode_done_ts_us = 3_000;
    cp.packetize_ts_us = 3_200;
    cp.send_ts_us = 3_500;

    // 1. Encode with timing
    let wire_bytes = envelope
        .encode_compact_realtime_with_timing(Some(cp))
        .expect("encode with timing");

    // Header (26) + HostTiming (24) + Payload (26) = 76 bytes
    assert_eq!(
        wire_bytes.len(),
        COMPACT_REALTIME_HEADER_LEN + HOST_TIMING_WIRE_LEN + payload.len()
    );

    // Verify flag 0x02 is present
    assert_ne!(wire_bytes[0] & COMPACT_REALTIME_TIMING_FLAG, 0);

    // 2. Decode with timing
    let (decoded_env, decoded_timing) =
        DataEnvelope::decode_compact_realtime_with_timing(&wire_bytes).expect("decode with timing");

    assert_eq!(decoded_env.payload, payload);
    assert_eq!(decoded_env.header.stream_id, 1);
    assert_eq!(decoded_env.header.sequence_number, 100);

    let timing = decoded_timing.expect("timing must be present");
    assert_eq!(timing.capture_ts_us, cp.capture_ts_us);
    assert_eq!(timing.encode_queue_ts_us, cp.encode_queue_ts_us);
    assert_eq!(timing.encode_done_ts_us, cp.encode_done_ts_us);
    assert_eq!(timing.packetize_ts_us, cp.packetize_ts_us);
    assert_eq!(timing.send_ts_us, cp.send_ts_us);
    assert_eq!(timing.recv_ts_us, 0);

    // 3. Encode without timing
    let wire_bytes_no_timing = envelope
        .encode_compact_realtime_with_timing(None)
        .expect("encode without timing");

    assert_eq!(
        wire_bytes_no_timing.len(),
        COMPACT_REALTIME_HEADER_LEN + payload.len()
    );
    assert_eq!(wire_bytes_no_timing[0] & COMPACT_REALTIME_TIMING_FLAG, 0);

    let (decoded_env_no_t, decoded_timing_none) =
        DataEnvelope::decode_compact_realtime_with_timing(&wire_bytes_no_timing)
            .expect("decode without timing");
    assert_eq!(decoded_env_no_t.payload, payload);
    assert_eq!(decoded_timing_none, None);

    // 4. Truncated packet with TIMING flag set but length < 50
    let truncated_timing_pkt = wire_bytes[..45].to_vec();
    match DataEnvelope::decode_compact_realtime_with_timing(&truncated_timing_pkt) {
        Err(CompactRealtimeError::InvalidLength {
            expected_at_least,
            actual,
        }) => {
            assert_eq!(expected_at_least, 50);
            assert_eq!(actual, 45);
        }
        other => panic!("Expected InvalidLength error, got {:?}", other),
    }
}

#[test]
fn test_fuzz_malformed_bytes_never_panic() {
    // Systematic bit mutation / fuzz harness to verify no panics on corrupt bytes
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    for seed in 0..500 {
        let mut hasher = DefaultHasher::new();
        seed.hash(&mut hasher);
        let h = hasher.finish();

        let len = (h % 120) as usize;
        let mut pseudo_random_bytes = Vec::with_capacity(len);
        for i in 0..len {
            pseudo_random_bytes.push(((h.wrapping_add(i as u64) >> (i % 8)) & 0xFF) as u8);
        }

        // Test from_wire_bytes
        let _ = FrameTimingCheckpoints::from_wire_bytes(&pseudo_random_bytes);

        // Test from_host_wire_bytes
        let _ = FrameTimingCheckpoints::from_host_wire_bytes(&pseudo_random_bytes);

        // Test decode_compact_realtime_with_timing
        let _ = DataEnvelope::decode_compact_realtime_with_timing(&pseudo_random_bytes);
    }
}

#[test]
fn test_serde_json_and_bincode_roundtrips() {
    let mut cp = FrameTimingCheckpoints::new(1_700_000_000_123_456);
    cp.encode_queue_ts_us = 1_200;
    cp.encode_done_ts_us = 4_500;
    cp.packetize_ts_us = 4_800;
    cp.send_ts_us = 5_000;
    cp.recv_ts_us = 12_000;
    cp.jitter_enter_ts_us = 12_200;
    cp.jitter_exit_ts_us = 15_000;
    cp.decode_enter_ts_us = 15_300;
    cp.decode_done_ts_us = 18_500;
    cp.render_submit_ts_us = 18_800;
    cp.render_done_ts_us = 22_000;

    // Bincode
    let bin = bincode::serialize(&cp).expect("bincode serialize");
    let cp_bin: FrameTimingCheckpoints = bincode::deserialize(&bin).expect("bincode deserialize");
    assert_eq!(cp, cp_bin);

    // StageLatencyStats
    let stats = StageLatencyStats {
        min_us: 100,
        p50_us: 1500,
        p95_us: 3200,
        p99_us: 4500,
        max_us: 8000,
        avg_us: 1600,
        stddev_us: 300,
        sample_count: 5000,
    };
    let bin_stats = bincode::serialize(&stats).expect("serialize stats");
    let stats_recovered: StageLatencyStats =
        bincode::deserialize(&bin_stats).expect("deserialize stats");
    assert_eq!(stats, stats_recovered);

    // TraceSpan
    let span = TraceSpan {
        name: "S1_Capture (F#1)".into(),
        cat: "host.video".into(),
        ph: 'X',
        ts_us: 1_700_000_000,
        dur_us: 1200,
        pid: 1,
        tid: 1,
    };
    let bin_span = bincode::serialize(&span).expect("serialize span");
    let span_recovered: TraceSpan = bincode::deserialize(&bin_span).expect("deserialize span");
    assert_eq!(span, span_recovered);
}
