use async_trait::async_trait;
use remote_core::{VideoCapturer, VideoFrame, VideoFrameHandleKind};
use screencapturekit::prelude::*;
use std::error::Error;
use std::sync::Arc;

use screencapturekit::cm::CMSampleBuffer;

pub struct MacVideoFrame {
    pub width: u32,
    pub height: u32,
    pub sample_buffer: CMSampleBuffer,
    pub capture_time_ms: u32,
    pub timing: protocol::FrameTimingCheckpoints,
}

impl VideoFrame for MacVideoFrame {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }

    fn handle_kind(&self) -> VideoFrameHandleKind {
        VideoFrameHandleKind::MacosCvPixelBuffer
    }
}

struct StreamOutput {
    slot: Arc<remote_core::LatestFrameSlot<MacVideoFrame>>,
}

impl SCStreamOutputTrait for StreamOutput {
    fn did_output_sample_buffer(
        &self,
        sample: screencapturekit::cm::CMSampleBuffer,
        _of_type: SCStreamOutputType,
    ) {
        let capture_ts_us = remote_core::timing::quanta_now_us();
        let now_ms = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            & 0xFFFFFFFF) as u32;
        let Some(buffer) = sample.image_buffer() else {
            return;
        };
        let frame = MacVideoFrame {
            width: buffer.width() as u32,
            height: buffer.height() as u32,
            sample_buffer: sample,
            capture_time_ms: now_ms,
            timing: protocol::FrameTimingCheckpoints::new(capture_ts_us),
        };

        self.slot.push(frame);
    }
}

pub struct MacVideoCapturer {
    slot: Arc<remote_core::LatestFrameSlot<MacVideoFrame>>,
    stream: Option<SCStream>,
    target_width: u32,
    source: protocol::session::CaptureSource,
    source_size: (u32, u32),
    geometry_checked: std::time::Instant,
    target_height: u32,
    target_fps: u32,
    pacer: remote_core::frame_pacer::FramePacer,
}

impl MacVideoCapturer {
    pub fn new(target_width: u32, target_height: u32, target_fps: u32) -> Self {
        Self {
            slot: Arc::new(remote_core::LatestFrameSlot::new()),
            stream: None,
            target_width,
            source: protocol::session::CaptureSource::MainDisplay,
            source_size: (0, 0),
            geometry_checked: std::time::Instant::now(),
            target_height,
            target_fps,
            pacer: remote_core::frame_pacer::FramePacer::new(target_fps),
        }
    }

    pub fn with_source(mut self, source: protocol::session::CaptureSource) -> Self {
        self.source = source;
        self
    }

    fn configuration(&self, size: (u32, u32)) -> SCStreamConfiguration {
        let (width, height) =
            protocol::session::fit_capture_size(size, (self.target_width, self.target_height));
        let mut config = SCStreamConfiguration::new();
        config.set_width(width);
        config.set_height(height);
        config.set_minimum_frame_interval(&CMTime::new(
            1,
            self.target_fps.saturating_mul(2).min(i32::MAX as u32) as i32,
        ));
        config.set_queue_depth(3);
        config.set_pixel_format(screencapturekit::stream::configuration::PixelFormat::YCbCr_420v);
        config.set_shows_cursor(false);
        config
    }

    pub fn update_resolution_and_fps(
        &mut self,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if (self.target_width, self.target_height, self.target_fps) == (width, height, fps) {
            return Ok(());
        }
        self.target_width = width;
        self.target_height = height;
        self.target_fps = fps;
        self.pacer.reset(fps);
        if let Some(stream) = &self.stream {
            stream.update_configuration(&self.configuration(self.source_size))?;
        }
        Ok(())
    }
    fn refresh_geometry(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.geometry_checked.elapsed() < std::time::Duration::from_secs(1) {
            return Ok(());
        }
        self.geometry_checked = std::time::Instant::now();
        let (_, size) = crate::capture_sources::filter(self.source)?;
        if size != self.source_size {
            self.source_size = size;
            if let Some(stream) = &self.stream {
                stream.update_configuration(&self.configuration(size))?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl VideoCapturer for MacVideoCapturer {
    type Frame = MacVideoFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let (filter, size) = crate::capture_sources::filter(self.source)?;
        self.source_size = size;
        let config = self.configuration(size);
        let output = StreamOutput {
            slot: self.slot.clone(),
        };

        let mut stream = SCStream::new(&filter, &config);
        stream.add_output_handler(output, SCStreamOutputType::Screen);
        stream.start_capture()?;

        self.stream = Some(stream);
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if let Some(stream) = self.stream.take() {
            stream.stop_capture()?;
        }
        Ok(())
    }

    async fn pause(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if let Some(stream) = &self.stream {
            stream.stop_capture()?;
        }
        drop(self.slot.take());
        Ok(())
    }

    async fn resume(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        drop(self.slot.take());
        self.pacer.reset(self.target_fps);
        if let Some(stream) = &self.stream {
            stream.start_capture()?;
        } else {
            self.start().await?;
        }
        Ok(())
    }

    async fn capture_frame(&mut self) -> Result<Self::Frame, Box<dyn Error + Send + Sync>> {
        loop {
            self.refresh_geometry()?;
            match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                self.slot.take_async(),
            )
            .await
            {
                Ok(frame) if self.pacer.admit(frame.timing.capture_ts_us) => return Ok(frame),
                Ok(_) => continue,
                Err(_) => {
                    // Check if stream is still active
                    if self.stream.is_none() {
                        return Err("Capture stream stopped".into());
                    }
                }
            }
        }
    }
}

impl Drop for MacVideoCapturer {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.stop_capture();
        }
    }
}
