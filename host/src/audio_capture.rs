use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use protocol::AudioSource;
use remote_core::{AudioCapturer, AudioFrame};
use screencapturekit::prelude::*;
use std::error::Error;
use tokio::sync::mpsc;

pub struct MacAudioFrame {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub source: AudioSource,
}

impl AudioFrame for MacAudioFrame {
    fn samples(&self) -> &[f32] {
        &self.samples
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn source(&self) -> AudioSource {
        self.source
    }
}

pub struct MacAudioCapturer {
    rx: mpsc::Receiver<MacAudioFrame>,
    stream: Option<cpal::Stream>,
    tx: mpsc::Sender<MacAudioFrame>,
    source: AudioSource,
}

impl MacAudioCapturer {
    pub fn new() -> Self {
        Self::microphone()
    }

    pub fn microphone() -> Self {
        let (tx, rx) = mpsc::channel(100);
        Self {
            rx,
            stream: None,
            tx,
            source: AudioSource::RemoteMicrophone,
        }
    }
}

#[async_trait]
impl AudioCapturer for MacAudioCapturer {
    type Frame = MacAudioFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let host = cpal::default_host();
        // Fallback to default input device
        let device = host
            .default_input_device()
            .ok_or("No input device available")?;

        let default_config = device.default_input_config()?;
        let mut config: cpal::StreamConfig = default_config.clone().into();
        config.sample_rate = 48000;
        let sample_rate = config.sample_rate;
        let channels = config.channels;

        println!(
            "Audio Capturer starting on device: {} ({}Hz, {} channels)",
            device.description()?.name(),
            sample_rate,
            channels
        );

        let tx = self.tx.clone();
        let source = self.source;

        let err_fn = |err| eprintln!("an error occurred on stream: {}", err);

        // default_config is needed to check sample format
        let stream = match default_config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _: &_| {
                    let frame = MacAudioFrame {
                        samples: data.to_vec(),
                        sample_rate,
                        channels,
                        source,
                    };
                    let _ = tx.blocking_send(frame);
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _: &_| {
                    let samples: Vec<f32> =
                        data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    let frame = MacAudioFrame {
                        samples,
                        sample_rate,
                        channels,
                        source,
                    };
                    let _ = tx.blocking_send(frame);
                },
                err_fn,
                None,
            )?,
            _ => return Err("Unsupported sample format".into()),
        };

        stream.play()?;
        self.stream = Some(stream);

        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if let Some(stream) = self.stream.take() {
            stream.pause()?;
        }
        Ok(())
    }

    async fn capture_frame(&mut self) -> Result<Self::Frame, Box<dyn Error + Send + Sync>> {
        if let Some(frame) = self.rx.recv().await {
            Ok(frame)
        } else {
            Err("Audio capture channel closed".into())
        }
    }
}

impl Default for MacAudioCapturer {
    fn default() -> Self {
        Self::new()
    }
}

pub struct MacSystemAudioCapturer {
    rx: mpsc::Receiver<MacAudioFrame>,
    stream: Option<SCStream>,
    tx_output: Option<mpsc::Sender<MacAudioFrame>>,
}

impl MacSystemAudioCapturer {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(100);
        Self {
            rx,
            stream: None,
            tx_output: Some(tx),
        }
    }
}

impl Default for MacSystemAudioCapturer {
    fn default() -> Self {
        Self::new()
    }
}

struct SystemAudioOutput {
    tx: mpsc::Sender<MacAudioFrame>,
}

impl SCStreamOutputTrait for SystemAudioOutput {
    fn did_output_sample_buffer(
        &self,
        sample: screencapturekit::cm::CMSampleBuffer,
        of_type: SCStreamOutputType,
    ) {
        if of_type != SCStreamOutputType::Audio {
            return;
        }

        if let Some(frame) = sample_buffer_to_audio_frame(&sample, AudioSource::RemoteSystem) {
            let _ = self.tx.try_send(frame);
        }
    }
}

#[async_trait]
impl AudioCapturer for MacSystemAudioCapturer {
    type Frame = MacAudioFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let content = SCShareableContent::get()?;
        let display = content
            .displays()
            .into_iter()
            .next()
            .ok_or("No display found for system audio capture")?;
        let filter = SCContentFilter::create().with_display(&display).build();

        let mut config = SCStreamConfiguration::new();
        config.set_width(2);
        config.set_height(2);
        config.set_captures_audio(true);
        config.set_excludes_current_process_audio(true);
        config.set_sample_rate(48_000);
        config.set_channel_count(2);

        let output = SystemAudioOutput {
            tx: self.tx_output.take().unwrap(),
        };
        let mut stream = SCStream::new(&filter, &config);
        stream.add_output_handler(output, SCStreamOutputType::Audio);
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
        if let Some(frame) = self.rx.recv().await {
            Ok(frame)
        } else {
            Err("System audio capture channel closed".into())
        }
    }
}

impl Drop for MacSystemAudioCapturer {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.stop_capture();
        }
    }
}

