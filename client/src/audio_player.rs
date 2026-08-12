use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus::Decoder;
use protocol::{AudioCodec, AudioDirection, AudioSource, AudioStreamConfig, RtpPacket};
use remote_core::jitter_buffer::JitterBuffer;
use std::collections::HashMap;
use std::error::Error;
use tokio::sync::mpsc;

pub struct AudioPlayer {
    _stream: cpal::Stream,
}

#[derive(Debug)]
pub enum AudioPlayerEvent {
    StreamConfig(AudioStreamConfig),
    Settings(AudioPlayerSettings),
    Packet(RtpPacket),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlayerSettings {
    pub remote_system_muted: bool,
    pub remote_microphone_muted: bool,
    pub volume_percent: u8,
}

impl Default for AudioPlayerSettings {
    fn default() -> Self {
        Self {
            remote_system_muted: false,
            remote_microphone_muted: false,
            volume_percent: 100,
        }
    }
}

impl AudioPlayerSettings {
    fn gain_for_source(self, source: AudioSource) -> f32 {
        match source {
            AudioSource::RemoteSystem | AudioSource::RemoteMixed if self.remote_system_muted => 0.0,
            AudioSource::RemoteMicrophone if self.remote_microphone_muted => 0.0,
            _ => f32::from(self.volume_percent.min(200)) / 100.0,
        }
    }
}

impl AudioPlayer {
    pub fn new(
        mut rx: mpsc::Receiver<AudioPlayerEvent>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("No output device available")?;

        let default_config = device.default_output_config()?;
        let output_config: cpal::StreamConfig = default_config.clone().into();
        let output_channels = usize::from(output_config.channels.max(1));

        let (sample_tx, sample_rx) = crossbeam_channel::unbounded::<f32>();

        let err_fn = |err| eprintln!("an error occurred on audio output stream: {}", err);

        let stream = match default_config.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &output_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    for sample in data.iter_mut() {
                        *sample = sample_rx.try_recv().unwrap_or(0.0);
                    }
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_output_stream(
                &output_config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    for sample in data.iter_mut() {
                        let f = sample_rx.try_recv().unwrap_or(0.0);
                        let i = (f * i16::MAX as f32) as i16;
                        *sample = i;
                    }
                },
                err_fn,
                None,
            )?,
            _ => return Err("Unsupported sample format".into()),
        };

        stream.play()?;

        tokio::spawn(async move {
            let mut streams = HashMap::<u32, AudioDecodeStream>::new();
            let mut settings = AudioPlayerSettings::default();

            while let Some(event) = rx.recv().await {
                match event {
                    AudioPlayerEvent::Settings(new_settings) => {
                        settings = new_settings;
                    }
                    AudioPlayerEvent::StreamConfig(config) => {
                        if config.direction != AudioDirection::HostToClient {
                            continue;
                        }
                        match AudioDecodeStream::new(config.clone()) {
                            Ok(stream) => {
                                streams.insert(config.stream_id, stream);
                            }
                            Err(err) => {
                                eprintln!("Ignoring unsupported audio stream config: {}", err);
                            }
                        }
                    }
                    AudioPlayerEvent::Packet(packet) => {
                        let stream_id = packet.header.ssrc;
                        if let std::collections::hash_map::Entry::Vacant(entry) =
                            streams.entry(stream_id)
                        {
                            let config = legacy_audio_stream_config(stream_id);
                            match AudioDecodeStream::new(config) {
                                Ok(stream) => {
                                    entry.insert(stream);
                                }
                                Err(err) => {
                                    eprintln!("Failed to initialize legacy audio decoder: {}", err);
                                    continue;
                                }
                            }
                        }

                        if let Some(stream) = streams.get_mut(&stream_id) {
                            stream.push_packet(packet, output_channels, &sample_tx, settings);
                        }
                    }
                }
            }
        });

        Ok(Self { _stream: stream })
    }
}

struct AudioDecodeStream {
    config: AudioStreamConfig,
    decoder: Decoder,
    jitter_buffer: JitterBuffer,
    expected_seq_init: bool,
}

impl AudioDecodeStream {
    fn new(config: AudioStreamConfig) -> Result<Self, Box<dyn Error + Send + Sync>> {
        if config.codec != AudioCodec::Opus {
            return Err("unsupported audio codec".into());
        }

        let decoder = Decoder::new(config.sample_rate_hz, opus_channels(config.channels)?)?;
        Ok(Self {
            config,
            decoder,
            jitter_buffer: JitterBuffer::new(0),
            expected_seq_init: false,
        })
    }

