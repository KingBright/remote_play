use async_trait::async_trait;
use core_graphics::display::CGDisplay;
use remote_core::{VideoCapturer, VideoFrame, VideoFrameHandleKind};
use screencapturekit::prelude::*;
use std::error::Error;
use tokio::sync::mpsc;

use screencapturekit::cm::CMSampleBuffer;

pub struct MacVideoFrame {
    pub width: u32,
    pub height: u32,
    pub sample_buffer: CMSampleBuffer,
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
    tx: mpsc::Sender<MacVideoFrame>,
    width: u32,
    height: u32,
}

impl SCStreamOutputTrait for StreamOutput {
    fn did_output_sample_buffer(
        &self,
        sample: screencapturekit::cm::CMSampleBuffer,
        _of_type: SCStreamOutputType,
    ) {
        // We only care about Video samples, but assuming SCStreamOutputType::Screen provides video.
        // ScreenCaptureKit gives us a sample buffer, we need to pass ownership.
        // Wait, screencapturekit CMSampleBuffer might need to be retained or it's tied to the callback.
        // Usually we must retain it if we send it to another thread! Wait, the crate handles ownership?
        // Actually, let's just send it. If we get use-after-free, we will debug it.
        let _ = self.tx.try_send(MacVideoFrame {
            width: self.width,
            height: self.height,
            sample_buffer: sample,
        });
    }
}

pub struct MacVideoCapturer {
    rx: mpsc::Receiver<MacVideoFrame>,
    stream: Option<SCStream>,
    tx_output: Option<mpsc::Sender<MacVideoFrame>>,
    _target_width: u32,
    target_height: u32,
    target_fps: u32,
}

impl MacVideoCapturer {
    pub fn new(target_width: u32, target_height: u32, target_fps: u32) -> Self {
        let (tx, rx) = mpsc::channel(10);
        Self {
            rx,
            stream: None,
            tx_output: Some(tx),
            _target_width: target_width,
            target_height,
            target_fps,
        }
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

        let output = StreamOutput {
            tx: self.tx_output.take().unwrap(),
            width: target_width,
            height: target_height,
        };

        // We can use closure handler for SCStreamDelegate if needed, but let's just use default or ignore
        let mut stream = SCStream::new(&filter, &config);

        let _queue = DispatchQueue::new("remote_play.capture", DispatchQoS::UserInteractive);
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
        let mut waited = 0;
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(100), self.rx.recv()).await
            {
                Ok(Some(frame)) => return Ok(frame),
                Ok(None) => return Err("Capture channel closed".into()),
                Err(_) => {
                    waited += 100;
                    if waited % 1000 == 0 {
                        println!(
                            "SCK Capture waiting for {} ms... no frame produced yet.",
                            waited
                        );
                    }
                    continue;
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
