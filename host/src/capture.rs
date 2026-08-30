use async_trait::async_trait;
use core_graphics::display::CGDisplay;
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
    slot: Arc<std::sync::Mutex<Option<MacVideoFrame>>>,
    notify: Arc<tokio::sync::Notify>,
    width: u32,
    height: u32,
}

impl SCStreamOutputTrait for StreamOutput {
    fn did_output_sample_buffer(
        &self,
        sample: screencapturekit::cm::CMSampleBuffer,
        _of_type: SCStreamOutputType,
    ) {
        let now_ms = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() & 0xFFFFFFFF) as u32;
        let frame = MacVideoFrame {
            width: self.width,
            height: self.height,
            sample_buffer: sample,
            capture_time_ms: now_ms,
        };

        if let Ok(mut lock) = self.slot.lock() {
            *lock = Some(frame);
        }
        self.notify.notify_one();
    }
}

pub struct MacVideoCapturer {
    slot: Arc<std::sync::Mutex<Option<MacVideoFrame>>>,
    notify: Arc<tokio::sync::Notify>,
    stream: Option<SCStream>,
    _target_width: u32,
    target_height: u32,
    target_fps: u32,
}

impl MacVideoCapturer {
    pub fn new(target_width: u32, target_height: u32, target_fps: u32) -> Self {
        Self {
            slot: Arc::new(std::sync::Mutex::new(None)),
            notify: Arc::new(tokio::sync::Notify::new()),
            stream: None,
            _target_width: target_width,
            target_height,
            target_fps,
        }
    }

    pub fn update_resolution_and_fps(
        &mut self,
        height: u32,
        fps: u32,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.target_height = height;
        self.target_fps = fps;
        if let Some(stream) = &self.stream {
            let content = SCShareableContent::get()?;
            let main_display_id = CGDisplay::main().id;
            let display = content
                .displays()
                .into_iter()
                .min_by_key(|display| u8::from(display.display_id() != main_display_id))
                .ok_or("No display found")?;

            let display_width = display.width();
            let display_height = display.height();

            let target_height = self.target_height.min(display_height);
            let scale = target_height as f32 / display_height as f32;
            let target_width = (display_width as f32 * scale) as u32;

            let mut config = SCStreamConfiguration::new();
            config.set_width(target_width);
            config.set_height(target_height);
            config.set_minimum_frame_interval(&CMTime::new(1, self.target_fps as i32));
            config.set_queue_depth(5);
            config.set_pixel_format(
                screencapturekit::stream::configuration::PixelFormat::YCbCr_420v,
            );
            config.set_shows_cursor(false);

            stream.update_configuration(&config)?;
        }
        Ok(())
    }
}

#[async_trait]
impl VideoCapturer for MacVideoCapturer {
    type Frame = MacVideoFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let content = SCShareableContent::get()?;
        let main_display_id = CGDisplay::main().id;
        let display = content
            .displays()
            .into_iter()
            .min_by_key(|display| u8::from(display.display_id() != main_display_id))
            .ok_or("No display found")?;

        let filter = SCContentFilter::create().with_display(&display).build();

        let mut config = SCStreamConfiguration::new();
        let display_width = display.width();
        let display_height = display.height();

        let target_height = self.target_height.min(display_height);
        let scale = target_height as f32 / display_height as f32;
        let target_width = (display_width as f32 * scale) as u32;

        config.set_width(target_width);
        config.set_height(target_height);
        config.set_minimum_frame_interval(&CMTime::new(1, self.target_fps as i32));
        config.set_queue_depth(5);
        config.set_pixel_format(screencapturekit::stream::configuration::PixelFormat::YCbCr_420v);
        config.set_shows_cursor(false);

        let output = StreamOutput {
            slot: self.slot.clone(),
            notify: self.notify.clone(),
            width: target_width,
            height: target_height,
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

    async fn capture_frame(&mut self) -> Result<Self::Frame, Box<dyn Error + Send + Sync>> {
        loop {
            if let Ok(mut lock) = self.slot.lock() {
                let frame_opt: Option<MacVideoFrame> = lock.take();
                if let Some(frame) = frame_opt {
                    return Ok(frame);
                }
            }

            match tokio::time::timeout(std::time::Duration::from_millis(500), self.notify.notified()).await {
                Ok(_) => {
                    if let Ok(mut lock) = self.slot.lock() {
                        let frame_opt: Option<MacVideoFrame> = lock.take();
                        if let Some(frame) = frame_opt {
                            return Ok(frame);
                        }
                    }
                }
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
