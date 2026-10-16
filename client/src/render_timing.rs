//! Renderer telemetry stops at UI submission, before GPU completion or display.
//! All input checkpoints are capture-relative offsets established at ingress;
//! the only new duration comes from the decoder's local monotonic instant.
//! Decoder queue wait has no checkpoint in the existing decode path. These
//! accumulated tracked stages therefore estimate only part of the latency,
//! excluding decoder queue wait and actual display, even with a fresh clock fit.

use protocol::FrameTimingCheckpoints;
use std::time::Duration;

#[derive(Debug, Default, PartialEq)]
pub(crate) struct SubmitLatencies {
    pub(crate) receive_to_submit_ms: Option<f32>,
    pub(crate) capture_to_submit_ms: Option<f32>,
}

pub(crate) fn stamp_ui_submission(
    timing: &mut FrameTimingCheckpoints,
    since_decode: Duration,
    clock_is_fresh: bool,
) -> SubmitLatencies {
    timing.render_submit_ts_us = 0;
    // Taking the frame into a UI slot is not evidence of GPU/VSync completion.
    timing.render_done_ts_us = 0;
    if timing.capture_ts_us == 0
        || timing.recv_ts_us == 0
        || timing.decode_done_ts_us < timing.recv_ts_us
        || timing.decode_done_ts_us < timing.jitter_exit_ts_us
        || timing.decode_done_ts_us < timing.decode_enter_ts_us
    {
        return SubmitLatencies::default();
    }
    let Some(submit_us) = u32::try_from(since_decode.as_micros())
        .ok()
        .and_then(|elapsed| timing.decode_done_ts_us.checked_add(elapsed))
        .filter(|submit| *submit < u32::MAX)
    else {
        // A saturated/overflowed checkpoint is unavailable, never wrapped or 0ms.
        return SubmitLatencies::default();
    };
    timing.render_submit_ts_us = submit_us;
    SubmitLatencies {
        receive_to_submit_ms: Some((submit_us - timing.recv_ts_us) as f32 / 1_000.0),
        // Local stage differences remain useful even if the mapping later expires.
        capture_to_submit_ms: clock_is_fresh.then_some(submit_us as f32 / 1_000.0),
    }
}

#[derive(Debug, Default)]
pub(crate) struct SmoothedLatency {
    latency_ms: Option<f32>,
    jitter_ms: Option<f32>,
}

impl SmoothedLatency {
    pub(crate) fn record(&mut self, sample_ms: Option<f32>) {
        let Some(sample) = sample_ms.filter(|sample| sample.is_finite() && *sample >= 0.0) else {
            *self = Self::default();
            return;
        };
        if let Some(previous) = self.latency_ms {
            let difference = (sample - previous).abs();
            self.jitter_ms = Some(
                self.jitter_ms
                    .map_or(difference, |jitter| jitter + (difference - jitter) / 16.0),
            );
            self.latency_ms = Some(previous + (sample - previous) / 16.0);
        } else {
            self.latency_ms = Some(sample);
            // Jitter needs a second measurement; zero is a real value, not a sentinel.
            self.jitter_ms = None;
        }
    }

    pub(crate) fn latency_text(&self, available: bool) -> String {
        Self::value_text(self.latency_ms, available)
    }

    pub(crate) fn jitter_text(&self, available: bool) -> String {
        Self::value_text(self.jitter_ms, available)
    }

