use crate::ffmpeg_hevc::{FfmpegHevcSource, replace_capture_source};
use crate::linux_capture::LinuxVideoFrame;
use crate::linux_raw_encode::{RawHevcSource, RawSettings};
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
    /// Local sidecar only; no public wire timing/identity changes.
    pub native_stamp: Option<crate::linux_frame::FrameStamp>,
}

pub struct LinuxVideoEncoder {
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    source: Option<FfmpegHevcSource>,
    force_keyframe: bool,
    native_generation: Option<u64>,
    native: Option<RawHevcSource>,
    paused: bool,
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
            native_generation: None,
            native: None,
            paused: false,
        })
    }

    pub(crate) fn from_owned_frames(
        generation: u64,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        protocol::validate_video_settings(width, height, fps, bitrate_kbps)?;
        if generation == 0 {
            return Err("native capture generation must be nonzero".into());
        }
        Ok(Self {
            width,
            height,
            fps,
            bitrate_kbps,
            source: None,
            force_keyframe: true,
            native_generation: Some(generation),
            native: None,
            paused: false,
        })
    }

    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
        if self.native_generation.is_some() {
            self.native = None;
        }
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
        if self.native_generation.is_some() {
            protocol::validate_video_settings(width, height, fps, bitrate_kbps)?;
            self.native = None;
            self.force_keyframe = true;
        } else if self.source.is_some() {
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
        self.paused = paused;
        if self.native_generation.is_some() {
            if paused {
                self.native = None;
            }
            self.force_keyframe = true;
            return Ok(());
        }
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
        if self.native_generation.is_some() {
            if let Some(native) = &self.native {
                let chunk = native.pull().await?;
                if self.force_keyframe && !chunk.is_keyframe {
                    return Err("native encoder did not begin with a keyframe".into());
                }
                self.force_keyframe = false;
                // Arrival is kept in the host clock domain. SPA PTS remains in
                // RawChunk's local stamp; it is never cast to capture_ts_us.
                let elapsed = chunk
                    .encode_done_ts_us
                    .saturating_sub(chunk.stamp.arrival_ts_us);
                let mut timing = protocol::FrameTimingCheckpoints::new(chunk.stamp.arrival_ts_us);
                timing.encode_done_ts_us = elapsed.min(u32::MAX as u64) as u32;
                return Ok(EncodedChunk {
                    nalu: chunk.nalu,
                    capture_time_ms: (chunk.stamp.arrival_ts_us / 1000) as u32,
                    is_keyframe: chunk.is_keyframe,
                    encode_cost_ms: elapsed as f32 / 1000.0,
                    timing,
                    native_stamp: Some(chunk.stamp),
                });
            }
            return std::future::pending().await;
        }
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
                    native_stamp: None,
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
        if let Some(generation) = self.native_generation {
            if self.paused {
                return Err("native encoder is paused".into());
            }
            let frame = frame
                .owned
                .ok_or("native encoder requires an owned capture frame")?;
            frame.validate_owned()?;
            if frame.stamp.generation != generation {
                return Err("native capture generation is stale".into());
            }
            if self
                .native
                .as_ref()
                .is_none_or(|native| !native.matches(&frame))
            {
                // A format/color change never reuses the previous SPS or bytes.
                self.native = None;
                self.native = Some(RawHevcSource::start(
                    &frame,
                    RawSettings {
                        width: self.width,
                        height: self.height,
                        fps: self.fps,
                        bitrate_kbps: self.bitrate_kbps,
                    },
                )?);
                self.force_keyframe = true;
            }
            self.native.as_ref().unwrap().submit(frame)?;
        } else if frame.owned.is_some() {
            return Err("owned frames cannot be discarded by the X11 compatibility encoder".into());
        }
        Ok(())
    }

    async fn pull_encoded(&mut self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        Ok(self.pull_encoded_chunk().await?.nalu)
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::linux_frame::{FrameMailbox, PixelFormat, SourceColor};
    use crate::linux_raw_encode::tests::{frame, known_color, wait_for_writer};
    use remote_core::VideoCapturer;
    use std::sync::Arc;
    use std::time::Duration;

    #[cfg(unix)]
    #[tokio::test]
    async fn actual_owned_capture_trait_enters_hevc_and_color_change_restarts_with_idr() {
        let mailbox = Arc::new(FrameMailbox::new(9).unwrap());
        let mut capture =
            crate::linux_capture::LinuxVideoCapturer::from_owned_mailbox(mailbox.clone(), 30)
                .unwrap();
        let mut encoder = LinuxVideoEncoder::from_owned_frames(9, 64, 64, 30, 2000).unwrap();
        capture.start().await.unwrap();
        for color in [
            known_color(),
            SourceColor {
                range: 2,
                ..SourceColor::default()
            },
        ] {
            for sequence in 0..6 {
                assert!(mailbox.publish(frame(PixelFormat::Nv12, color, sequence)));
                let captured = capture.capture_frame().await.unwrap();
                let arrival = captured.timing.capture_ts_us;
                assert!(arrival != 0);
                encoder.submit_frame(captured).await.unwrap();
                wait_for_writer(encoder.native.as_ref().unwrap()).await;
            }
            encoder.native.as_ref().unwrap().finish_input();
            for sequence in 0..6 {
                let chunk =
                    tokio::time::timeout(Duration::from_secs(5), encoder.pull_encoded_chunk())
                        .await
                        .unwrap()
                        .unwrap();
                if sequence == 0 {
                    assert!(chunk.is_keyframe);
                }
                assert!(!chunk.nalu.is_empty());
                assert!(chunk.timing.capture_ts_us > 0);
                assert!(chunk.timing.encode_done_ts_us > 0);
                assert_eq!(chunk.native_stamp.unwrap().sequence, Some(sequence));
                assert_eq!(
                    chunk.native_stamp.unwrap().pipewire_pts_ns,
                    Some(900_000_000 + sequence as i64)
                );
            }
        }
        capture.pause().await.unwrap();
        encoder.set_paused(true).unwrap();
        assert!(!mailbox.publish(frame(PixelFormat::Nv12, known_color(), 20)));
        assert!(encoder.native.is_none());
        encoder.update_settings(128, 64, 60, 4000).unwrap();
        assert!(encoder.native.is_none());
        capture.resume().await.unwrap();
        encoder.set_paused(false).unwrap();
        assert!(mailbox.publish(frame(PixelFormat::Nv12, known_color(), 21)));
        let mut stale = capture.capture_frame().await.unwrap();
        stale.owned.as_mut().unwrap().stamp.generation = 10;
        assert!(encoder.submit_frame(stale).await.is_err());
        assert!(encoder.native.is_none());
        capture.stop().await.unwrap();
        assert!(!mailbox.publish(frame(PixelFormat::Nv12, known_color(), 22)));
    }
    #[test]
    fn paused_settings_update_never_starts_ffmpeg() {
        let mut encoder = LinuxVideoEncoder {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 8000,
            source: None,
            force_keyframe: false,
            native_generation: None,
            native: None,
            paused: true,
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
