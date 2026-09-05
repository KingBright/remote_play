use std::sync::{Mutex, MutexGuard};
use tokio::sync::Notify;

/// Latest-wins frame slot that never blocks the producer.
///
/// Capture callbacks must not wait on the consumer. `push` uses `try_lock` and
/// drops the previous frame if the consumer currently holds the slot.
pub struct LatestFrameSlot<T> {
    slot: Mutex<Option<T>>,
    notify: Notify,
}

impl<T> LatestFrameSlot<T> {
    pub fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    pub fn push(&self, value: T) {
        match self.slot.try_lock() {
            Ok(mut guard) => *guard = Some(value),
            Err(_) => {
                // Consumer is reading; drop this frame rather than stall capture.
            }
        }
        self.notify.notify_waiters();
    }

    pub fn take(&self) -> Option<T> {
        self.lock().take()
    }

    pub async fn take_async(&self) -> T {
        loop {
            if let Some(value) = self.take() {
                return value;
            }
            self.notify.notified().await;
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<T>> {
        self.slot.lock().unwrap_or_else(|err| err.into_inner())
    }
}

impl<T> Default for LatestFrameSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_wins_and_never_queues() {
        let slot = LatestFrameSlot::new();
        slot.push(1);
        slot.push(2);
        assert_eq!(slot.take(), Some(2));
        assert_eq!(slot.take(), None);
    }
}
