//! Bounded, four-timestamp clock measurement in process-anchored monotonic domains.
//!
//! A sample estimates `host_time = client_time + offset`; it never changes either
//! system clock. Every timestamp must come from that endpoint's `quanta_now_us`
//! (or a clone of its global `HighPrecisionClock`). Epoch-shaped values do NOT
//! mean the two machines' wall clocks are synchronized.
//!
//! Callers authenticate the exchange and call `reset` when the authenticated
//! session, remote clock epoch, or network path changes. Probe matching is a
//! measurement-integrity check, not authentication or an anti-replay protocol.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::time::{Duration, SystemTime};

/// Local continuity detector only. Wall time never enters clock-offset fitting,
/// authorization, liveness or input deadlines. A discontinuity invalidates the
/// measurement epoch; it does not repair either operating system clock.
#[derive(Clone, Debug, Default)]
pub struct ClockContinuity {
    previous: Option<(u64, SystemTime)>,
}

impl ClockContinuity {
    pub fn new() -> Self {
        Self::default()
    }

    /// True on first observation and while ticks remain continuous. Backward
    /// wall/monotonic time, elapsed disagreement >1s, or a monotonic tick gap >10s
    /// returns false. Every observation becomes the baseline for the next tick.
    pub fn observe(&mut self, monotonic_us: u64, wall: SystemTime) -> bool {
        let previous = self.previous.replace((monotonic_us, wall));
        let Some((previous_us, previous_wall)) = previous else {
            return true;
        };
        let Some(monotonic_elapsed_us) = monotonic_us.checked_sub(previous_us) else {
            return false;
        };
        let Ok(wall_elapsed) = wall.duration_since(previous_wall) else {
            return false;
        };
        let monotonic_elapsed = Duration::from_micros(monotonic_elapsed_us);
        monotonic_elapsed <= Duration::from_secs(10)
            && monotonic_elapsed.abs_diff(wall_elapsed) <= Duration::from_secs(1)
    }

    pub fn reset(&mut self) {
        self.previous = None;
    }
}

/// All durations are microseconds in the local anchored monotonic clock domain.
#[derive(Clone, Debug)]
pub struct ClockSyncConfig {
    pub sample_capacity: usize,
    pub sample_window_us: u64,
    pub max_pending_probes: usize,
    pub probe_timeout_us: u64,
    pub freshness_limit_us: u64,
    pub min_fit_samples: usize,
    pub min_fit_span_us: u64,
    /// Fit samples with network RTT <= minimum RTT + max(slack, minimum * ratio).
    pub low_rtt_slack_us: u64,
    pub low_rtt_ratio: f64,
    /// Assumed unmodelled oscillator wander during holdover, not a hardware guarantee.
    pub holdover_uncertainty_ppm: f64,
}

impl Default for ClockSyncConfig {
    fn default() -> Self {
        Self {
            sample_capacity: 64,
            sample_window_us: 60_000_000,
            max_pending_probes: 16,
            probe_timeout_us: 5_000_000,
            freshness_limit_us: 10_000_000,
            min_fit_samples: 4,
            min_fit_span_us: 3_000_000,
            low_rtt_slack_us: 250,
            low_rtt_ratio: 0.25,
            holdover_uncertainty_ppm: 100.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockSyncError {
    InvalidConfig,
    ProbeIdNotIncreasing,
    PendingCapacity,
    LocalTimeReversed,
    UnknownOrCompletedProbe,
    EchoMismatch,
    ExpiredProbe,
    InvalidTimestampOrder,
    HostProcessingExceedsRoundTrip,
    OffsetOutOfRange,
}

impl fmt::Display for ClockSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfig => "invalid clock synchronizer configuration",
            Self::ProbeIdNotIncreasing => "probe ID must increase until the synchronizer is reset",
            Self::PendingCapacity => "clock probe pending window is full",
            Self::LocalTimeReversed => {
                "local monotonic time moved backwards; reset the clock epoch"
            }
            Self::UnknownOrCompletedProbe => "unknown, expired, or already completed clock probe",
            Self::EchoMismatch => "clock response does not echo the locally stored send timestamp",
            Self::ExpiredProbe => "clock probe response arrived after its deadline",
            Self::InvalidTimestampOrder => "clock probe timestamps are not causally ordered",
            Self::HostProcessingExceedsRoundTrip => "host processing exceeds the local round trip",
            Self::OffsetOutOfRange => {
                "clock offset does not fit the supported signed microsecond range"
            }
        })
    }
}

impl std::error::Error for ClockSyncError {}

