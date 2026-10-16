use crate::linux_frame::{FrameMailbox, OwnedFrame, PixelFormat};
use async_trait::async_trait;
use remote_core::{VideoCapturer, VideoFrame, VideoFrameHandleKind, VideoPixelFormat};
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

pub struct LinuxVideoFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub capture_time_ms: u32,
    pub is_keyframe: bool,
    pub timing: protocol::FrameTimingCheckpoints,
    pub(crate) owned: Option<OwnedFrame>,
}

impl LinuxVideoFrame {
    pub(crate) fn from_owned(frame: OwnedFrame) -> Self {
        Self {
            width: frame.width,
            height: frame.height,
            data: Vec::new(),
            capture_time_ms: (frame.stamp.arrival_ts_us / 1000) as u32,
            is_keyframe: false,
            timing: protocol::FrameTimingCheckpoints::new(frame.stamp.arrival_ts_us),
            owned: Some(frame),
        }
    }
}

impl VideoFrame for LinuxVideoFrame {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn handle_kind(&self) -> VideoFrameHandleKind {
        VideoFrameHandleKind::CpuMemory
    }
    fn pixel_format(&self) -> VideoPixelFormat {
        match self
            .owned
            .as_ref()
            .map(|frame| frame.source_format.pixel_format)
        {
            Some(PixelFormat::Bgra | PixelFormat::Bgrx) => VideoPixelFormat::Bgra8,
            Some(PixelFormat::Rgba | PixelFormat::Rgbx) => VideoPixelFormat::Rgba8,
            Some(PixelFormat::Nv12) => VideoPixelFormat::Nv12,
            _ => VideoPixelFormat::Unknown,
        }
    }
}

pub struct LinuxVideoCapturer {
    target_width: u32,
    target_height: u32,
    target_fps: u32,
    running: Arc<AtomicBool>,
    frame_rx: mpsc::Receiver<LinuxVideoFrame>,
    run: Option<CaptureRun>,
    native: Option<Arc<FrameMailbox>>,
}

