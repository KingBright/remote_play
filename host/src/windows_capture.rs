use async_trait::async_trait;
use remote_core::{VideoCapturer, VideoFrame, VideoFrameHandleKind, VideoPixelFormat};
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};

pub struct WindowsVideoFrame {
    pub width: u32,
    pub height: u32,
    pub timing: protocol::FrameTimingCheckpoints,
}

impl VideoFrame for WindowsVideoFrame {
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
        VideoPixelFormat::Bgra8
    }
}

pub struct WindowsVideoCapturer {
    target_width: u32,
    target_height: u32,
    target_fps: u32,
    running: Arc<AtomicBool>,
    frame_rx: Mutex<mpsc::Receiver<WindowsVideoFrame>>,
    frame_tx: mpsc::Sender<WindowsVideoFrame>,
}

impl WindowsVideoCapturer {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let (frame_tx, frame_rx) = mpsc::channel(4);
        Ok(Self {
            target_width: width,
            target_height: height,
            target_fps: fps.clamp(15, 120),
            running: Arc::new(AtomicBool::new(false)),
            frame_rx: Mutex::new(frame_rx),
            frame_tx,
        })
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
impl VideoCapturer for WindowsVideoCapturer {
    type Frame = WindowsVideoFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let tx = self.frame_tx.clone();
        let width = self.target_width;
        let height = self.target_height;
        let fps = self.target_fps;
        tokio::task::spawn_blocking(move || {
            let interval = Duration::from_micros((1_000_000 / fps.max(1)) as u64);
            while running.load(Ordering::Relaxed) {
                let capture_ts_us = remote_core::timing::quanta_now_us();
                let _ = tx.blocking_send(WindowsVideoFrame {
                    width,
                    height,
                    timing: protocol::FrameTimingCheckpoints::new(capture_ts_us),
                });
                std::thread::sleep(interval);
            }
        });
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn capture_frame(&mut self) -> Result<Self::Frame, Box<dyn Error + Send + Sync>> {
        let mut rx = self.frame_rx.lock().await;
        rx.recv()
            .await
            .ok_or_else(|| "Windows capture channel closed".into())
    }
}
