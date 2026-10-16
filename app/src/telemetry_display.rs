//! Presentation-only telemetry state. Never infer availability from legacy zeroes.
use remote_core::{
    HostStats, SharedHostStats,
    clock_sync::{ClockEstimate, ClockSyncState},
};
use std::ops::Deref;

/// Taking a decoded frame into the UI is only a submission estimate. No GPU
/// completion or VSync callback has occurred here, so presentation stays unknown.
pub(crate) fn mark_ui_frame_submission(
    timing: &mut protocol::FrameTimingCheckpoints,
    elapsed_since_decode: std::time::Duration,
) {
    timing.render_submit_ts_us = 0;
    timing.render_done_ts_us = 0;
    if timing.capture_ts_us == 0 || timing.recv_ts_us == 0 || timing.decode_done_ts_us == 0 {
        return;
    }
    let floor = timing.decode_done_ts_us;
    if floor < timing.recv_ts_us || floor < timing.jitter_exit_ts_us || floor < timing.decode_enter_ts_us {
        return;
    }
    timing.render_submit_ts_us = u32::try_from(elapsed_since_decode.as_micros())
        .ok()
        .and_then(|elapsed| floor.checked_add(elapsed))
        .filter(|submit| *submit < u32::MAX)
        .unwrap_or(0);
}

pub(crate) struct UiHostStats {
    host: HostStats,
    pub(crate) capture_receive: CaptureReceiveDisplay,
}

impl UiHostStats {
    pub(crate) fn read(shared: &SharedHostStats) -> Self {
        Self {
            host: shared.snapshot(),
            capture_receive: CaptureReceiveDisplay::from_measurements(
                shared.estimated_e2e_latency_ms(),
                shared.clock_estimate(),
            ),
        }
    }
}

impl Deref for UiHostStats {
    type Target = HostStats;

    fn deref(&self) -> &Self::Target {
        &self.host
    }
}

pub(crate) struct CaptureReceiveDisplay {
    latency_ms: Option<f32>,
    uncertainty_ms: Option<f64>,
    clock_state: Option<ClockSyncState>,
}

impl CaptureReceiveDisplay {
    fn from_measurements(latency_ms: Option<f32>, clock: Option<ClockEstimate>) -> Self {
        let clock_state = clock.as_ref().map(|estimate| estimate.state);
        let latency_ms = latency_ms.filter(|latency| {
            clock_state == Some(ClockSyncState::Synchronized)
                && latency.is_finite()
                && *latency >= 0.0
        });
        let uncertainty_ms = clock
            .and_then(|estimate| estimate.uncertainty_us())
            .filter(|uncertainty| uncertainty.is_finite() && *uncertainty >= 0.0)
            .map(|uncertainty| uncertainty / 1000.0);
        Self {
            latency_ms,
            uncertainty_ms,
            clock_state,
        }
    }

    pub(crate) fn value(&self) -> String {
        match (self.latency_ms, self.uncertainty_ms) {
            (Some(latency), Some(uncertainty)) => {
                format!("≈ {latency:.1} ± {uncertainty:.1} ms")
            }
            (Some(latency), None) => format!("≈ {latency:.1} ms"),
            (None, _) => "—".into(),
        }
    }