    fn value_text(value: Option<f32>, available: bool) -> String {
        match value.filter(|_| available) {
            Some(value) => format!("{value:.1} ms"),
            None => "unavailable".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded_timing() -> FrameTimingCheckpoints {
        let mut timing = FrameTimingCheckpoints::new(9_000_000_000_000);
        timing.recv_ts_us = 20_000;
        timing.jitter_exit_ts_us = 21_000;
        timing.decode_enter_ts_us = 21_000;
        timing.decode_done_ts_us = 24_000;
        timing
    }

    #[test]
    fn submission_uses_relative_stages_and_monotonic_elapsed_not_wall_time() {
        for capture_ts_us in [1, 9_000_000_000_000, u64::MAX] {
            let mut timing = decoded_timing();
            timing.capture_ts_us = capture_ts_us;
            let sample = stamp_ui_submission(&mut timing, Duration::from_micros(3_500), true);
            assert_eq!(sample.receive_to_submit_ms, Some(7.5));
            assert_eq!(sample.capture_to_submit_ms, Some(27.5));
            assert_eq!(timing.render_submit_ts_us, 27_500);
            assert_eq!(timing.render_done_ts_us, 0);
            assert_eq!(timing.render_presentation_duration_us(), None);
            assert_eq!(timing.total_e2e_duration_us(), None);
        }
    }

    #[test]
    fn missing_or_negative_stages_are_unavailable_without_fallback() {
        let base = decoded_timing();
        let mut missing_capture = base;
        missing_capture.capture_ts_us = 0;
        let mut missing_receive = base;
        missing_receive.recv_ts_us = 0;
        let mut missing_decode = base;
        missing_decode.decode_done_ts_us = 0;
        let mut negative_receive = base;
        negative_receive.recv_ts_us = base.decode_done_ts_us + 1;
        let mut negative_decode = base;
        negative_decode.decode_enter_ts_us = base.decode_done_ts_us + 1;
        let mut negative_jitter = base;
        negative_jitter.jitter_exit_ts_us = base.decode_done_ts_us + 1;
        for mut timing in [
            missing_capture,
            missing_receive,
            missing_decode,
            negative_receive,
            negative_decode,
            negative_jitter,
        ] {
            timing.render_submit_ts_us = 98_000;
            timing.render_done_ts_us = 99_000;
            assert_eq!(
                stamp_ui_submission(&mut timing, Duration::from_millis(1), true),
                SubmitLatencies::default()
            );
            assert_eq!(timing.render_submit_ts_us, 0);
            assert_eq!(timing.render_done_ts_us, 0);
        }
    }

    #[test]
    fn expired_mapping_hides_cross_machine_estimate_but_keeps_local_duration() {
        let mut timing = decoded_timing();
        let sample = stamp_ui_submission(&mut timing, Duration::from_millis(2), false);
        assert_eq!(sample.capture_to_submit_ms, None);
        assert_eq!(sample.receive_to_submit_ms, Some(6.0));
        assert_eq!(timing.render_submit_ts_us, 26_000);
        assert_eq!(timing.render_done_ts_us, 0);
    }

    #[test]
    fn overflow_or_saturated_stage_is_unavailable_instead_of_wrapping() {
        for elapsed in [
            Duration::from_micros(u32::MAX as u64),
            Duration::from_micros(u32::MAX as u64 + 1),
            Duration::MAX,
        ] {
            let mut timing = decoded_timing();
            assert_eq!(
                stamp_ui_submission(&mut timing, elapsed, true),
                SubmitLatencies::default()
            );
        }
        let mut timing = decoded_timing();
        timing.decode_done_ts_us = u32::MAX;
        assert_eq!(
            stamp_ui_submission(&mut timing, Duration::ZERO, true),
            SubmitLatencies::default()
        );
    }

    #[test]
    fn valid_zero_is_distinct_from_missing_and_jitter_needs_two_samples() {
        let mut timing = decoded_timing();
        timing.decode_done_ts_us = timing.recv_ts_us;
        timing.decode_enter_ts_us = timing.recv_ts_us;
        timing.jitter_exit_ts_us = timing.recv_ts_us;
        let sample = stamp_ui_submission(&mut timing, Duration::ZERO, true);
        assert_eq!(sample.receive_to_submit_ms, Some(0.0));
        let mut stats = SmoothedLatency::default();
        assert_eq!(stats.latency_text(true), "unavailable");
        stats.record(sample.receive_to_submit_ms);
        assert_eq!(stats.latency_text(true), "0.0 ms");
        assert_eq!(stats.jitter_text(true), "unavailable");
        stats.record(Some(0.0));
        assert_eq!(stats.jitter_text(true), "0.0 ms");
    }

    #[test]
    fn unavailable_resets_smoothing_and_stale_display_never_exposes_old_value() {
        for invalid in [None, Some(-1.0), Some(f32::NAN), Some(f32::INFINITY)] {
            let mut stats = SmoothedLatency::default();
            stats.record(Some(10.0));
            stats.record(Some(26.0));
            assert_eq!(stats.latency_text(true), "11.0 ms");
            assert_eq!(stats.jitter_text(true), "16.0 ms");
            assert_eq!(stats.latency_text(false), "unavailable");
            assert_eq!(stats.jitter_text(false), "unavailable");
            stats.record(invalid);
            assert_eq!(stats.latency_text(true), "unavailable");
            assert_eq!(stats.jitter_text(true), "unavailable");
            stats.record(Some(50.0));
            assert_eq!(stats.latency_text(true), "50.0 ms");
            assert_eq!(stats.jitter_text(true), "unavailable");
        }
    }
}