    fn push_packet(
        &mut self,
        packet: RtpPacket,
        output_channels: usize,
        sample_tx: &crossbeam_channel::Sender<f32>,
        settings: AudioPlayerSettings,
    ) {
        if !self.expected_seq_init {
            self.jitter_buffer = JitterBuffer::new(packet.header.sequence_number);
            self.expected_seq_init = true;
        }
        self.jitter_buffer.push(packet);

        while let Some(ordered) = self.jitter_buffer.pop() {
            let source_channels = usize::from(self.config.channels);
            let mut decoded = vec![0.0f32; 5760 * source_channels];
            match self
                .decoder
                .decode_float(&ordered.payload, &mut decoded, false)
            {
                Ok(samples_per_channel) => {
                    let gain = settings.gain_for_source(self.config.source);
                    if gain == 0.0 {
                        continue;
                    }
                    for_each_output_sample(
                        &decoded,
                        samples_per_channel,
                        source_channels,
                        output_channels,
                        |sample| {
                            let _ = sample_tx.send(sample * gain);
                        },
                    );
                }
                Err(e) => eprintln!("Opus decode error: {}", e),
            }
        }
    }
}

fn legacy_audio_stream_config(stream_id: u32) -> AudioStreamConfig {
    AudioStreamConfig::remote_microphone(stream_id, 48_000, 2, 20)
}

fn opus_channels(channels: u16) -> Result<opus::Channels, Box<dyn Error + Send + Sync>> {
    match channels {
        1 => Ok(opus::Channels::Mono),
        2 => Ok(opus::Channels::Stereo),
        other => Err(format!("unsupported Opus channel count {other}").into()),
    }
}

fn for_each_output_sample(
    decoded: &[f32],
    samples_per_channel: usize,
    source_channels: usize,
    output_channels: usize,
    mut emit: impl FnMut(f32),
) {
    if source_channels == 0 || output_channels == 0 {
        return;
    }

    let available_frames = decoded.len() / source_channels;
    let frames = samples_per_channel.min(available_frames);
    for frame_index in 0..frames {
        let input_offset = frame_index * source_channels;
        for output_channel in 0..output_channels {
            let sample = if source_channels == output_channels {
                decoded[input_offset + output_channel]
            } else if source_channels == 1 {
                decoded[input_offset]
            } else if output_channels == 1 {
                let sum: f32 = decoded[input_offset..input_offset + source_channels]
                    .iter()
                    .sum();
                sum / source_channels as f32
            } else {
                let source_channel = output_channel.min(source_channels - 1);
                decoded[input_offset + source_channel]
            };
            emit(sample);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remap(
        decoded: &[f32],
        samples_per_channel: usize,
        source_channels: usize,
        output_channels: usize,
    ) -> Vec<f32> {
        let mut out = Vec::new();
        for_each_output_sample(
            decoded,
            samples_per_channel,
            source_channels,
            output_channels,
            |sample| out.push(sample),
        );
        out
    }

    #[test]
    fn mono_audio_duplicates_to_stereo_output() {
        assert_eq!(remap(&[0.25, -0.5], 2, 1, 2), vec![0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn stereo_audio_downmixes_to_mono_output() {
        assert_eq!(remap(&[0.25, 0.75, -0.5, 0.25], 2, 2, 1), vec![0.5, -0.125]);
    }

    #[test]
    fn matching_channel_counts_pass_through() {
        assert_eq!(
            remap(&[0.1, 0.2, 0.3, 0.4], 2, 2, 2),
            vec![0.1, 0.2, 0.3, 0.4]
        );
    }

    #[test]
    fn unsupported_opus_channel_counts_are_rejected() {
        let err = match opus_channels(6) {
            Ok(_) => panic!("surround stream needs explicit downmix"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("unsupported Opus channel count"));
    }

    #[test]
    fn playback_settings_gate_sources_and_volume() {
        let settings = AudioPlayerSettings {
            remote_system_muted: true,
            remote_microphone_muted: false,
            volume_percent: 150,
        };

        assert_eq!(settings.gain_for_source(AudioSource::RemoteSystem), 0.0);
        assert_eq!(settings.gain_for_source(AudioSource::RemoteMixed), 0.0);
        assert_eq!(settings.gain_for_source(AudioSource::RemoteMicrophone), 1.5);

        let settings = AudioPlayerSettings {
            remote_system_muted: false,
            remote_microphone_muted: true,
            volume_percent: 255,
        };

        assert_eq!(settings.gain_for_source(AudioSource::RemoteMicrophone), 0.0);
        assert_eq!(settings.gain_for_source(AudioSource::RemoteSystem), 2.0);
    }
}