fn sample_buffer_to_audio_frame(
    sample: &screencapturekit::cm::CMSampleBuffer,
    source: AudioSource,
) -> Option<MacAudioFrame> {
    if !sample.is_valid() {
        return None;
    }
    if !sample.is_data_ready() && sample.make_data_ready().is_err() {
        return None;
    }

    let format = sample.format_description()?;
    let sample_rate = format.audio_sample_rate().unwrap_or(48_000.0).round() as u32;
    let channels = u16::try_from(format.audio_channel_count().unwrap_or(2)).ok()?;
    if channels == 0 {
        return None;
    }

    let list = sample.audio_buffer_list()?;
    let bits_per_channel = format.audio_bits_per_channel().unwrap_or(32);
    let samples = if format.audio_is_float() && bits_per_channel == 32 {
        decode_audio_buffer_list_f32(&list, usize::from(channels), format.audio_is_big_endian())
    } else if !format.audio_is_float() && bits_per_channel == 16 {
        decode_audio_buffer_list_i16(&list, usize::from(channels), format.audio_is_big_endian())
    } else {
        return None;
    };

    if samples.is_empty() {
        return None;
    }

    Some(MacAudioFrame {
        samples,
        sample_rate,
        channels,
        source,
    })
}

fn decode_audio_buffer_list_f32(
    list: &screencapturekit::cm::AudioBufferList,
    channels: usize,
    big_endian: bool,
) -> Vec<f32> {
    decode_audio_buffer_list(list, channels, |bytes| {
        pcm_f32_bytes_to_samples(bytes, big_endian)
    })
}

fn decode_audio_buffer_list_i16(
    list: &screencapturekit::cm::AudioBufferList,
    channels: usize,
    big_endian: bool,
) -> Vec<f32> {
    decode_audio_buffer_list(list, channels, |bytes| {
        pcm_i16_bytes_to_samples(bytes, big_endian)
    })
}

fn decode_audio_buffer_list(
    list: &screencapturekit::cm::AudioBufferList,
    channels: usize,
    decode: impl Fn(&[u8]) -> Vec<f32>,
) -> Vec<f32> {
    if channels == 0 || list.num_buffers() == 0 {
        return Vec::new();
    }

    if list.num_buffers() == 1 {
        let Some(buffer) = list.get(0) else {
            return Vec::new();
        };
        return decode(buffer.data());
    }

    if list.num_buffers() != channels {
        return Vec::new();
    }

    let planar = list
        .iter()
        .map(|buffer| decode(buffer.data()))
        .collect::<Vec<_>>();
    interleave_planar_channels(&planar)
}

fn pcm_f32_bytes_to_samples(bytes: &[u8], big_endian: bool) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| {
            let raw = [chunk[0], chunk[1], chunk[2], chunk[3]];
            if big_endian {
                f32::from_be_bytes(raw)
            } else {
                f32::from_le_bytes(raw)
            }
        })
        .collect()
}

fn pcm_i16_bytes_to_samples(bytes: &[u8], big_endian: bool) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|chunk| {
            let raw = [chunk[0], chunk[1]];
            let sample = if big_endian {
                i16::from_be_bytes(raw)
            } else {
                i16::from_le_bytes(raw)
            };
            sample as f32 / i16::MAX as f32
        })
        .collect()
}

fn interleave_planar_channels(planar: &[Vec<f32>]) -> Vec<f32> {
    let Some(frame_count) = planar.iter().map(Vec::len).min() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(frame_count * planar.len());
    for frame_index in 0..frame_count {
        for channel in planar {
            out.push(channel[frame_index]);
        }
    }
    out
}

impl Drop for MacAudioCapturer {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.pause();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_capturer_declares_remote_microphone_source() {
        let capturer = MacAudioCapturer::microphone();

        assert_eq!(capturer.source, AudioSource::RemoteMicrophone);
    }

    #[test]
    fn mac_audio_frame_reports_capture_source() {
        let frame = MacAudioFrame {
            samples: vec![0.0],
            sample_rate: 48_000,
            channels: 1,
            source: AudioSource::RemoteMicrophone,
        };

        assert_eq!(frame.source(), AudioSource::RemoteMicrophone);
    }

    #[test]
    fn decodes_little_endian_f32_pcm_samples() {
        let bytes = [0.25_f32.to_le_bytes(), (-0.5_f32).to_le_bytes()].concat();

        assert_eq!(pcm_f32_bytes_to_samples(&bytes, false), vec![0.25, -0.5]);
    }

    #[test]
    fn decodes_big_endian_i16_pcm_samples() {
        let bytes = [i16::MAX.to_be_bytes(), i16::MIN.to_be_bytes()].concat();
        let samples = pcm_i16_bytes_to_samples(&bytes, true);

        assert_eq!(samples[0], 1.0);
        assert!(samples[1] <= -1.0);
    }

    #[test]
    fn interleaves_planar_audio_channels() {
        let planar = vec![vec![1.0, 2.0], vec![10.0, 20.0]];

        assert_eq!(
            interleave_planar_channels(&planar),
            vec![1.0, 10.0, 2.0, 20.0]
        );
    }
}
