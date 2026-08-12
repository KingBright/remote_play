use crate::audio_capture::MacAudioFrame;
use async_trait::async_trait;
use opus::{Application, Channels, Encoder};
use remote_core::audio::InterleavedAudioFrameChunker;
use remote_core::{AudioEncoder, AudioFrame};
use std::collections::VecDeque;
use std::error::Error;

const OPUS_FRAME_DURATION_MS: u32 = 20;

pub struct OpusAudioEncoder {
    encoder: Encoder,
    chunker: InterleavedAudioFrameChunker,
    pending_packets: VecDeque<Vec<u8>>,
}

impl OpusAudioEncoder {
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let ch = match channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            other => {
                return Err(format!(
                    "unsupported Opus channel count {other}; expected mono or stereo"
                )
                .into());
            }
        };
        let encoder = Encoder::new(sample_rate, ch, Application::LowDelay)?;
        let chunker =
            InterleavedAudioFrameChunker::new(sample_rate, channels, OPUS_FRAME_DURATION_MS)?;
        Ok(Self {
            encoder,
            chunker,
            pending_packets: VecDeque::new(),
        })
    }

    pub fn samples_per_packet_per_channel(&self) -> usize {
        self.chunker.samples_per_channel_per_frame()
    }

    pub fn frame_duration_ms(&self) -> u16 {
        OPUS_FRAME_DURATION_MS as u16
    }

    pub fn encode_packets<F: AudioFrame>(
        &mut self,
        frame: &F,
    ) -> Result<Vec<Vec<u8>>, Box<dyn Error + Send + Sync>> {
        if frame.channels() != self.chunker.channels() {
            return Err(format!(
                "audio channel count changed from {} to {}",
                self.chunker.channels(),
                frame.channels()
            )
            .into());
        }

        let mut packets = Vec::new();
        for samples in self.chunker.push(frame.samples()) {
            let mut output = vec![0u8; 4_000];
            let size = self.encoder.encode_float(&samples, &mut output)?;
            output.truncate(size);
            if !output.is_empty() {
                packets.push(output);
            }
        }
        Ok(packets)
    }
}

#[async_trait]
impl AudioEncoder for OpusAudioEncoder {
    type Frame = MacAudioFrame;

    async fn encode(
        &mut self,
        frame: Self::Frame,
    ) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        if let Some(packet) = self.pending_packets.pop_front() {
            return Ok(packet);
        }

        let mut packets = self.encode_packets(&frame)?;
        if packets.is_empty() {
            return Ok(Vec::new());
        }

        let first = packets.remove(0);
        self.pending_packets.extend(packets);
        Ok(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(samples: usize) -> MacAudioFrame {
        MacAudioFrame {
            samples: vec![0.0; samples],
            sample_rate: 48_000,
            channels: 1,
            source: protocol::AudioSource::RemoteMicrophone,
        }
    }

    #[test]
    fn packetizer_encodes_only_complete_opus_frames() {
        let mut encoder = OpusAudioEncoder::new(48_000, 1).unwrap();

        assert!(encoder.encode_packets(&frame(480)).unwrap().is_empty());

        let packets = encoder.encode_packets(&frame(480)).unwrap();
        assert_eq!(packets.len(), 1);
        assert!(!packets[0].is_empty());

        let packets = encoder.encode_packets(&frame(1_920)).unwrap();
        assert_eq!(packets.len(), 2);
        assert!(packets.iter().all(|packet| !packet.is_empty()));
    }

    #[test]
    fn rejects_channel_counts_that_opus_encoder_cannot_signal() {
        let err = match OpusAudioEncoder::new(48_000, 6) {
            Ok(_) => panic!("surround input needs downmixing"),
            Err(err) => err,
        };

        assert!(
            err.to_string().contains("unsupported Opus channel count"),
            "unexpected error: {err}"
        );
    }
}
