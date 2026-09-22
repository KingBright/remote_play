/// Select frames on a target timeline instead of rounding every interval up to
/// a whole display refresh. This permits e.g. 37 FPS from a 60 Hz capture source.
pub struct FramePacer {
    period_us: u64,
    next_due_us: Option<u64>,
}

impl FramePacer {
    pub fn new(fps: u32) -> Self {
        Self {
            period_us: (1_000_000 / u64::from(fps.max(1))).max(1),
            next_due_us: None,
        }
    }

    pub fn reset(&mut self, fps: u32) {
        *self = Self::new(fps);
    }

    pub fn admit(&mut self, timestamp_us: u64) -> bool {
        let due = self.next_due_us.unwrap_or(timestamp_us);
        let latest = timestamp_us.saturating_add(self.period_us / 2);
        if latest < due {
            return false;
        }
        // Select the nearest available capture frame. Advance the timeline past
        // late frames rather than producing a catch-up burst after a stall.
        let periods = (latest - due) / self.period_us + 1;
        self.next_due_us = Some(due.saturating_add(periods.saturating_mul(self.period_us)));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arbitrary_rates_do_not_round_down_to_display_divisors() {
        for fps in [1, 7, 24, 29, 37, 47, 59, 60, 120] {
            let mut pacer = FramePacer::new(fps);
            let emitted = (0..600).filter(|n| pacer.admit(n * 1_000_000 / 60)).count();
            let expected = fps.min(60) as usize * 10;
            assert!(
                emitted.abs_diff(expected) <= 1,
                "{fps}: {emitted}, expected {expected}"
            );
        }
    }
    #[test]
    fn gaps_and_rate_changes_do_not_create_a_catch_up_burst() {
        let mut pacer = FramePacer::new(30);
        assert!(pacer.admit(0));
        assert!(pacer.admit(20_000_000));
        assert!(!pacer.admit(20_000_001));
        pacer.reset(37);
        assert!(pacer.admit(20_000_002));
    }
}
