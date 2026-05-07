use quanta::{Clock, Instant};
use std::time::Duration;

pub struct TimeBox {
    clock: Clock,
}

impl TimeBox {
    pub fn new() -> Self {
        Self {
            clock: Clock::new(),
        }
    }

    pub fn now(&self) -> Instant {
        self.clock.now()
    }

    /// High precision sleep loop if needed for microsecond level pacing.
    /// In a real scenario, consider using `spin_sleep` crate.
    pub fn spin_sleep(&self, duration: Duration) {
        let start = self.clock.now();
        while self.clock.now() - start < duration {
            std::hint::spin_loop();
        }
    }
}
