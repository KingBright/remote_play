use crate::ffmpeg_hevc::{FfmpegHevcSource, replace_capture_source};
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
    force_keyframe: bool,
}

impl LinuxVideoEncoder {
    pub fn new(
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let source = FfmpegHevcSource::start(width, height, fps, bitrate_kbps)?;
        Ok(Self {
            width,
            height,
            fps,
            bitrate_kbps,
            source: Some(source),
            force_keyframe: false,
        })
    }

    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    pub fn update_settings(
        &mut self,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if (self.width, self.height, self.fps, self.bitrate_kbps)
            == (width, height, fps, bitrate_kbps)
        {
            return Ok(());
        }
        if self.source.is_some() {
            replace_capture_source(&mut self.source, || {
                FfmpegHevcSource::start(width, height, fps, bitrate_kbps)
            })?;
            self.force_keyframe = true;
        }
        self.width = width;
        self.height = height;
        self.fps = fps;
        self.bitrate_kbps = bitrate_kbps;
        Ok(())
    }

    pub fn set_paused(&mut self, paused: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
        if paused {
            self.source = None;
        } else if self.source.is_none() {
            self.source = Some(FfmpegHevcSource::start(
                self.width,
                self.height,
                self.fps,
                self.bitrate_kbps,
            )?);
            self.force_keyframe = true;
        }
        Ok(())
    }

    pub async fn pull_encoded_chunk(
        &mut self,
    ) -> Result<EncodedChunk, Box<dyn Error + Send + Sync>> {
        if let Some(source) = &self.source {
            loop {
                let started = std::time::Instant::now();
                let (nalu, is_keyframe) = source.pull_access_unit().await?;
                if self.force_keyframe && !is_keyframe {
                    continue;
                }
                if is_keyframe {
                    self.force_keyframe = false;
                }
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
        }

        std::future::pending().await
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

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[test]
    fn paused_settings_update_never_starts_ffmpeg() {
        let mut encoder = LinuxVideoEncoder {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 8000,
            source: None,
            force_keyframe: false,
        };
        encoder.update_settings(1280, 720, 30, 4000).unwrap();
        assert!(encoder.source.is_none());
        assert_eq!(
            (
                encoder.width,
                encoder.height,
                encoder.fps,
                encoder.bitrate_kbps
            ),
            (1280, 720, 30, 4000)
        );
    }
}