/// A validated exchange. Values are raw measurements: zero RTT is permitted and
/// no artificial minimum, millisecond clamp, EWMA, or outlier replacement is used.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockSample {
    pub probe_id: u64,
    pub client_send_us: u64,
    pub host_recv_us: u64,
    pub host_send_us: u64,
    pub client_recv_us: u64,
    pub raw_total_rtt_us: u64,
    pub host_processing_us: u64,
    pub network_rtt_us: u64,
    pub offset_us: f64,
    offset_twice_us: i128,
    client_midpoint_twice_us: i128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockSyncState {
    /// A usable offset sample exists, but the count/time span cannot fit drift yet.
    Acquiring,
    /// A fresh affine fit exists. This is not a claim that path asymmetry vanished.
    Synchronized,
    /// The last selected low-RTT sample is old or the query clock went backwards.
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockEstimate {
    pub state: ClockSyncState,
    /// Offset evaluated at `client_time_us`, not at a remote/wall-clock epoch.
    pub client_time_us: u64,
    pub offset_us: f64,
    /// Zero while acquiring; `state` and `drift_estimated` distinguish this from a fit.
    pub drift_ppm: f64,
    pub drift_estimated: bool,
    /// Approximate 95% residual fit interval at the query time, in microseconds.
    /// None with only one sample. Assumes independent stationary residuals; this
    /// does not cover a fixed asymmetric path bias or guarantee 95% coverage.
    pub fit_ci95_us: Option<f64>,
    /// Per-probe network RTT/2 uncertainty propagated through absolute affine
    /// fit weights. At extrapolated times this can exceed the samples' RTT/2.
    /// Separate from the residual fit interval: constant asymmetry is invisible
    /// to residuals. Assumes nonnegative delays and an affine clock in the window.
    pub asymmetry_bound_us: f64,
    /// Additional assumed oscillator-wander allowance since the newest fit sample.
    pub holdover_bound_us: f64,
    /// Floating-point/clock timestamp quantization allowance, separate from fit CI.
    pub quantization_bound_us: f64,
    pub sample_age_us: u64,
    pub sample_count: usize,
    pub selected_sample_count: usize,
    pub selected_span_us: u64,
    /// Most recently received valid sample, even when excluded by low-RTT selection.
    pub latest_sample: ClockSample,
}

impl ClockEstimate {
    /// Combined model allowance, not a certified clock-accuracy or safety bound.
    /// None until residual variability can be estimated from at least two samples.
    pub fn uncertainty_us(&self) -> Option<f64> {
        self.fit_ci95_us.map(|ci| {
            ci + self.asymmetry_bound_us + self.holdover_bound_us + self.quantization_bound_us
        })
    }
}

#[derive(Clone, Debug)]
struct ClockFit {
    reference_client_us: u64,
    reference_offset_twice_us: i128,
    mean_x_us: f64,
    mean_y_us: f64,
    slope: f64,
    sxx: f64,
    residual_variance: Option<f64>,
    residual_degrees_of_freedom: usize,
    drift_estimated: bool,
    newest_recv_us: u64,
    selected_span_us: u64,
    /// Centered local sample coordinates and each sample's intrinsic RTT/2 bound.
    selected: Vec<(f64, f64)>,
}

/// A per-session synchronizer with bounded pending probes and sample history.
/// Probe IDs must be strictly increasing; call `reset` for a new session/epoch.
#[derive(Clone, Debug)]
pub struct ClockSynchronizer {
    config: ClockSyncConfig,
    pending: BTreeMap<u64, u64>,
    highest_probe_id: Option<u64>,
    latest_local_us: Option<u64>,
    samples: VecDeque<ClockSample>,
    fit: Option<ClockFit>,
}

impl Default for ClockSynchronizer {
    fn default() -> Self {
        Self::new()
    }
}

impl ClockSynchronizer {
    pub fn new() -> Self {
        Self::with_config(ClockSyncConfig::default()).expect("valid default clock sync config")
    }

    pub fn with_config(config: ClockSyncConfig) -> Result<Self, ClockSyncError> {
        if !(3..=4096).contains(&config.sample_capacity)
            || config.max_pending_probes == 0
            || config.max_pending_probes > 4096
            || config.probe_timeout_us == 0
            || config.sample_window_us < config.probe_timeout_us
            || config.freshness_limit_us == 0
            || config.freshness_limit_us > config.sample_window_us
            || config.min_fit_samples < 3
            || config.min_fit_samples > config.sample_capacity
            || config.min_fit_span_us == 0
            || config.min_fit_span_us > config.sample_window_us
            || !config.low_rtt_ratio.is_finite()
            || config.low_rtt_ratio < 0.0
            || config.low_rtt_ratio > 1.0
            || !config.holdover_uncertainty_ppm.is_finite()
            || config.holdover_uncertainty_ppm < 0.0
            || config.holdover_uncertainty_ppm > 1_000_000.0
        {
            return Err(ClockSyncError::InvalidConfig);
        }
        Ok(Self {
            config,
            pending: BTreeMap::new(),
            highest_probe_id: None,
            latest_local_us: None,
            samples: VecDeque::new(),
            fit: None,
        })
    }

    /// Register immediately before sending. Use this exact stored timestamp in
    /// the authenticated probe payload. Failed sends can be cancelled explicitly.
    pub fn begin_probe(&mut self, probe_id: u64, local_send_us: u64) -> Result<(), ClockSyncError> {
        self.check_local_time(local_send_us)?;
        if self
            .highest_probe_id
            .is_some_and(|previous| probe_id <= previous)
        {
            return Err(ClockSyncError::ProbeIdNotIncreasing);
        }
        self.pending
            .retain(|_, sent| local_send_us - *sent <= self.config.probe_timeout_us);
        if self.pending.len() >= self.config.max_pending_probes {
            return Err(ClockSyncError::PendingCapacity);
        }
        self.pending.insert(probe_id, local_send_us);
        self.highest_probe_id = Some(probe_id);
        self.latest_local_us = Some(local_send_us);
        Ok(())
    }

    /// Cancel a failed send without permitting that probe ID to be reused.
    pub fn cancel_probe(&mut self, probe_id: u64) -> bool {
        self.pending.remove(&probe_id).is_some()
    }

    /// Complete an authenticated response. t1 is read from local state and the
    /// echo must match it exactly. A matched response consumes its pending slot,
    /// including invalid/expired measurements, so it cannot later update a fit.
    pub fn complete_probe(
        &mut self,
        probe_id: u64,
        echoed_send_us: u64,
        host_recv_us: u64,
        host_send_us: u64,
        local_recv_us: u64,
    ) -> Result<ClockSample, ClockSyncError> {
        let client_send_us = *self
            .pending
            .get(&probe_id)
            .ok_or(ClockSyncError::UnknownOrCompletedProbe)?;
        if echoed_send_us != client_send_us {
            return Err(ClockSyncError::EchoMismatch);
        }
        self.pending.remove(&probe_id);
        self.check_local_time(local_recv_us)?;
        let raw_total_rtt_us = local_recv_us
            .checked_sub(client_send_us)
            .ok_or(ClockSyncError::InvalidTimestampOrder)?;
        if raw_total_rtt_us > self.config.probe_timeout_us {
            return Err(ClockSyncError::ExpiredProbe);
        }
        let host_processing_us = host_send_us
            .checked_sub(host_recv_us)
            .ok_or(ClockSyncError::InvalidTimestampOrder)?;
        let network_rtt_us = raw_total_rtt_us
            .checked_sub(host_processing_us)
            .ok_or(ClockSyncError::HostProcessingExceedsRoundTrip)?;
        // Wide signed arithmetic avoids u64 subtraction, i64 casting, and sum overflow.
        let offset_twice_us = (host_recv_us as i128 - client_send_us as i128)
            + (host_send_us as i128 - local_recv_us as i128);
        if !(2 * i64::MIN as i128..=2 * i64::MAX as i128).contains(&offset_twice_us) {
            return Err(ClockSyncError::OffsetOutOfRange);
        }
        let sample = ClockSample {
            probe_id,
            client_send_us,
            host_recv_us,
            host_send_us,
            client_recv_us: local_recv_us,
            raw_total_rtt_us,
            host_processing_us,
            network_rtt_us,
            offset_us: offset_twice_us as f64 / 2.0,
            offset_twice_us,
            client_midpoint_twice_us: client_send_us as i128 + local_recv_us as i128,
        };
        if self.sample_changes_clock_epoch(&sample) {
            // A suspend/resume, remote clock-epoch change or major path shift
            // must reacquire. Replies from all older pending probes are retired
            // together; retain this already validated sample and ID high-water mark.
            self.invalidate_measurements();
        }
        self.latest_local_us = Some(local_recv_us);
        while self.samples.front().is_some_and(|old| {
            local_recv_us - old.client_recv_us > self.config.sample_window_us
                || self.samples.len() >= self.config.sample_capacity
        }) {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        self.refit();
        Ok(sample)
    }

    /// Query the affine mapping at local monotonic `now_us`. Returns None until
    /// one valid response exists. Stale fits remain visible for diagnostics; users
    /// of the mapping must inspect `state` instead of treating Some as fresh.
    pub fn estimate(&self, now_us: u64) -> Option<ClockEstimate> {
        let fit = self.fit.as_ref()?;
        let x = (now_us as i128 - fit.reference_client_us as i128) as f64;
        let centered_x = x - fit.mean_x_us;
        let offset_us =
            fit.reference_offset_twice_us as f64 / 2.0 + fit.mean_y_us + fit.slope * centered_x;
        let sample_age_us = now_us.saturating_sub(fit.newest_recv_us);
        let n = fit.selected.len() as f64;
        let leverage = 1.0 / n
            + if fit.drift_estimated {
                centered_x.powi(2) / fit.sxx
            } else {
                0.0
            };
        let fit_ci95_us = fit.residual_variance.map(|variance| {
            student_t_975(fit.residual_degrees_of_freedom) * (variance * leverage).sqrt()
        });
        let asymmetry_bound_us = fit
            .selected
            .iter()
            .map(|(sample_x, half_rtt)| {
                let weight = 1.0 / n
                    + if fit.drift_estimated {
                        centered_x * sample_x / fit.sxx
                    } else {
                        0.0
                    };
                weight.abs() * half_rtt
            })
            .sum();
        let reversed = self.latest_local_us.is_some_and(|latest| now_us < latest);
        let state = if reversed || sample_age_us > self.config.freshness_limit_us {
            ClockSyncState::Stale
        } else if fit.drift_estimated {
            ClockSyncState::Synchronized
        } else {
            ClockSyncState::Acquiring
        };
        Some(ClockEstimate {
            state,
            client_time_us: now_us,
            offset_us,
            drift_ppm: fit.slope * 1_000_000.0,
            drift_estimated: fit.drift_estimated,
            fit_ci95_us,
            asymmetry_bound_us,
            holdover_bound_us: sample_age_us as f64 * self.config.holdover_uncertainty_ppm
                / 1_000_000.0,
            quantization_bound_us: 1.0 + offset_us.abs() * f64::EPSILON,
            sample_age_us,
            sample_count: self.samples.len(),
            selected_sample_count: fit.selected.len(),
            selected_span_us: fit.selected_span_us,
            latest_sample: *self.samples.back()?,
        })
    }

    /// Invalidate measurements within the SAME authenticated session. Pending
    /// responses are retired but the ID high-water mark is retained, so stale
    /// replies cannot match reused IDs after sleep or a local continuity break.
    pub fn invalidate_measurements(&mut self) {
        self.pending.clear();
        self.latest_local_us = None;
        self.samples.clear();
        self.fit = None;
    }

    /// Drop the previous clock/path epoch, samples and pending responses together.
    /// Use for a NEW authenticated session; the caller must reject old-session
    /// responses. For same-session continuity loss use invalidate_measurements.
    pub fn reset(&mut self) {
        self.invalidate_measurements();
        self.highest_probe_id = None;
    }

    fn sample_changes_clock_epoch(&self, sample: &ClockSample) -> bool {
        let Some(fit) = self.fit.as_ref() else {
            return false;
        };
        // Center before float conversion, including when absolute epochs/offsets
        // are very large. Compare the model at this exchange's local midpoint.
        let x =
            (sample.client_midpoint_twice_us - 2 * fit.reference_client_us as i128) as f64 / 2.0;
        let measured_delta = (sample.offset_twice_us - fit.reference_offset_twice_us) as f64 / 2.0;
        let residual = (measured_delta - fit.mean_y_us - fit.slope * (x - fit.mean_x_us)).abs();
        let midpoint = (sample.client_midpoint_twice_us / 2) as u64;
        let Some(previous) = self.estimate(midpoint) else {
            return false;
        };
        // Missing residual CI for a one-sample acquisition is not a zero error
        // guarantee: its full path, holdover and quantization allowances remain.
        let old_allowance = previous.fit_ci95_us.unwrap_or(0.0)
            + previous.asymmetry_bound_us
            + previous.holdover_bound_us
            + previous.quantization_bound_us;
        residual > old_allowance + sample.network_rtt_us as f64 / 2.0 + 1_000.0
    }

    pub fn pending_probe_count(&self) -> usize {
        self.pending.len()
    }

    fn check_local_time(&self, now_us: u64) -> Result<(), ClockSyncError> {
        if self.latest_local_us.is_some_and(|latest| now_us < latest) {
            Err(ClockSyncError::LocalTimeReversed)
        } else {
            Ok(())
        }
    }

    fn refit(&mut self) {
        let min_rtt = self
            .samples
            .iter()
            .map(|s| s.network_rtt_us)
            .min()
            .unwrap_or(0) as f64;
        let threshold = min_rtt
            + (self.config.low_rtt_slack_us as f64).max(min_rtt * self.config.low_rtt_ratio);
        let selected: Vec<_> = self
            .samples
            .iter()
            .filter(|s| s.network_rtt_us as f64 <= threshold)
            .collect();
        let first = selected[0];
        let reference_client_us = first.client_send_us;
        let reference_offset_twice_us = first.offset_twice_us;
        // Subtract integer anchors BEFORE conversion to f64. Never regress raw
        // epoch-sized floats or subtract two already-rounded absolute timestamps.
        let xy: Vec<_> = selected
            .iter()
            .map(|s| {
                (
                    (s.client_midpoint_twice_us - 2 * reference_client_us as i128) as f64 / 2.0,
                    (s.offset_twice_us - reference_offset_twice_us) as f64 / 2.0,
                )
            })
            .collect();
        let n = xy.len() as f64;
        let mean_x_us = xy.iter().map(|v| v.0).sum::<f64>() / n;
        let mean_y_us = xy.iter().map(|v| v.1).sum::<f64>() / n;
        let sxx = xy.iter().map(|v| (v.0 - mean_x_us).powi(2)).sum::<f64>();
        let sxy = xy
            .iter()
            .map(|v| (v.0 - mean_x_us) * (v.1 - mean_y_us))
            .sum::<f64>();
        let min_midpoint = selected
            .iter()
            .map(|s| s.client_midpoint_twice_us)
            .min()
            .unwrap();
        let max_midpoint = selected
            .iter()
            .map(|s| s.client_midpoint_twice_us)
            .max()
            .unwrap();
        let selected_span_us = ((max_midpoint - min_midpoint) / 2) as u64;
        let drift_estimated = xy.len() >= self.config.min_fit_samples
            && selected_span_us >= self.config.min_fit_span_us
            && sxx > 0.0;
        let slope = if drift_estimated { sxy / sxx } else { 0.0 };
        let residual_degrees_of_freedom =
            xy.len().saturating_sub(if drift_estimated { 2 } else { 1 });
        let residual_variance = (residual_degrees_of_freedom > 0).then(|| {
            xy.iter()
                .map(|(x, y)| (y - mean_y_us - slope * (x - mean_x_us)).powi(2))
                .sum::<f64>()
                / residual_degrees_of_freedom as f64
        });
        self.fit = Some(ClockFit {
            reference_client_us,
            reference_offset_twice_us,
            mean_x_us,
            mean_y_us,
            slope,
            sxx,
            residual_variance,
            residual_degrees_of_freedom,
            drift_estimated,
            newest_recv_us: selected.iter().map(|s| s.client_recv_us).max().unwrap(),
            selected_span_us,
            selected: xy
                .iter()
                .zip(selected.iter())
                .map(|(xy, sample)| (xy.0 - mean_x_us, sample.network_rtt_us as f64 / 2.0))
                .collect(),
        });
    }
}

// Two-sided 95% Student-t multipliers. Larger-degree buckets round UP to a
// smaller degree of freedom, avoiding overconfident tiny-sample normal intervals.
fn student_t_975(df: usize) -> f64 {
    const SMALL: [f64; 31] = [
        0.0, 12.7063, 4.3027, 3.1825, 2.7765, 2.5706, 2.4469, 2.3647, 2.3061, 2.2622, 2.2282,
        2.2010, 2.1789, 2.1604, 2.1448, 2.1315, 2.1199, 2.1099, 2.1009, 2.0931, 2.0860, 2.0797,
        2.0739, 2.0687, 2.0639, 2.0596, 2.0556, 2.0519, 2.0485, 2.0453, 2.0423,
    ];
    match df {
        0..=30 => SMALL[df],
        31..=59 => 2.0423,
        60..=119 => 2.0003,
        _ => 1.97994,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(
        sync: &mut ClockSynchronizer,
        id: u64,
        t1: u64,
        offset: i64,
        forward: u64,
        processing: u64,
        backward: u64,
    ) -> ClockSample {
        let t2 = u64::try_from(t1 as i128 + offset as i128 + forward as i128).unwrap();
        let t3 = t2 + processing;
        let t4 = t1 + forward + processing + backward;
        sync.begin_probe(id, t1).unwrap();
        sync.complete_probe(id, t1, t2, t3, t4).unwrap()
    }

    fn regular_fit(sync: &mut ClockSynchronizer, offset: i64) -> u64 {
        let mut last = 0;
        for id in 0..6 {
            last = exchange(
                sync,
                id,
                1_700_000_000_000_000 + id * 1_000_000,
                offset,
                1_000,
                100,
                1_000,
            )
            .client_recv_us;
        }
        last
    }

    #[test]
    fn four_timestamps_keep_raw_and_processing_adjusted_rtt() {
        let mut sync = ClockSynchronizer::new();
        assert!(sync.estimate(0).is_none());
        let sample = exchange(&mut sync, 1, 1_000_000, 0, 5_000, 1_000, 6_000);
        assert_eq!(sample.raw_total_rtt_us, 12_000);
        assert_eq!(sample.host_processing_us, 1_000);
        assert_eq!(sample.network_rtt_us, 11_000);
        assert_eq!(sample.offset_us, -500.0);
        let estimate = sync.estimate(sample.client_recv_us).unwrap();
        assert_eq!(estimate.state, ClockSyncState::Acquiring);
        assert_eq!(estimate.offset_us, -500.0);
        assert_eq!(estimate.asymmetry_bound_us, 5_500.0);
        assert!(estimate.fit_ci95_us.is_none());
        assert!(estimate.uncertainty_us().is_none());
    }

    #[test]
    fn probe_must_exist_and_echo_the_locally_stored_t1() {
        let mut sync = ClockSynchronizer::new();
        assert_eq!(
            sync.complete_probe(1, 100, 200, 200, 300),
            Err(ClockSyncError::UnknownOrCompletedProbe)
        );
        sync.begin_probe(1, 100).unwrap();
        assert_eq!(
            sync.complete_probe(1, 101, 200, 200, 300),
            Err(ClockSyncError::EchoMismatch)
        );
        assert_eq!(sync.pending_probe_count(), 1);
        sync.complete_probe(1, 100, 200, 200, 300).unwrap();
        assert_eq!(
            sync.complete_probe(1, 100, 200, 200, 300),
            Err(ClockSyncError::UnknownOrCompletedProbe)
        );
        assert_eq!(
            sync.begin_probe(1, 400),
            Err(ClockSyncError::ProbeIdNotIncreasing)
        );
        assert_eq!(
            sync.begin_probe(0, 400),
            Err(ClockSyncError::ProbeIdNotIncreasing)
        );
        assert_eq!(sync.estimate(300).unwrap().sample_count, 1);
    }

    #[test]
    fn pending_window_is_bounded_and_expired_slots_are_reclaimed() {
        let mut sync = ClockSynchronizer::with_config(ClockSyncConfig {
            max_pending_probes: 2,
            ..Default::default()
        })
        .unwrap();
        sync.begin_probe(1, 100).unwrap();
        sync.begin_probe(2, 101).unwrap();
        assert_eq!(
            sync.begin_probe(3, 102),
            Err(ClockSyncError::PendingCapacity)
        );
        sync.begin_probe(3, 5_000_102).unwrap();
        assert_eq!(sync.pending_probe_count(), 1);
        assert_eq!(
            sync.complete_probe(1, 100, 200, 200, 5_000_103),
            Err(ClockSyncError::UnknownOrCompletedProbe)
        );
    }

    #[test]
    fn expired_response_cannot_be_used_again() {
        let mut sync = ClockSynchronizer::new();
        sync.begin_probe(7, 100).unwrap();
        assert_eq!(
            sync.complete_probe(7, 100, 200, 200, 5_000_101),
            Err(ClockSyncError::ExpiredProbe)
        );
        assert_eq!(sync.pending_probe_count(), 0);
        assert!(sync.estimate(5_000_101).is_none());
    }

    #[test]
    fn local_epoch_reversal_and_remote_causal_reversal_are_rejected() {
        let mut sync = ClockSynchronizer::new();
        sync.begin_probe(1, 100).unwrap();
        assert_eq!(
            sync.complete_probe(1, 100, 200, 200, 99),
            Err(ClockSyncError::LocalTimeReversed)
        );
        assert_eq!(
            sync.begin_probe(2, 99),
            Err(ClockSyncError::LocalTimeReversed)
        );
        sync.begin_probe(2, 100).unwrap();
        assert_eq!(
            sync.complete_probe(2, 100, 201, 200, 300),
            Err(ClockSyncError::InvalidTimestampOrder)
        );
        assert!(sync.estimate(300).is_none());
    }

    #[test]
    fn negative_adjusted_rtt_is_rejected_instead_of_clamped() {
        let mut sync = ClockSynchronizer::new();
        sync.begin_probe(1, 100).unwrap();
        assert_eq!(
            sync.complete_probe(1, 100, 200, 401, 300),
            Err(ClockSyncError::HostProcessingExceedsRoundTrip)
        );
        assert!(sync.estimate(300).is_none());
    }

    #[test]
    fn zero_and_large_round_trips_are_preserved_without_fake_limits() {
        let mut sync = ClockSynchronizer::new();
        let zero = exchange(&mut sync, 1, 1_000_000, 42, 0, 0, 0);
        assert_eq!(zero.network_rtt_us, 0);
        let large = exchange(&mut sync, 2, 2_000_000, 42, 1_000_000, 500_000, 1_000_000);
        assert_eq!(large.raw_total_rtt_us, 2_500_000);
        assert_eq!(large.network_rtt_us, 2_000_000);
        assert_eq!(
            sync.estimate(large.client_recv_us).unwrap().latest_sample,
            large
        );
    }

    #[test]
    fn signed_offset_overflow_is_rejected_and_large_local_epochs_are_supported() {
        let mut sync = ClockSynchronizer::new();
        sync.begin_probe(1, 0).unwrap();
        assert_eq!(
            sync.complete_probe(1, 0, u64::MAX, u64::MAX, 0),
            Err(ClockSyncError::OffsetOutOfRange)
        );
        sync.reset();
        let sample = exchange(&mut sync, 1, u64::MAX - 10_000, 100, 100, 100, 100);
        assert_eq!(sample.offset_us, 100.0);
        assert_eq!(sample.network_rtt_us, 200);
    }

    #[test]
    fn clocks_can_start_seconds_or_days_apart_in_either_direction() {
        for offset in [-86_400_000_000, -5_000_000, 5_000_000, 86_400_000_000] {
            let mut sync = ClockSynchronizer::new();
            let now = regular_fit(&mut sync, offset);
            let estimate = sync.estimate(now).unwrap();
            assert_eq!(estimate.state, ClockSyncState::Synchronized);
            assert!((estimate.offset_us - offset as f64).abs() < 0.001);
            assert_eq!(estimate.drift_ppm, 0.0);
        }
    }

    #[test]
    fn centered_fit_recovers_drift_at_epoch_sized_timestamps() {
        for drift_ppm in [-100_i64, 50, 100] {
            let mut sync = ClockSynchronizer::new();
            let base = 1_700_000_000_000_000;
            let offset = -5_000_000_i64;
            let host_time = |client: u64| -> u64 {
                (client as i128
                    + offset as i128
                    + (client - base) as i128 * drift_ppm as i128 / 1_000_000)
                    as u64
            };
            let mut now = base;
            for id in 0..8 {
                let t1 = base + id * 1_000_000;
                let t2_local = t1 + 10_000;
                let t3_local = t1 + 20_000;
                let t4 = t1 + 30_000;
                sync.begin_probe(id, t1).unwrap();
                sync.complete_probe(id, t1, host_time(t2_local), host_time(t3_local), t4)
                    .unwrap();
                now = t4;
            }
            let estimate = sync.estimate(now).unwrap();
            assert!(estimate.drift_estimated);
            assert!((estimate.drift_ppm - drift_ppm as f64).abs() < 0.001);
            let true_offset = host_time(now) as i128 - now as i128;
            assert!((estimate.offset_us - true_offset as f64).abs() <= 0.51);
        }
    }

    #[test]
    fn many_near_simultaneous_samples_do_not_claim_a_drift_fit() {
        let mut sync = ClockSynchronizer::new();
        for id in 0..12 {
            exchange(&mut sync, id, 1_000_000 + id * 100, 10, 10, 10, 10);
        }
        let estimate = sync.estimate(1_001_200).unwrap();
        assert_eq!(estimate.state, ClockSyncState::Acquiring);
        assert!(!estimate.drift_estimated);
        assert_eq!(estimate.drift_ppm, 0.0);
    }

    #[test]
    fn low_rtt_fit_excludes_queue_spikes_but_keeps_the_raw_latest_sample() {
        let mut sync = ClockSynchronizer::new();
        let last = regular_fit(&mut sync, 5_000_000);
        let spike = exchange(
            &mut sync,
            10,
            last + 1_000_000,
            5_000_000,
            300_000,
            10_000,
            1_000,
        );
        let estimate = sync.estimate(spike.client_recv_us).unwrap();
        assert_eq!(estimate.sample_count, 7);
        assert_eq!(estimate.selected_sample_count, 6);
        assert_eq!(estimate.offset_us, 5_000_000.0);
        assert_eq!(estimate.latest_sample.network_rtt_us, 301_000);
        assert_eq!(estimate.latest_sample.raw_total_rtt_us, 311_000);
        assert_eq!(estimate.sample_age_us, spike.client_recv_us - last);
    }

    #[test]
    fn fixed_asymmetry_remains_visible_even_when_fit_residuals_are_zero() {
        let mut sync = ClockSynchronizer::new();
        let true_offset = 5_000_000;
        let mut now = 0;
        for id in 0..6 {
            now = exchange(
                &mut sync,
                id,
                1_700_000_000_000_000 + id * 1_000_000,
                true_offset,
                9_000,
                100,
                1_000,
            )
            .client_recv_us;
        }
        let estimate = sync.estimate(now).unwrap();
        assert_eq!(estimate.offset_us, 5_004_000.0);
        assert_eq!(estimate.fit_ci95_us, Some(0.0));
        assert!(estimate.asymmetry_bound_us >= 5_000.0);
        assert!((estimate.offset_us - true_offset as f64).abs() <= estimate.asymmetry_bound_us);
        assert!(estimate.uncertainty_us().unwrap() > 5_000.0);
    }

    #[test]
    fn freshness_follows_selected_samples_and_holdover_uncertainty_grows() {
        let mut sync = ClockSynchronizer::new();
        let last = regular_fit(&mut sync, 50);
        let at_last = sync.estimate(last).unwrap();
        assert_eq!(at_last.state, ClockSyncState::Synchronized);
        let stale = sync.estimate(last + 10_000_001).unwrap();
        assert_eq!(stale.state, ClockSyncState::Stale);
        assert!(stale.holdover_bound_us > 1_000.0);
        assert!(stale.asymmetry_bound_us > at_last.asymmetry_bound_us);
        // An unselected queued response must not make the old fit look fresh.
        let spike = exchange(&mut sync, 10, last + 11_000_000, 50, 300_000, 0, 1_000);
        assert_eq!(
            sync.estimate(spike.client_recv_us).unwrap().state,
            ClockSyncState::Stale
        );
    }

    #[test]
    fn backward_query_is_stale_and_cannot_be_treated_as_fresh() {
        let mut sync = ClockSynchronizer::new();
        let now = regular_fit(&mut sync, 0);
        assert_eq!(sync.estimate(now - 1).unwrap().state, ClockSyncState::Stale);
    }

    #[test]
    fn history_is_bounded_by_both_count_and_age() {
        let mut sync = ClockSynchronizer::with_config(ClockSyncConfig {
            sample_capacity: 4,
            ..Default::default()
        })
        .unwrap();
        let last = regular_fit(&mut sync, 100);
        assert_eq!(sync.estimate(last).unwrap().sample_count, 4);
        let fresh = exchange(&mut sync, 10, last + 60_000_001, 200, 1_000, 100, 1_000);
        let estimate = sync.estimate(fresh.client_recv_us).unwrap();
        assert_eq!(estimate.sample_count, 1);
        assert_eq!(estimate.state, ClockSyncState::Acquiring);
        assert_eq!(estimate.offset_us, 200.0);
    }

    #[test]
    fn reset_removes_pending_samples_and_old_clock_domain() {
        let mut sync = ClockSynchronizer::new();
        let now = regular_fit(&mut sync, 100);
        sync.begin_probe(10, now + 100).unwrap();
        sync.reset();
        assert!(sync.estimate(0).is_none());
        assert_eq!(sync.pending_probe_count(), 0);
        assert_eq!(
            sync.complete_probe(10, now + 100, now + 200, now + 200, now + 300),
            Err(ClockSyncError::UnknownOrCompletedProbe)
        );
        assert!(sync.begin_probe(0, 0).is_ok());
    }

    #[test]
    fn cancelled_probe_id_cannot_be_reused() {
        let mut sync = ClockSynchronizer::new();
        sync.begin_probe(5, 100).unwrap();
        assert!(sync.cancel_probe(5));
        assert!(!sync.cancel_probe(5));
        assert_eq!(
            sync.begin_probe(5, 101),
            Err(ClockSyncError::ProbeIdNotIncreasing)
        );
        assert_eq!(
            sync.complete_probe(5, 100, 200, 200, 300),
            Err(ClockSyncError::UnknownOrCompletedProbe)
        );
    }

    #[test]
    fn responses_may_arrive_out_of_probe_order() {
        let mut sync = ClockSynchronizer::new();
        sync.begin_probe(1, 100).unwrap();
        sync.begin_probe(2, 110).unwrap();
        sync.complete_probe(2, 110, 120, 120, 130).unwrap();
        sync.complete_probe(1, 100, 120, 120, 140).unwrap();
        assert_eq!(sync.estimate(140).unwrap().sample_count, 2);
    }

    #[test]
    fn invalid_configs_do_not_create_unbounded_or_nan_estimators() {
        for config in [
            ClockSyncConfig {
                sample_capacity: 0,
                ..Default::default()
            },
            ClockSyncConfig {
                max_pending_probes: 0,
                ..Default::default()
            },
            ClockSyncConfig {
                min_fit_samples: 2,
                ..Default::default()
            },
            ClockSyncConfig {
                min_fit_span_us: 0,
                ..Default::default()
            },
            ClockSyncConfig {
                low_rtt_ratio: f64::NAN,
                ..Default::default()
            },
            ClockSyncConfig {
                low_rtt_ratio: -1.0,
                ..Default::default()
            },
            ClockSyncConfig {
                freshness_limit_us: u64::MAX,
                ..Default::default()
            },
            ClockSyncConfig {
                holdover_uncertainty_ppm: f64::INFINITY,
                ..Default::default()
            },
        ] {
            assert!(matches!(
                ClockSynchronizer::with_config(config),
                Err(ClockSyncError::InvalidConfig)
            ));
        }
    }

    #[test]
    fn continuity_tracks_local_progress_without_using_remote_time() {
        let mut guard = ClockContinuity::new();
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert!(guard.observe(1_000_000, wall));
        assert!(guard.observe(2_000_000, wall + Duration::from_secs(1)));
        // Normal paused cadence is continuous.
        assert!(guard.observe(7_000_000, wall + Duration::from_secs(6)));
        assert!(guard.observe(8_000_000, wall + Duration::from_millis(7_900)));
    }

    #[test]
    fn continuity_rejects_wall_steps_backwards_ticks_and_suspend() {
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for (mono, elapsed) in [
            (2_000_000, Duration::from_secs(3)),
            (13_000_000, Duration::from_secs(12)),
            (1_000_000, Duration::from_secs(60)),
        ] {
            let mut guard = ClockContinuity::new();
            assert!(guard.observe(1_000_000, wall));
            assert!(!guard.observe(mono, wall + elapsed));
            assert!(guard.observe(mono + 1_000_000, wall + elapsed + Duration::from_secs(1)));
        }
        let mut guard = ClockContinuity::new();
        guard.observe(1_000_000, wall);
        assert!(!guard.observe(2_000_000, wall - Duration::from_secs(1)));
        assert!(!guard.observe(1_000_000, wall));
        guard.reset();
        assert!(guard.observe(0, wall));
    }

    #[test]
    fn same_session_invalidation_preserves_probe_id_high_water_mark() {
        let mut sync = ClockSynchronizer::new();
        let now = regular_fit(&mut sync, 100);
        sync.begin_probe(10, now + 100).unwrap();
        sync.invalidate_measurements();
        assert!(sync.estimate(0).is_none());
        assert_eq!(sync.pending_probe_count(), 0);
        assert_eq!(
            sync.begin_probe(10, 0),
            Err(ClockSyncError::ProbeIdNotIncreasing)
        );
        assert_eq!(
            sync.complete_probe(10, now + 100, now + 200, now + 200, now + 300),
            Err(ClockSyncError::UnknownOrCompletedProbe)
        );
        sync.begin_probe(11, 0).unwrap();
    }

    #[test]
    fn remote_offset_step_reacquires_and_retires_all_old_pending_probes() {
        let mut sync = ClockSynchronizer::new();
        let last = regular_fit(&mut sync, 100);
        let t1 = last + 1_000_000;
        sync.begin_probe(10, t1).unwrap();
        sync.begin_probe(11, t1 + 100).unwrap();
        let new_offset = 5_000_100;
        let sample = sync
            .complete_probe(
                11,
                t1 + 100,
                t1 + 100 + new_offset + 1_000,
                t1 + 100 + new_offset + 1_100,
                t1 + 2_200,
            )
            .unwrap();
        let estimate = sync.estimate(sample.client_recv_us).unwrap();
        assert_eq!(estimate.state, ClockSyncState::Acquiring);
        assert_eq!(estimate.sample_count, 1);
        assert_eq!(estimate.offset_us, new_offset as f64);
        assert_eq!(sync.pending_probe_count(), 0);
        assert_eq!(
            sync.complete_probe(10, t1, t1 + 1_100, t1 + 1_200, t1 + 2_300),
            Err(ClockSyncError::UnknownOrCompletedProbe)
        );
        assert_eq!(
            sync.begin_probe(11, t1 + 3_000),
            Err(ClockSyncError::ProbeIdNotIncreasing)
        );
        assert!(sync.begin_probe(12, t1 + 3_000).is_ok());
    }

    #[test]
    fn ordinary_rtt_asymmetry_within_error_allowance_does_not_reset_fit() {
        let mut sync = ClockSynchronizer::new();
        let last = regular_fit(&mut sync, 100);
        let sample = exchange(&mut sync, 10, last + 1_000_000, 100, 3_000, 100, 1_000);
        let estimate = sync.estimate(sample.client_recv_us).unwrap();
        assert_eq!(estimate.sample_count, 7);
        assert_eq!(estimate.selected_sample_count, 6);
        assert_eq!(estimate.state, ClockSyncState::Synchronized);
    }
}