/// Owns cancellation even when stop/start is itself cancelled while awaiting
/// teardown. Native mailbox ownership remains with the selected-source lease.
struct CaptureRun {
    cancelled: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl CaptureRun {
    fn cancel(&self) {
        self.cancelled.send_replace(true);
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl Drop for CaptureRun {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct ClearRunningOnExit(Arc<AtomicBool>);

impl Drop for ClearRunningOnExit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

async fn capture_cancelled(cancelled: &mut watch::Receiver<bool>) {
    loop {
        if *cancelled.borrow_and_update() {
            return;
        }
        if cancelled.changed().await.is_err() {
            return;
        }
    }
}

async fn send_frame(
    tx: &mpsc::Sender<LinuxVideoFrame>,
    cancelled: &mut watch::Receiver<bool>,
    frame: LinuxVideoFrame,
) -> bool {
    tokio::select! {
        biased;
        _ = capture_cancelled(cancelled) => false,
        sent = tx.send(frame) => sent.is_ok(),
    }
}

async fn receive_native_frame(
    mailbox: &FrameMailbox,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<LinuxVideoFrame, Box<dyn Error + Send + Sync>> {
    tokio::select! {
        biased;
        _ = capture_cancelled(cancelled) => Err("Linux capture stopped".into()),
        frame = mailbox.receive() => Ok(LinuxVideoFrame::from_owned(frame?)),
    }
}

impl LinuxVideoCapturer {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let (frame_tx, frame_rx) = mpsc::channel(4);
        // An idle capturer must not retain a sender that could keep recv alive.
        drop(frame_tx);
        Ok(Self {
            target_width: width,
            target_height: height,
            target_fps: fps.clamp(15, 120),
            running: Arc::new(AtomicBool::new(false)),
            frame_rx,
            run: None,
            native: None,
        })
    }

    pub(crate) fn from_owned_mailbox(
        mailbox: Arc<FrameMailbox>,
        fps: u32,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut capture = Self::new(0, 0, fps)?;
        capture.native = Some(mailbox);
        Ok(capture)
    }

    pub fn update_resolution_and_fps(
        &mut self,
        height: u32,
        fps: u32,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.target_height = height;
        self.target_fps = fps.clamp(15, 120);
        Ok(())
    }

    async fn retire_run(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(run) = &mut self.run {
            run.cancel();
            // Keep the handle in self across await: cancellation of stop must
            // not detach it or permit a new generation to race its final Drop.
            if let Some(task) = &mut run.task {
                let _ = task.await;
            }
        }
        self.run = None;
        self.frame_rx.close();
        while self.frame_rx.try_recv().is_ok() {}
    }
}

impl Drop for LinuxVideoCapturer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(run) = &self.run {
            run.cancel();
        }
        self.frame_rx.close();
        // Do not close a shared native mailbox here. A replacement stream may
        // reuse its selected-source lease after the old stream task is aborted.
    }
}

#[async_trait]
impl VideoCapturer for LinuxVideoCapturer {
    type Frame = LinuxVideoFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if let Some(mailbox) = &self.native {
            if mailbox.is_closed() {
                return Err("Linux native capture mailbox is closed".into());
            }
            mailbox.set_paused(false);
        }
        if self.running.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.retire_run().await;
        let (cancelled, mut cancel_rx) = watch::channel(false);
        self.running.store(true, Ordering::SeqCst);
        let mut run = CaptureRun {
            cancelled,
            task: None,
        };
        if self.native.is_some() {
            self.run = Some(run);
            return Ok(());
        }
        // A new queue and cancellation epoch isolate every restart. Old
        // senders can never deliver frames into the replacement receiver.
        let (tx, rx) = mpsc::channel(4);
        self.frame_rx = rx;
        let width = self.target_width;
        let height = self.target_height;
        let fps = self.target_fps;

        let clear_running = ClearRunningOnExit(self.running.clone());

        // This path only makes placeholder frames. It performs no blocking
        // native capture and must never occupy Tokio's blocking pool: aborting
        // a spawn_blocking task cannot stop it once it has started.
        run.task = Some(tokio::spawn(async move {
            let _clear_running = clear_running;
            let interval = Duration::from_micros((1_000_000 / fps.max(1)) as u64);
            let mut ticks =
                tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut frame_count: u64 = 0;

            println!("[LinuxCapture] Capture loop started: {width}x{height} @ {fps}fps");

            loop {
                tokio::select! {
                    biased;
                    _ = capture_cancelled(&mut cancel_rx) => break,
                    _ = tx.closed() => break,
                    _ = ticks.tick() => {},
                }

                let now_ms = (SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    & 0xFFFFFFFF) as u32;

                if frame_count == 0 {
                    eprintln!(
                        "[LinuxCapture] PLACEHOLDER: not capturing PipeWire/X11 pixels; emitting empty frames."
                    );
                }
                let data = Vec::new();
                let is_key = frame_count % (fps as u64 * 2) == 0;
                frame_count += 1;

                let capture_ts_us = remote_core::timing::quanta_now_us();
                let frame = LinuxVideoFrame {
                    width,
                    height,
                    data,
                    capture_time_ms: now_ms,
                    is_keyframe: is_key,
                    timing: protocol::FrameTimingCheckpoints::new(capture_ts_us),
                    owned: None,
                };

                if !send_frame(&tx, &mut cancel_rx, frame).await {
                    break;
                }
            }
            println!("[LinuxCapture] Capture loop stopped.");
        }));
        self.run = Some(run);

        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.retire_run().await;
        if let Some(mailbox) = &self.native {
            // Preserve explicit stop's terminal native-generation semantics.
            // Use pause/resume to keep the selected source reusable.
            mailbox.close();
        }
        Ok(())
    }

    async fn pause(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if let Some(mailbox) = &self.native {
            mailbox.set_paused(true);
            Ok(())
        } else {
            self.stop().await
        }
    }
    async fn resume(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.start().await
    }

    async fn capture_frame(&mut self) -> Result<Self::Frame, Box<dyn Error + Send + Sync>> {
        let Some(run) = &self.run else {
            return Err("Linux capture is not started".into());
        };
        let mut cancelled = run.cancelled.subscribe();
        if let Some(mailbox) = &self.native {
            return receive_native_frame(mailbox, &mut cancelled).await;
        }
        // The trait already requires &mut self. No receiver lock is needed,
        // and teardown never joins a task while holding one.
        tokio::select! {
            biased;
            _ = capture_cancelled(&mut cancelled) => Err("Linux capture stopped".into()),
            frame = self.frame_rx.recv() => frame.ok_or_else(|| "Linux capture channel closed".into()),
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::future::{Future, poll_fn};
    use std::pin::Pin;
    use std::task::Poll;

    const TEST_TIMEOUT: Duration = Duration::from_secs(1);

    async fn assert_pending<F: Future>(mut future: Pin<&mut F>) {
        poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    async fn task_finished(task: &tokio::task::AbortHandle) {
        tokio::time::timeout(TEST_TIMEOUT, async {
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned capture task must finish without blocking runtime shutdown");
    }

    fn frame() -> LinuxVideoFrame {
        LinuxVideoFrame {
            width: 2,
            height: 2,
            data: Vec::new(),
            capture_time_ms: 1,
            is_keyframe: false,
            timing: protocol::FrameTimingCheckpoints::new(1_000),
            owned: None,
        }
    }

    async fn queue_full(capture: &LinuxVideoCapturer) {
        tokio::time::timeout(TEST_TIMEOUT, async {
            while capture.frame_rx.len() != capture.frame_rx.max_capacity() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("synthetic placeholder queue should fill");
    }

    /// This cleanup is deliberately independent of LinuxVideoCapturer::Drop:
    /// the regression must be safe to run against the leaking implementation.
    /// Closing its receiver releases any blocking_send; clearing the old flag
    /// then bounds the worker's remaining work to one existing frame interval.
    struct CaptureCleanup {
        capture: Option<LinuxVideoCapturer>,
        running: Arc<AtomicBool>,
    }

    impl CaptureCleanup {
        fn new() -> Self {
            let capture = LinuxVideoCapturer::new(2, 2, 120).unwrap();
            Self {
                running: capture.running.clone(),
                capture: Some(capture),
            }
        }
    }

    impl Drop for CaptureCleanup {
        fn drop(&mut self) {
            self.running.store(false, Ordering::SeqCst);
            drop(self.capture.take());
        }
    }

    #[tokio::test]
    async fn drop_capturer_cancels_owned_run() {
        let mut cleanup = CaptureCleanup::new();
        cleanup.capture.as_mut().unwrap().start().await.unwrap();
        assert!(cleanup.running.load(Ordering::SeqCst));
        drop(cleanup.capture.take());
        assert!(
            !cleanup.running.load(Ordering::SeqCst),
            "dropping capture must stop its worker even without an explicit stop"
        );
    }

    #[tokio::test]
    async fn full_queue_send_wakes_on_cancellation_without_draining() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(frame()).ok().unwrap();
        let (cancelled, mut cancel_rx) = watch::channel(false);
        let run = CaptureRun {
            cancelled,
            task: None,
        };
        let send = send_frame(&tx, &mut cancel_rx, frame());
        tokio::pin!(send);
        assert_pending(send.as_mut()).await;
        run.cancel();
        assert!(!tokio::time::timeout(TEST_TIMEOUT, send).await.unwrap());
        assert_eq!(
            rx.len(),
            1,
            "cancellation must not require draining the queue"
        );
        rx.close();
    }

    #[tokio::test]
    async fn full_queue_send_wakes_when_receiver_closes() {
        let (tx, rx) = mpsc::channel(1);
        tx.try_send(frame()).ok().unwrap();
        let (_cancelled, mut cancel_rx) = watch::channel(false);
        let send = send_frame(&tx, &mut cancel_rx, frame());
        tokio::pin!(send);
        assert_pending(send.as_mut()).await;
        drop(rx);
        assert!(!tokio::time::timeout(TEST_TIMEOUT, send).await.unwrap());
    }

    #[tokio::test]
    async fn drop_full_queue_cancels_and_finishes_owned_task() {
        let mut capture = LinuxVideoCapturer::new(2, 2, 120).unwrap();
        capture.start().await.unwrap();
        let task = capture
            .run
            .as_ref()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .abort_handle();
        queue_full(&capture).await;
        drop(capture);
        task_finished(&task).await;
    }

    #[tokio::test]
    async fn stop_full_queue_joins_worker_and_clears_queued_frames() {
        let mut capture = LinuxVideoCapturer::new(2, 2, 120).unwrap();
        capture.start().await.unwrap();
        let task = capture
            .run
            .as_ref()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .abort_handle();
        queue_full(&capture).await;
        tokio::time::timeout(TEST_TIMEOUT, capture.stop())
            .await
            .unwrap()
            .unwrap();
        assert!(task.is_finished());
        assert!(!capture.running.load(Ordering::SeqCst));
        assert!(capture.run.is_none());
        assert_eq!(capture.frame_rx.len(), 0);
        assert!(
            tokio::time::timeout(TEST_TIMEOUT, capture.capture_frame())
                .await
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn receiver_close_finishes_worker_without_explicit_stop() {
        let mut capture = LinuxVideoCapturer::new(2, 2, 120).unwrap();
        capture.start().await.unwrap();
        let task = capture
            .run
            .as_ref()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .abort_handle();
        capture.frame_rx.close();
        task_finished(&task).await;
        assert!(!capture.running.load(Ordering::SeqCst));
        assert!(capture.capture_frame().await.is_err());
        capture.stop().await.unwrap();
    }

    #[tokio::test]
    async fn repeated_start_is_idempotent_and_restart_uses_fresh_queue() {
        let mut capture = LinuxVideoCapturer::new(2, 2, 120).unwrap();
        capture.start().await.unwrap();
        let first = capture.run.as_ref().unwrap().task.as_ref().unwrap().id();
        capture.start().await.unwrap();
        assert_eq!(
            capture.run.as_ref().unwrap().task.as_ref().unwrap().id(),
            first
        );
        queue_full(&capture).await;
        capture.pause().await.unwrap();
        capture.update_resolution_and_fps(6, 120).unwrap();
        capture.resume().await.unwrap();
        assert_ne!(
            capture.run.as_ref().unwrap().task.as_ref().unwrap().id(),
            first
        );
        let received = tokio::time::timeout(TEST_TIMEOUT, capture.capture_frame())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            received.height, 6,
            "no queued frame from the old run may cross restart"
        );
        capture.stop().await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_stop_retains_handle_and_restart_awaits_old_exit() {
        let mut capture = LinuxVideoCapturer::new(2, 2, 120).unwrap();
        capture.start().await.unwrap();
        let old = capture
            .run
            .as_ref()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .abort_handle();
        {
            // On this current-thread test runtime the abort cannot complete
            // until we yield. Poll stop once, then cancel that stop future.
            let stop = capture.stop();
            tokio::pin!(stop);
            assert_pending(stop.as_mut()).await;
        }
        assert!(capture.run.as_ref().unwrap().task.is_some());
        tokio::time::timeout(TEST_TIMEOUT, capture.start())
            .await
            .unwrap()
            .unwrap();
        assert!(old.is_finished());
        assert!(capture.running.load(Ordering::SeqCst));
        tokio::task::yield_now().await;
        assert!(
            capture.running.load(Ordering::SeqCst),
            "old task cannot clear the new run flag"
        );
        capture.stop().await.unwrap();
    }

    #[tokio::test]
    async fn native_drop_wakes_local_wait_without_revoking_shared_mailbox() {
        let mailbox = Arc::new(FrameMailbox::new(1).unwrap());
        let mut capture = LinuxVideoCapturer::from_owned_mailbox(mailbox.clone(), 30).unwrap();
        capture.start().await.unwrap();
        let mut cancelled = capture.run.as_ref().unwrap().cancelled.subscribe();
        let receive = receive_native_frame(&mailbox, &mut cancelled);
        tokio::pin!(receive);
        assert_pending(receive.as_mut()).await;
        drop(capture);
        assert!(
            tokio::time::timeout(TEST_TIMEOUT, receive)
                .await
                .unwrap()
                .is_err()
        );
        assert!(
            !mailbox.is_closed(),
            "drop must preserve a reused source lease"
        );
        let mut replacement = LinuxVideoCapturer::from_owned_mailbox(mailbox.clone(), 30).unwrap();
        replacement.start().await.unwrap();
        assert!(!mailbox.is_closed());
    }

    #[tokio::test]
    async fn native_stop_wakes_wait_and_preserves_explicit_terminal_semantics() {
        let mailbox = Arc::new(FrameMailbox::new(1).unwrap());
        let mut capture = LinuxVideoCapturer::from_owned_mailbox(mailbox.clone(), 30).unwrap();
        capture.start().await.unwrap();
        let mut cancelled = capture.run.as_ref().unwrap().cancelled.subscribe();
        let receive = receive_native_frame(&mailbox, &mut cancelled);
        tokio::pin!(receive);
        assert_pending(receive.as_mut()).await;
        capture.stop().await.unwrap();
        assert!(
            tokio::time::timeout(TEST_TIMEOUT, receive)
                .await
                .unwrap()
                .is_err()
        );
        assert!(mailbox.is_closed());
        assert!(capture.start().await.is_err());
    }

    #[tokio::test]
    async fn native_pause_resume_preserves_mailbox_and_run_epoch() {
        let mailbox = Arc::new(FrameMailbox::new(1).unwrap());
        let mut capture = LinuxVideoCapturer::from_owned_mailbox(mailbox.clone(), 30).unwrap();
        capture.start().await.unwrap();
        let cancelled = capture.run.as_ref().unwrap().cancelled.subscribe();
        capture.pause().await.unwrap();
        assert!(mailbox.is_paused());
        capture.resume().await.unwrap();
        assert!(!mailbox.is_paused());
        assert!(!mailbox.is_closed());
        assert!(
            !*cancelled.borrow(),
            "pause/resume must keep the native run epoch"
        );
    }

    #[tokio::test]
    async fn native_mailbox_revocation_wakes_capture_wait() {
        let mailbox = Arc::new(FrameMailbox::new(1).unwrap());
        let mut capture = LinuxVideoCapturer::from_owned_mailbox(mailbox.clone(), 30).unwrap();
        capture.start().await.unwrap();
        let receive = capture.capture_frame();
        tokio::pin!(receive);
        assert_pending(receive.as_mut()).await;
        mailbox.close();
        assert!(
            tokio::time::timeout(TEST_TIMEOUT, receive)
                .await
                .unwrap()
                .is_err()
        );
    }
}
