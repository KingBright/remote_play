//! Scheduling boundary for synchronous native codec calls, not a new decoder.
//! Production uses a multi-thread Tokio runtime: let queued network/timer work
//! move to another worker while the current OS thread remains inside native code.
//! Borrowed packet storage, callback context, and native frames stay on this thread.
//! No additional media queue, frame copy, thread-per-frame or codec flag change.
//!
//! This does not cancel a driver call or promise a deadline inside foreign code.
//! Outside a multi-thread runtime retain the synchronous caller contract; the
//! product GUI separately rejects a current-thread runtime before starting it.
use tokio::runtime::{Handle, RuntimeFlavor};

pub(crate) fn run<R>(call: impl FnOnce() -> R) -> R {
    if Handle::try_current().is_ok_and(|h| h.runtime_flavor() == RuntimeFlavor::MultiThread) {
        tokio::task::block_in_place(call)
    } else {
        call()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn slow_native_initialization_does_not_starve_network_or_timers() {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (answer_tx, answer_rx) = std::sync::mpsc::channel();
        let worker = tokio::spawn(async move {
            run(|| {
                ready_tx.send(()).unwrap();
                // A response depends on an async timer and simulated network IO
                // on this runtime, even though only one async worker is configured.
                answer_rx.recv_timeout(Duration::from_secs(2))
            })
        });
        ready_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        answer_tx.send(42u32).unwrap();
        assert_eq!(worker.await.unwrap().unwrap(), 42);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn submission_keeps_caller_thread_and_borrowed_buffer() {
        tokio::spawn(async {
            let thread = std::thread::current().id();
            let mut data = [7u8; 32];
            let pointer = data.as_ptr();
            run(|| {
                assert_eq!(std::thread::current().id(), thread);
                assert_eq!(data.as_ptr(), pointer);
                data[0] = 9;
            });
            assert_eq!(data[0], 9);
        })
        .await
        .unwrap();
    }
    #[test]
    fn synchronous_caller_still_works_without_a_runtime() {
        assert_eq!(run(|| 19), 19);
    }
    #[tokio::test]
    async fn current_thread_caller_is_not_panicked_or_moved() {
        let thread = std::thread::current().id();
        assert_eq!(run(|| std::thread::current().id()), thread);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn native_release_does_not_starve_async_progress() {
        struct NativeResource(std::sync::mpsc::Receiver<()>);
        impl Drop for NativeResource {
            fn drop(&mut self) {
                run(|| self.0.recv_timeout(Duration::from_secs(2))).unwrap();
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (ready, started) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let resource = NativeResource(rx);
            ready.send(()).unwrap();
            drop(resource);
        });
        started.await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        tx.send(()).unwrap();
        worker.await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn error_value_and_cleanup_order_are_preserved() {
        let result: Result<(), &'static str> =
            tokio::spawn(async { run(|| Err("native failure")) })
                .await
                .unwrap();
        assert_eq!(result, Err("native failure"));
    }
}
