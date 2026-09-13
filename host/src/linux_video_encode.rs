use crate::ffmpeg_hevc::FfmpegHevcSource;
use crate::linux_capture::LinuxVideoFrame;
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

pub struct LinuxVideoEncoder {
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    source: Option<FfmpegHevcSource>,
    placeholder: bool,
    force_keyframe: bool,
}

impl LinuxVideoEncoder {
    pub fn new(
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        match FfmpegHevcSource::start(width, height, fps, bitrate_kbps) {
            Ok(source) => Ok(Self {
                width,
                height,
                fps,
                bitrate_kbps,
                source: Some(source),
                placeholder: false,
                force_keyframe: false,
            }),
            Err(err) => {
                eprintln!(
                    "[LinuxEncoder] ffmpeg HEVC unavailable ({err}); using non-decodable placeholder."
                );
                Ok(Self {
                    width,
                    height,
                    fps,
                    bitrate_kbps,
                    source: None,
                    placeholder: true,
                    force_keyframe: true,
                })
            }
        }
    }

    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    pub fn update_settings(&mut self, width: u32, height: u32, fps: u32, bitrate_kbps: u32) {
        self.width = width;
        self.height = height;
        self.fps = fps;
        self.bitrate_kbps = bitrate_kbps;
        if !self.placeholder {
            match FfmpegHevcSource::start(width, height, fps, bitrate_kbps) {
                Ok(source) => self.source = Some(source),
                Err(err) => eprintln!("[LinuxEncoder] failed to restart ffmpeg: {err}"),
            }
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

        tokio::time::sleep(std::time::Duration::from_millis(
            (1000 / self.fps.max(1)) as u64,
        ))
        .await;
        let capture_ts_us = remote_core::timing::quanta_now_us();
        let is_key = self.force_keyframe;
        self.force_keyframe = false;
        Ok(EncodedChunk {
            nalu: Vec::new(),
            capture_time_ms: (capture_ts_us / 1000) as u32,
            is_keyframe: is_key,
            encode_cost_ms: 0.0,
            timing: protocol::FrameTimingCheckpoints::new(capture_ts_us),
        })
    }
}

#[async_trait]
impl VideoEncoder for LinuxVideoEncoder {
    type Frame = LinuxVideoFrame;

    async fn submit_frame(
        &mut self,
        frame: Self::Frame,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let _ = frame;
        Ok(())
    }

    async fn pull_encoded(&mut self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        Ok(self.pull_encoded_chunk().await?.nalu)
    }
}
