use async_trait::async_trait;
use remote_core::{VideoCapturer, VideoFrame, VideoFrameHandleKind, VideoPixelFormat};
use std::error::Error;
use std::time::Duration;

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

/// FFmpeg owns Windows pixel capture. This lightweight clock only feeds capture
/// telemetry, so pause/resume never leaves a desktop-capture worker behind.
pub struct WindowsVideoCapturer {
    target_width: u32,
    target_height: u32,
    target_fps: u32,
    active: bool,
    last_frame_at: tokio::time::Instant,
}

impl WindowsVideoCapturer {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(Self {
            target_width: width,
            target_height: height,
            target_fps: fps.clamp(15, 120),
            active: false,
            last_frame_at: tokio::time::Instant::now(),
        })
    }

    pub fn update_resolution_and_fps(
        &mut self,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.target_width = width;
        self.target_height = height;
        self.target_fps = fps.clamp(15, 120);
        Ok(())
    }
}

#[async_trait]
impl VideoCapturer for WindowsVideoCapturer {
    type Frame = WindowsVideoFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.active = true;
        self.last_frame_at = tokio::time::Instant::now();
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.active = false;
        Ok(())
    }

    async fn capture_frame(&mut self) -> Result<Self::Frame, Box<dyn Error + Send + Sync>> {
        if !self.active {
            return Err("Windows capture clock is paused".into());
        }
        let interval = Duration::from_micros((1_000_000 / self.target_fps.max(1)) as u64);
        let due = self.last_frame_at + interval;
        if due > tokio::time::Instant::now() {
            tokio::time::sleep_until(due).await;
        }
        self.last_frame_at = tokio::time::Instant::now();
        let capture_ts_us = remote_core::timing::quanta_now_us();
        Ok(WindowsVideoFrame {
            width: self.target_width,
            height: self.target_height,
            timing: protocol::FrameTimingCheckpoints::new(capture_ts_us),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolution_update_changes_width_without_background_worker() {
        let mut capture = WindowsVideoCapturer::new(1920, 1080, 60).unwrap();
        capture.update_resolution_and_fps(1280, 720, 30).unwrap();
        capture.start().await.unwrap();
        let frame = capture.capture_frame().await.unwrap();
        assert_eq!((frame.width, frame.height), (1280, 720));
        capture.stop().await.unwrap();
        assert!(capture.capture_frame().await.is_err());
    }
}
