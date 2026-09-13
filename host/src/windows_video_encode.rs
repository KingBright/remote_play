use crate::ffmpeg_hevc::FfmpegHevcSource;
use crate::windows_capture::WindowsVideoFrame;
use async_trait::async_trait;
use remote_core::VideoEncoder;
use std::error::Error;

#[derive(Clone, Debug)]
pub struct EncodedChunk {
    pub nalu: Vec<u8>,
    pub capture_time_ms: u32,
    pub is_keyframe: bool,
    pub encode_cost_ms: f32,
    pub timing: protocol::FrameTimingCheckpoints,
}

pub struct WindowsVideoEncoder {
    source: Option<FfmpegHevcSource>,
    force_keyframe: bool,
}

impl WindowsVideoEncoder {
    pub fn new(
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        match FfmpegHevcSource::start(width, height, fps, bitrate_kbps) {
            Ok(source) => Ok(Self {
                source: Some(source),
                force_keyframe: false,
            }),
            Err(err) => {
                eprintln!("[WindowsEncoder] ffmpeg HEVC unavailable ({err})");
                Ok(Self {
                    source: None,
                    force_keyframe: true,
                })
            }
        }
    }

    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    pub fn update_settings(&mut self, width: u32, height: u32, fps: u32, bitrate_kbps: u32) {
        if let Ok(source) = FfmpegHevcSource::start(width, height, fps, bitrate_kbps) {
            self.source = Some(source);
        }
    }

    pub async fn pull_encoded_chunk(
        &mut self,
    ) -> Result<EncodedChunk, Box<dyn Error + Send + Sync>> {
        if let Some(source) = &self.source {
            let started = std::time::Instant::now();
            let (nalu, is_keyframe) = tokio::task::block_in_place(|| source.pull_access_unit())?;
            let capture_ts_us = remote_core::timing::quanta_now_us();
            let mut timing = protocol::FrameTimingCheckpoints::new(capture_ts_us);
            timing.encode_done_ts_us = started.elapsed().as_micros() as u32;
            return Ok(EncodedChunk {
                nalu,
                capture_time_ms: (capture_ts_us / 1000) as u32,
                is_keyframe,
                encode_cost_ms: started.elapsed().as_secs_f32() * 1000.0,
                timing,
            });
        }
        let _ = self.force_keyframe;
        let capture_ts_us = remote_core::timing::quanta_now_us();
        Ok(EncodedChunk {
            nalu: Vec::new(),
            capture_time_ms: (capture_ts_us / 1000) as u32,
            is_keyframe: false,
            encode_cost_ms: 0.0,
            timing: protocol::FrameTimingCheckpoints::new(capture_ts_us),
        })
    }
}

#[async_trait]
impl VideoEncoder for WindowsVideoEncoder {
    type Frame = WindowsVideoFrame;

    async fn submit_frame(
        &mut self,
        _frame: Self::Frame,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        Ok(())
    }

    async fn pull_encoded(&mut self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        Ok(self.pull_encoded_chunk().await?.nalu)
    }
}
