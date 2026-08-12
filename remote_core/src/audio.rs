use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioChunkerError {
    InvalidSampleRate,
    InvalidChannels,
    InvalidFrameDuration,
}

impl fmt::Display for AudioChunkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioChunkerError::InvalidSampleRate => write!(f, "audio sample rate must be non-zero"),
            AudioChunkerError::InvalidChannels => write!(f, "audio channels must be non-zero"),
            AudioChunkerError::InvalidFrameDuration => {
                write!(f, "audio frame duration must produce at least one sample")
            }
        }
    }
}

impl Error for AudioChunkerError {}

#[derive(Debug, Clone)]
pub struct InterleavedAudioFrameChunker {
    channels: u16,
    samples_per_channel_per_frame: usize,
    samples_per_frame: usize,
    buffered_samples: Vec<f32>,
}

impl InterleavedAudioFrameChunker {
    pub fn new(
        sample_rate: u32,
        channels: u16,
        frame_duration_ms: u32,
    ) -> Result<Self, AudioChunkerError> {
        if sample_rate == 0 {
            return Err(AudioChunkerError::InvalidSampleRate);
        }
        if channels == 0 {
            return Err(AudioChunkerError::InvalidChannels);
        }

        let samples_per_channel_per_frame =
            (sample_rate as usize * frame_duration_ms as usize) / 1_000;
        if samples_per_channel_per_frame == 0 {
            return Err(AudioChunkerError::InvalidFrameDuration);
        }

        let samples_per_frame = samples_per_channel_per_frame * channels as usize;
        Ok(Self {
            channels,
            samples_per_channel_per_frame,
            samples_per_frame,
            buffered_samples: Vec::with_capacity(samples_per_frame * 2),
        })
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn samples_per_channel_per_frame(&self) -> usize {
        self.samples_per_channel_per_frame
    }

    pub fn samples_per_frame(&self) -> usize {
        self.samples_per_frame
    }

    pub fn buffered_samples(&self) -> usize {
        self.buffered_samples.len()
    }

    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        self.buffered_samples.extend_from_slice(samples);

        let complete_frames = self.buffered_samples.len() / self.samples_per_frame;
        let mut frames = Vec::with_capacity(complete_frames);
        for _ in 0..complete_frames {
            frames.push(
                self.buffered_samples
                    .drain(..self.samples_per_frame)
                    .collect::<Vec<_>>(),
            );
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_variable_input_into_fixed_interleaved_frames() {
        let mut chunker = InterleavedAudioFrameChunker::new(48_000, 2, 20).unwrap();
        assert_eq!(chunker.samples_per_channel_per_frame(), 960);
        assert_eq!(chunker.samples_per_frame(), 1_920);

        let first = chunker.push(&vec![0.1; 700]);
        assert!(first.is_empty());
        assert_eq!(chunker.buffered_samples(), 700);

        let second = chunker.push(&vec![0.2; 2_000]);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].len(), 1_920);
        assert_eq!(chunker.buffered_samples(), 780);

        let third = chunker.push(&vec![0.3; 3_100]);
        assert_eq!(third.len(), 2);
        assert_eq!(third[0].len(), 1_920);
        assert_eq!(third[1].len(), 1_920);
        assert_eq!(chunker.buffered_samples(), 40);
    }

    #[test]
    fn rejects_invalid_audio_layouts() {
        assert_eq!(
            InterleavedAudioFrameChunker::new(0, 2, 20).unwrap_err(),
            AudioChunkerError::InvalidSampleRate
        );
        assert_eq!(
            InterleavedAudioFrameChunker::new(48_000, 0, 20).unwrap_err(),
            AudioChunkerError::InvalidChannels
        );
        assert_eq!(
            InterleavedAudioFrameChunker::new(8_000, 1, 0).unwrap_err(),
            AudioChunkerError::InvalidFrameDuration
        );
    }
}
