use std::sync::{Mutex, MutexGuard, TryLockError};
use tokio::sync::Notify;

/// Latest-wins frame slot that never blocks the producer.
///
/// Capture callbacks must not wait on the consumer. `push` uses `try_lock` and
/// replaces the previous frame, or drops the incoming frame on contention.
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
        let mut guard = match self.slot.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(err)) => err.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        let previous = guard.replace(value);
        drop(guard);
        // Keep a permit if the consumer is between checking the slot and waiting.
        self.notify.notify_one();
        // Releasing a platform frame can be expensive; never hold the slot for it.
        drop(previous);
    }

    pub fn take(&self) -> Option<T> {
        self.lock().take()
    }

    pub async fn take_async(&self) -> T {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(value) = self.take() {
                return value;
            }
            notified.await;
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

    #[tokio::test]
    async fn notification_survives_push_before_wait_registration() {
        let slot = LatestFrameSlot::new();
        assert_eq!(slot.take(), None);
        slot.push(42);
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            slot.notify.notified(),
        )
        .await
        .expect("producer must retain a wakeup permit");
        assert_eq!(slot.take_async().await, 42);
    }

    #[test]
    fn replaced_frame_is_released_outside_the_lock() {
        struct Frame(std::sync::Weak<LatestFrameSlot<Frame>>);
        impl Drop for Frame {
            fn drop(&mut self) {
                let slot = self.0.upgrade().expect("slot is alive");
                assert!(slot.slot.try_lock().is_ok());
            }
        }
        let slot = std::sync::Arc::new(LatestFrameSlot::new());
        slot.push(Frame(std::sync::Arc::downgrade(&slot)));
        slot.push(Frame(std::sync::Arc::downgrade(&slot)));
        drop(slot.take());
    }
}
