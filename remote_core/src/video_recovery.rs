//! Local video generations shared by ingress, decoding and presentation.
//! These tokens never enter the wire protocol. A GUI polling late must observe
//! the boundary that actually invalidated the picture, not invent a newer one.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoEpoch {
    pub source: u64,
    pub recovery: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct VideoRecoverySnapshot {
    pub epoch: VideoEpoch,
    pub began_at: Instant,
    pub waiting: bool,
}

pub struct VideoRecovery {
    source: Arc<AtomicU64>,
    tracks_output: AtomicBool,
    state: Mutex<VideoRecoverySnapshot>,
}

impl Default for VideoRecovery {
    fn default() -> Self {
        Self {
            source: Arc::new(AtomicU64::new(0)),
            tracks_output: AtomicBool::new(false),
            state: Mutex::new(VideoRecoverySnapshot {
                epoch: VideoEpoch::default(),
                began_at: Instant::now(),
                waiting: false,
            }),
        }
    }
}

impl VideoRecovery {
    /// Desktop decoders report actual output. Encoded-only consumers (such as
    /// the mobile bridge) retain ingress recovery without claiming a decode.
    pub fn track_output(&self) {
        self.tracks_output.store(true, Ordering::Release);
    }

    pub fn tracks_output(&self) -> bool {
        self.tracks_output.load(Ordering::Acquire)
    }

    pub fn source_counter(&self) -> Arc<AtomicU64> {
        self.source.clone()
    }

    pub fn snapshot(&self) -> VideoRecoverySnapshot {
        *self.state.lock().unwrap()
    }

    pub fn is_waiting(&self) -> bool {
        self.snapshot().waiting
    }

    /// Establish a source/payload boundary before reopening ingress. All frame
    /// publication takes this lock before the latest-frame lock. Retire native
    /// resources returned by `clear` only after this function releases its lock.
    pub fn reset_source<T>(&self, clear: impl FnOnce() -> T) -> T {
        let mut state = self.state.lock().unwrap();
        state.epoch.source = state.epoch.source.wrapping_add(1).max(1);
        state.epoch.recovery = state.epoch.recovery.wrapping_add(1);
        state.began_at = Instant::now();
        state.waiting = true;
        self.source.store(state.epoch.source, Ordering::Release);
        clear()
    }

    /// Invalidate a decoder attempt, even when another recovery is pending.
    /// Repeated failed attempts keep the original presentation boundary, but
    /// cannot publish a late output or clear a newer attempt's waiting state.
    pub fn require_keyframe(&self, source: u64) -> Option<VideoEpoch> {
        let mut state = self.state.lock().unwrap();
        if source != state.epoch.source {
            return None;
        }
        state.epoch.recovery = state.epoch.recovery.wrapping_add(1);
        if !state.waiting {
            state.began_at = Instant::now();
        }
        state.waiting = true;
        Some(state.epoch)
    }

    /// An asynchronous error belongs to its original attempt, too.
    pub fn fail(&self, epoch: VideoEpoch, report: impl FnOnce()) -> Option<VideoEpoch> {
        let mut state = self.state.lock().unwrap();
        if epoch != state.epoch {
            return None;
        }
        state.epoch.recovery = state.epoch.recovery.wrapping_add(1);
        if !state.waiting {
            state.began_at = Instant::now();
        }
        state.waiting = true;
        report();
        Some(state.epoch)
    }

    pub fn with_current<T>(&self, epoch: VideoEpoch, action: impl FnOnce() -> T) -> Option<T> {
        let state = self.state.lock().unwrap();
        (epoch == state.epoch).then(action)
    }

    /// Validate and publish atomically with respect to every reset. Receiving
    /// an IDR alone never calls this; only an actual decoded output can recover.
    pub fn publish<T>(&self, epoch: VideoEpoch, output: impl FnOnce() -> T) -> Option<T> {
        let mut state = self.state.lock().unwrap();
        if epoch != state.epoch {
            return None;
        }
        let output = output();
        state.waiting = false;
        Some(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_observation_retains_the_real_recovery_boundary() {
        let recovery = VideoRecovery::default();
        recovery.reset_source(|| ());
        let accepted = recovery.snapshot();
        let decoded_at = Instant::now();
        assert_eq!(recovery.publish(accepted.epoch, || 7), Some(7));
        let observed = recovery.snapshot();
        assert_eq!(observed.began_at, accepted.began_at);
        assert!(decoded_at >= observed.began_at);
        assert!(!observed.waiting);
    }

    #[test]
    fn stale_source_and_stale_attempt_cannot_publish_or_clear_waiting() {
        let recovery = VideoRecovery::default();
        recovery.reset_source(|| ());
        let old = recovery.snapshot();
        recovery.reset_source(|| ());
        assert!(
            recovery
                .publish(old.epoch, || panic!("old source published"))
                .is_none()
        );
        assert!(recovery.require_keyframe(old.epoch.source).is_none());
        let first = recovery.snapshot();
        let retry = recovery.require_keyframe(first.epoch.source).unwrap();
        assert_eq!(recovery.snapshot().began_at, first.began_at);
        assert!(
            recovery
                .publish(first.epoch, || panic!("old attempt published"))
                .is_none()
        );
        assert!(recovery.is_waiting());
        assert_eq!(recovery.publish(retry, || 9), Some(9));
        assert!(!recovery.is_waiting());
    }

    #[test]
    fn failed_attempt_keeps_its_boundary_and_late_errors_cannot_poison_recovery() {
        let recovery = VideoRecovery::default();
        recovery.reset_source(|| ());
        let first = recovery.snapshot();
        let retry = recovery.fail(first.epoch, || ()).unwrap();
        assert_eq!(recovery.snapshot().began_at, first.began_at);
        assert_ne!(retry, first.epoch);
        assert!(recovery.publish(retry, || ()).is_some());
        assert!(
            recovery
                .fail(first.epoch, || panic!("stale error reported"))
                .is_none()
        );
        assert!(
            recovery
                .with_current(first.epoch, || panic!("stale diagnostic reported"))
                .is_none()
        );
        assert!(!recovery.is_waiting());
        assert_eq!(recovery.snapshot().epoch, retry);

        let decoded_at = Instant::now();
        let next = recovery.require_keyframe(retry.source).unwrap();
        assert_ne!(next, retry);
        assert!(recovery.snapshot().began_at >= decoded_at);
        assert!(recovery.is_waiting());
    }
}