    pub(crate) fn detail(&self) -> String {
        let scope = "Host capture to client receive estimate; excludes decoding and display.";
        if self.latency_ms.is_some() {
            let uncertainty = self.uncertainty_ms.map_or_else(
                || "Model clock uncertainty unavailable.".to_string(),
                |uncertainty| format!(
                    "Model clock uncertainty ± {uncertainty:.1} ms; not a guaranteed accuracy bound."
                ),
            );
            format!("{scope} {uncertainty}")
        } else {
            let reason = match self.clock_state {
                None => "awaiting clock measurements",
                Some(ClockSyncState::Acquiring) => "acquiring clock mapping",
                Some(ClockSyncState::Stale) => "clock mapping expired",
                Some(ClockSyncState::Synchronized) => "awaiting a valid capture/receive sample",
            };
            format!("{scope} Unavailable: {reason}.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remote_core::clock_sync::ClockSynchronizer;

    #[test]
    fn ui_submission_never_fabricates_presentation_or_wraps_invalid_timing() {
        let mut timing = protocol::FrameTimingCheckpoints::new(1_000_000);
        timing.recv_ts_us = 10_000;
        timing.jitter_exit_ts_us = 11_000;
        timing.decode_done_ts_us = 13_000;
        timing.render_done_ts_us = 99_000;
        mark_ui_frame_submission(&mut timing, std::time::Duration::from_micros(2_000));
        assert_eq!(timing.render_submit_ts_us, 15_000);
        assert_eq!(timing.render_done_ts_us, 0);
        assert_eq!(timing.total_e2e_duration_us(), None);
        assert_eq!(timing.render_presentation_duration_us(), None);
        for (capture, decoded, elapsed) in [
            (0, 13_000, 2_000),
            (1_000_000, 0, 2_000),
            (1_000_000, 9_000, 2_000),
            (1_000_000, u32::MAX, 2_000),
            (1_000_000, u32::MAX, 0),
            (1_000_000, 13_000, u64::MAX),
        ] {
            timing.capture_ts_us = capture;
            timing.decode_done_ts_us = decoded;
            mark_ui_frame_submission(&mut timing, std::time::Duration::from_micros(elapsed));
            assert_eq!(timing.render_submit_ts_us, 0);
            assert_eq!(timing.render_done_ts_us, 0);
        }
        timing.capture_ts_us = 1_000_000;
        timing.decode_done_ts_us = 13_000;
        timing.decode_enter_ts_us = 14_000;
        mark_ui_frame_submission(&mut timing, std::time::Duration::ZERO);
        assert_eq!(timing.render_submit_ts_us, 0);
        assert_eq!(timing.render_done_ts_us, 0);
    }

    fn synchronized_clock() -> ClockEstimate {
        let mut clock = ClockSynchronizer::new();
        for probe_id in 1..=4 {
            let sent = probe_id * 1_000_000;
            clock.begin_probe(probe_id, sent).unwrap();
            clock
                .complete_probe(probe_id, sent, sent + 1500, sent + 1600, sent + 1100)
                .unwrap();
        }
        let estimate = clock.estimate(4_001_100).unwrap();
        assert_eq!(estimate.state, ClockSyncState::Synchronized);
        estimate
    }

    #[test]
    fn missing_acquiring_and_expired_measurements_are_unavailable() {
        let missing = CaptureReceiveDisplay::from_measurements(Some(12.5), None);
        assert_eq!(missing.value(), "—");
        assert!(missing.detail().contains("awaiting clock measurements"));
        for (state, reason) in [
            (ClockSyncState::Acquiring, "acquiring clock mapping"),
            (ClockSyncState::Stale, "clock mapping expired"),
        ] {
            let mut clock = synchronized_clock();
            clock.state = state;
            let display = CaptureReceiveDisplay::from_measurements(Some(12.5), Some(clock));
            assert_eq!(display.value(), "—");
            assert!(display.detail().contains(reason));
        }
    }

    #[test]
    fn fresh_estimate_shows_model_uncertainty_and_limited_scope() {
        let clock = synchronized_clock();
        let uncertainty_ms = clock.uncertainty_us().unwrap() / 1000.0;
        let display = CaptureReceiveDisplay::from_measurements(Some(12.5), Some(clock));
        assert_eq!(display.value(), format!("≈ 12.5 ± {uncertainty_ms:.1} ms"));
        assert!(display.detail().contains("excludes decoding and display"));
        assert!(display.detail().contains("not a guaranteed accuracy bound"));
    }

    #[test]
    fn valid_zero_is_distinct_from_missing_without_clamping_invalid_values() {
        let clock = synchronized_clock();
        let zero = CaptureReceiveDisplay::from_measurements(Some(0.0), Some(clock));
        assert!(zero.value().starts_with("≈ 0.0"));
        for latency in [None, Some(-1.0), Some(f32::NAN), Some(f32::INFINITY)] {
            let display = CaptureReceiveDisplay::from_measurements(latency, Some(clock));
            assert_eq!(display.value(), "—");
        }
    }

    #[test]
    fn unknown_uncertainty_is_not_reported_as_zero_error() {
        let mut clock = synchronized_clock();
        clock.fit_ci95_us = None;
        let display = CaptureReceiveDisplay::from_measurements(Some(12.5), Some(clock));
        assert_eq!(display.value(), "≈ 12.5 ms");
        assert!(display.detail().contains("uncertainty unavailable"));
    }

    #[test]
    fn default_shared_stats_do_not_turn_unknown_into_zero_latency() {
        let shared = SharedHostStats::default();
        let display = UiHostStats::read(&shared);
        assert_eq!(display.capture_receive.value(), "—");
        assert!(display.capture_receive.detail().contains("Unavailable"));
    }
}
