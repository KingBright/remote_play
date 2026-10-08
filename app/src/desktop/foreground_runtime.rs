//! Run a native GUI event loop on its required calling thread, outside a Tokio
//! task's cooperative polling budget. Network/media tasks stay on Tokio workers.
//!
//! Calling a synchronous Application::run directly inside #[tokio::main] nests
//! GPUI's executor inside one never-ending Tokio poll. Tokio channels/select can
//! exhaust that outer budget and then return Pending forever to the wrong
//! executor. block_in_place releases that budget without moving the OS UI thread.
//! It is not used to make CPU-heavy work run on the UI thread.
use tokio::runtime::{Handle, RuntimeFlavor};

pub(crate) fn run<R>(event_loop: impl FnOnce() -> R) -> Result<R, &'static str> {
    let handle = Handle::try_current()
        .map_err(|_| "Start the native GUI from the application's multi-thread Tokio runtime")?;
    if handle.runtime_flavor() != RuntimeFlavor::MultiThread {
        return Err(
            "The native GUI requires the multi-thread runtime so network and media keep running independently",
        );
    }
    Ok(tokio::task::block_in_place(event_loop))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_loop_does_not_inherit_exhausted_tokio_budget() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for value in 0..2048u64 {
            tx.send(value).unwrap();
        }
        drop(tx);
        let mut future = Box::pin(async move {
            let mut count = 0;
            while rx.recv().await.is_some() {
                count += 1;
            }
            count
        });
        let mut context = Context::from_waker(Waker::noop());
        // Reproduce the legacy nested-executor boundary without creating a window.
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        // The very same blocked future must progress when the OS event loop owns
        // the thread rather than the outer Tokio task budget.
        let count = run(|| match future.as_mut().poll(&mut context) {
            Poll::Ready(count) => count,
            Poll::Pending => panic!("GUI loop inherited an exhausted cooperative budget"),
        })
        .unwrap();
        assert_eq!(count, 2048);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ui_loop_stays_on_the_calling_thread() {
        let thread = std::thread::current().id();
        assert_eq!(run(|| std::thread::current().id()).unwrap(), thread);
    }
    #[tokio::test]
    async fn single_thread_runtime_is_rejected_before_creating_gui() {
        let mut called = false;
        assert!(run(|| called = true).is_err());
        assert!(!called);
    }
}
