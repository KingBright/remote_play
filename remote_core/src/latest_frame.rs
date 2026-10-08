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

#[cfg(test)]
mod lifecycle_regression_tests {
    use super::*;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    #[test]
    fn contended_slot_never_blocks_the_producer_or_replaces_the_held_frame() {
        let slot = Arc::new(LatestFrameSlot::new());
        slot.push(7);
        let lock = slot.slot.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_slot = slot.clone();
        let worker = std::thread::spawn(move || {
            worker_slot.push(8);
            tx.send(()).unwrap();
        });
        // A liveness deadline, not a speed benchmark. Always unlock before joining,
        // so a future blocking implementation produces a failure, not a hung suite.
        let completed_without_unlock = rx.recv_timeout(Duration::from_secs(1)).is_ok();
        drop(lock);
        worker.join().unwrap();
        assert!(
            completed_without_unlock,
            "producer waited for the UI consumer"
        );
        assert_eq!(
            slot.take(),
            Some(7),
            "contention must discard the incoming frame"
        );
    }

    #[test]
    fn a_poisoned_consumer_lock_does_not_permanently_stop_frames() {
        let slot = Arc::new(LatestFrameSlot::new());
        slot.push(1);
        let worker_slot = slot.clone();
        let worker = std::thread::spawn(move || {
            let _lock = worker_slot.slot.lock().unwrap();
            panic!("intentional test-only consumer failure");
        });
        assert!(worker.join().is_err());
        slot.push(2);
        assert_eq!(slot.take(), Some(2));
        slot.push(3);
        assert_eq!(slot.take(), Some(3));
    }

    #[test]
    fn ten_thousand_replacements_retain_only_one_frame() {
        struct CountedFrame {
            index: usize,
            alive: Arc<AtomicUsize>,
        }
        impl Drop for CountedFrame {
            fn drop(&mut self) {
                self.alive.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let alive = Arc::new(AtomicUsize::new(0));
        let slot = LatestFrameSlot::new();
        for index in 0..10_000 {
            alive.fetch_add(1, Ordering::SeqCst);
            slot.push(CountedFrame {
                index,
                alive: alive.clone(),
            });
            assert_eq!(
                alive.load(Ordering::SeqCst),
                1,
                "producer built an old-frame backlog"
            );
        }
        let latest = slot.take().unwrap();
        assert_eq!(latest.index, 9_999);
        drop(latest);
        assert_eq!(alive.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancelled_consumer_does_not_lose_a_later_frame() {
        let slot = LatestFrameSlot::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), slot.take_async())
                .await
                .is_err()
        );
        slot.push(42);
        let next = tokio::time::timeout(Duration::from_secs(1), slot.take_async())
            .await
            .unwrap();
        assert_eq!(next, 42);
        assert_eq!(slot.take(), None);
    }
}
