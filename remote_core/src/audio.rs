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
        let consumed = complete_frames * self.samples_per_frame;
        let mut frames = Vec::with_capacity(complete_frames);
        {
            let mut samples = self.buffered_samples.drain(..consumed);
            for _ in 0..complete_frames {
                frames.push(samples.by_ref().take(self.samples_per_frame).collect());
            }
        }
        // Dropping the single drain shifts only the incomplete tail, once.
        // Draining per frame made capture bursts quadratic in their size.
        frames
    }
}

/// Callback-owned PCM queues. Independent sources advance on the same output
/// clock instead of being concatenated. Storage is allocated before playback.
pub struct AudioMixer {
    channels: usize,
    capacity_samples: usize,
    lanes: Vec<AudioMixLane>,
}

struct AudioMixLane {
    stream_id: Option<u32>,
    samples: std::collections::VecDeque<f32>,
}

impl AudioMixer {
    pub fn new(channels: usize, capacity_frames: usize, lanes: usize) -> Self {
        assert!(channels > 0 && capacity_frames > 0 && lanes > 0);
        let capacity_samples = channels * capacity_frames;
        Self {
            channels,
            capacity_samples,
            lanes: (0..lanes)
                .map(|_| AudioMixLane {
                    stream_id: None,
                    samples: std::collections::VecDeque::with_capacity(capacity_samples),
                })
                .collect(),
        }
    }

    /// Returns the number of stale samples discarded. Overflow keeps the
    /// newest complete channel frames; a new session clears that source's tail.
    pub fn push(&mut self, lane: usize, stream_id: u32, samples: &[f32]) -> usize {
        let lane = &mut self.lanes[lane];
        let mut dropped = 0;
        if lane.stream_id != Some(stream_id) {
            dropped += lane.samples.len();
            lane.samples.clear();
            lane.stream_id = Some(stream_id);
        }
        let samples = &samples[..samples.len() / self.channels * self.channels];
        let keep = samples.len().min(self.capacity_samples);
        dropped += samples.len() - keep;
        let samples = &samples[samples.len() - keep..];
        let excess = (lane.samples.len() + keep).saturating_sub(self.capacity_samples);
        let excess = excess.div_ceil(self.channels) * self.channels;
        let excess = excess.min(lane.samples.len());
        lane.samples.drain(..excess);
        dropped += excess;
        lane.samples.extend(samples.iter().copied());
        dropped
    }

    pub fn clear_stream(&mut self, stream_id: u32) {
        for lane in &mut self.lanes {
            if lane.stream_id == Some(stream_id) {
                lane.samples.clear();
            }
        }
    }

    /// No mutex, allocation, per-sample channel receive, or tail memmove.
    pub fn render<T>(&mut self, output: &mut [T], map: impl Fn(f32) -> T) {
        for sample in output {
            let mixed: f32 = self
                .lanes
                .iter_mut()
                .map(|lane| lane.samples.pop_front().unwrap_or(0.0))
                .sum();
            *sample = map(mixed.clamp(-1.0, 1.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simultaneous_audio_sources_mix_without_doubling_playback_duration() {
        let mut mixer = AudioMixer::new(2, 2880, 2);
        mixer.push(0, 10, &vec![0.2; 1920]);
        mixer.push(1, 11, &vec![0.3; 1920]);
        let mut output = vec![0.0; 1920];
        mixer.render(&mut output, |v| v);
        assert!(output.iter().all(|v| (*v - 0.5).abs() < 1e-6));
        mixer.render(&mut output, |v| v);
        assert!(output.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn audio_overload_keeps_recent_frames_and_preserves_stereo_alignment() {
        let mut mixer = AudioMixer::new(2, 3, 1);
        mixer.push(0, 10, &[0.1, -0.1, 0.2, -0.2]);
        assert_eq!(mixer.push(0, 10, &[0.3, -0.3, 0.4, -0.4, 0.5, -0.5]), 4);
        let mut out = [0.0; 6];
        mixer.render(&mut out, |v| v);
        assert_eq!(out, [0.3, -0.3, 0.4, -0.4, 0.5, -0.5]);
    }

    #[test]
    fn new_audio_session_discards_old_samples_and_mixed_output_is_clamped() {
        let mut mixer = AudioMixer::new(1, 3, 2);
        mixer.push(0, 10, &[0.1, 0.2, 0.3]);
        assert_eq!(mixer.push(0, 12, &[0.8, -0.8]), 3);
        mixer.push(1, 13, &[0.8, -0.8]);
        let mut out = [0.0; 3];
        mixer.render(&mut out, |v| v);
        assert_eq!(out, [1.0, -1.0, 0.0]);
    }

    #[test]
    fn chunking_a_burst_preserves_order_and_the_partial_tail() {
        let input: Vec<f32> = (0..20003).map(|v| v as f32).collect();
        let mut chunker = InterleavedAudioFrameChunker::new(48000, 2, 20).unwrap();
        let mut result: Vec<f32> = chunker.push(&input).into_iter().flatten().collect();
        result.extend(chunker.push(&vec![-1.0; 1920 - 803]).into_iter().flatten());
        assert_eq!(&result[..input.len()], input);
        assert!(result[input.len()..].iter().all(|v| *v == -1.0));
        assert_eq!(chunker.buffered_samples(), 0);
    }

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
