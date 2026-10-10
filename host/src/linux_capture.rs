use crate::linux_frame::{FrameMailbox, OwnedFrame, PixelFormat};
use async_trait::async_trait;
use remote_core::{VideoCapturer, VideoFrame, VideoFrameHandleKind, VideoPixelFormat};
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, mpsc};

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
    frame_rx: Mutex<mpsc::Receiver<LinuxVideoFrame>>,
    frame_tx: mpsc::Sender<LinuxVideoFrame>,
    native: Option<Arc<FrameMailbox>>,
}

impl LinuxVideoCapturer {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let (frame_tx, frame_rx) = mpsc::channel(4);
        Ok(Self {
            target_width: width,
            target_height: height,
            target_fps: fps.clamp(15, 120),
            running: Arc::new(AtomicBool::new(false)),
            frame_rx: Mutex::new(frame_rx),
            frame_tx,
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
}

#[async_trait]
impl VideoCapturer for LinuxVideoCapturer {
    type Frame = LinuxVideoFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.running.store(true, Ordering::SeqCst);
        if let Some(mailbox) = &self.native {
            mailbox.set_paused(false);
            return Ok(());
        }
        let running = self.running.clone();
        let tx = self.frame_tx.clone();
        let width = self.target_width;
        let height = self.target_height;
        let fps = self.target_fps;

        // Background capture loop
        tokio::task::spawn_blocking(move || {
            let interval = Duration::from_micros((1_000_000 / fps.max(1)) as u64);
            let mut last_capture = std::time::Instant::now();
            let mut frame_count: u64 = 0;

            println!("[LinuxCapture] Capture loop started: {width}x{height} @ {fps}fps");

            while running.load(Ordering::Relaxed) {
                let elapsed = last_capture.elapsed();
                if elapsed < interval {
                    std::thread::sleep(interval - elapsed);
                }
                last_capture = std::time::Instant::now();

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

                let _ = tx.blocking_send(frame);
            }
            println!("[LinuxCapture] Capture loop stopped.");
        });

        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.running.store(false, Ordering::SeqCst);
        if let Some(mailbox) = &self.native {
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
        if let Some(mailbox) = &self.native {
            return Ok(LinuxVideoFrame::from_owned(mailbox.receive().await?));
        }
        let mut rx = self.frame_rx.lock().await;
        rx.recv()
            .await
            .ok_or_else(|| "Linux capture channel closed".into())
    }
}
