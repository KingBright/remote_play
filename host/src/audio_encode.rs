use crate::audio_capture::MacAudioFrame;
use async_trait::async_trait;
use opus::{Application, Channels, Encoder};
use remote_core::{AudioEncoder, AudioFrame};
use std::error::Error;

pub struct OpusAudioEncoder {
    encoder: Encoder,
}

impl OpusAudioEncoder {
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let ch = if channels == 1 {
            Channels::Mono
        } else {
            Channels::Stereo
        };
        let encoder = Encoder::new(sample_rate, ch, Application::LowDelay)?;
        Ok(Self { encoder })
    }
}

#[async_trait]
impl AudioEncoder for OpusAudioEncoder {
    type Frame = MacAudioFrame;

    async fn encode(
        &mut self,
        frame: Self::Frame,
    ) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        // Typical Opus frame size is 2.5, 5, 10, 20, 40 or 60 ms.
        // We assume the incoming frame has the correct number of samples.
        let max_size = 4000;
        let mut output = vec![0u8; max_size];
        let size = self.encoder.encode_float(frame.samples(), &mut output)?;
        output.truncate(size);
        Ok(output)
    }
}
