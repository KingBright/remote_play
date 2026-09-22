use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus::Decoder;
use protocol::{AudioCodec, AudioDirection, AudioSource, AudioStreamConfig, RtpPacket};
use remote_core::audio::AudioMixer;
use remote_core::jitter_buffer::JitterBuffer;
use std::collections::HashMap;
use std::error::Error;
use tokio::sync::mpsc;

pub struct AudioPlayer {
    _stream: Option<cpal::Stream>,
}

#[derive(Debug)]
pub enum AudioPlayerEvent {
    StreamConfig(AudioStreamConfig),
    Settings(AudioPlayerSettings),
    Packet(RtpPacket),
    StreamVolume {
        stream_id: u32,
        volume_percent: u8,
        muted: bool,
    },
    RemoveStream(u32),
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
        {
            let host = cpal::default_host();
            let device = host
                .default_output_device()
                .ok_or("No output device available")?;

            let default_config = select_output_config(&device)?;
            let output_config: cpal::StreamConfig = default_config.clone().into();
            let output_channels = usize::from(output_config.channels.max(1));
            let output_rate = output_config.sample_rate;

            let (sample_tx, sample_rx) = crossbeam_channel::bounded::<PlaybackChunk>(32);
            let mut mixer = AudioMixer::new(output_channels, output_rate as usize * 60 / 1000, 16);

            let err_fn = |err| eprintln!("an error occurred on audio output stream: {}", err);

            let stream = match default_config.sample_format() {
                cpal::SampleFormat::F32 => device.build_output_stream(
                    &output_config,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        fill_audio_buffer(data, &sample_rx, &mut mixer, |s| s);
                    },
                    err_fn,
                    None,
                )?,
                cpal::SampleFormat::I16 => device.build_output_stream(
                    &output_config,
                    move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                        fill_audio_buffer(data, &sample_rx, &mut mixer, |s| {
                            (s * i16::MAX as f32) as i16
                        });
                    },
                    err_fn,
                    None,
                )?,
                _ => return Err("Unsupported sample format".into()),
            };

            stream.play()?;

            tokio::spawn(async move {
                let mut streams = HashMap::<u32, AudioDecodeStream>::new();
                let mut volumes = HashMap::<u32, f32>::new();
                let mut settings = AudioPlayerSettings::default();

                loop {
                    let wait = streams
                        .values()
                        .filter_map(|s| s.jitter_buffer.next_ready_in())
                        .min();
                    let event = tokio::select! {
                        event = rx.recv() => {
                            let Some(event) = event else { break };
                            event
                        }
                        _ = async {
                            match wait {
                                Some(delay) => tokio::time::sleep(delay).await,
                                None => std::future::pending().await,
                            }
                        } => {
                            for stream in streams.values_mut() {
                                stream.drain(output_channels, &sample_tx, settings);
                            }
                            continue;
                        }
                    };
                    match event {
                        AudioPlayerEvent::StreamVolume {
                            stream_id,
                            volume_percent,
                            muted,
                        } => {
                            let gain = if muted {
                                0.0
                            } else {
                                f32::from(volume_percent.min(200)) / 100.0
                            };
                            if volumes.len() < 16 || volumes.contains_key(&stream_id) {
                                volumes.insert(stream_id, gain);
                            }
                            if let Some(stream) = streams.get_mut(&stream_id) {
                                stream.local_gain = if muted {
                                    0.0
                                } else {
                                    f32::from(volume_percent.min(200)) / 100.0
                                };
                                let _ = sample_tx.try_send(PlaybackChunk {
                                    stream_id,
                                    lane: stream.lane,
                                    samples: Vec::new(),
                                });
                            }
                        }
                        AudioPlayerEvent::RemoveStream(stream_id) => {
                            volumes.remove(&stream_id);
                            if let Some(stream) = streams.remove(&stream_id) {
                                let _ = sample_tx.try_send(PlaybackChunk {
                                    stream_id,
                                    lane: stream.lane,
                                    samples: Vec::new(),
                                });
                            }
                        }
                        AudioPlayerEvent::Settings(new_settings) => {
                            settings = new_settings;
                        }
                        AudioPlayerEvent::StreamConfig(config) => {
                            if config.direction != AudioDirection::HostToClient {
                                continue;
                            }
                            if streams
                                .get(&config.stream_id)
                                .is_some_and(|s| s.config == config)
                            {
                                continue;
                            }
                            match AudioDecodeStream::new(config.clone(), output_rate) {
                                Ok(mut stream) => {
                                    let lane = streams
                                        .get(&config.stream_id)
                                        .map(|s| s.lane)
                                        .or_else(|| {
                                            (0..16).find(|lane| {
                                                streams.values().all(|s| s.lane != *lane)
                                            })
                                        });
                                    let Some(lane) = lane else {
                                        continue;
                                    };
                                    stream.lane = lane;
                                    stream.local_gain =
                                        volumes.get(&config.stream_id).copied().unwrap_or(1.0);
                                    streams.insert(config.stream_id, stream);
                                }
                                Err(err) => {
                                    eprintln!("Ignoring unsupported audio stream config: {}", err);
                                }
                            }
                        }
                        AudioPlayerEvent::Packet(packet) => {
                            let stream_id = packet.header.ssrc;
                            let next_lane =
                                (0..16).find(|lane| streams.values().all(|s| s.lane != *lane));
                            if let std::collections::hash_map::Entry::Vacant(entry) =
                                streams.entry(stream_id)
                            {
                                let Some(lane) = next_lane else {
                                    continue;
                                };
                                let config = legacy_audio_stream_config(stream_id);
                                match AudioDecodeStream::new(config, output_rate) {
                                    Ok(mut stream) => {
                                        stream.lane = lane;
                                        entry.insert(stream);
                                    }
                                    Err(err) => {
                                        eprintln!(
                                            "Failed to initialize legacy audio decoder: {}",
                                            err
                                        );
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

            Ok(Self {
                _stream: Some(stream),
            })
        }
    }
}

struct AudioDecodeStream {
    lane: usize,
    local_gain: f32,
    config: AudioStreamConfig,
    decoder: Decoder,
    jitter_buffer: JitterBuffer,
    expected_seq_init: bool,
    decode_buffer: Vec<f32>,
}

impl AudioDecodeStream {
    fn new(
        config: AudioStreamConfig,
        output_rate: u32,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        if config.codec != AudioCodec::Opus {
            return Err("unsupported audio codec".into());
        }

        // Opus can decode directly at a supported device rate regardless of the
        // encoder's rate, avoiding both pitch errors and an extra resampler.
        let decoder = Decoder::new(output_rate, opus_channels(config.channels)?)?;
        let decode_buffer =
            vec![0.0; output_rate as usize * 120 / 1000 * usize::from(config.channels)];
        Ok(Self {
            lane: playback_lane(config.source),
            local_gain: 1.0,
            config,
            decoder,
            jitter_buffer: JitterBuffer::new(0),
            expected_seq_init: false,
            decode_buffer,
        })
    }

    fn push_packet(
        &mut self,
        packet: RtpPacket,
        output_channels: usize,
        sample_tx: &crossbeam_channel::Sender<PlaybackChunk>,
        settings: AudioPlayerSettings,
    ) {
        if !self.expected_seq_init {
            self.jitter_buffer = JitterBuffer::new(packet.header.sequence_number);
            self.expected_seq_init = true;
        }
        self.jitter_buffer.push(packet);
        self.drain(output_channels, sample_tx, settings);
    }

    fn drain(
        &mut self,
        output_channels: usize,
        sample_tx: &crossbeam_channel::Sender<PlaybackChunk>,
        settings: AudioPlayerSettings,
    ) {
        while let Some(ordered) = self.jitter_buffer.pop() {
            let source_channels = usize::from(self.config.channels);
            match self
                .decoder
                .decode_float(&ordered.payload, &mut self.decode_buffer, false)
            {
                Ok(samples_per_channel) => {
                    let gain = settings.gain_for_source(self.config.source) * self.local_gain;
                    if gain == 0.0 {
                        continue;
                    }
                    let mut chunk = Vec::with_capacity(samples_per_channel * output_channels);
                    for_each_output_sample(
                        &self.decode_buffer,
                        samples_per_channel,
                        source_channels,
                        output_channels,
                        |sample| chunk.push(sample * gain),
                    );
                    let _ = sample_tx.try_send(PlaybackChunk {
                        stream_id: self.config.stream_id,
                        lane: self.lane,
                        samples: chunk,
                    });
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

struct PlaybackChunk {
    stream_id: u32,
    lane: usize,
    samples: Vec<f32>,
}

fn playback_lane(source: AudioSource) -> usize {
    usize::from(source == AudioSource::RemoteMicrophone)
}

fn select_output_config(
    device: &cpal::Device,
) -> Result<cpal::SupportedStreamConfig, Box<dyn Error + Send + Sync>> {
    let default = device.default_output_config()?;
    if matches!(default.sample_rate(), 8000 | 12000 | 16000 | 24000 | 48000)
        && matches!(
            default.sample_format(),
            cpal::SampleFormat::F32 | cpal::SampleFormat::I16
        )
    {
        return Ok(default);
    }
    let configs: Vec<_> = device.supported_output_configs()?.collect();
    for rate in [48000, 24000, 16000, 12000, 8000] {
        if let Some(config) = configs.iter().find(|c| {
            c.channels() == default.channels()
                && c.min_sample_rate() <= rate
                && c.max_sample_rate() >= rate
                && matches!(
                    c.sample_format(),
                    cpal::SampleFormat::F32 | cpal::SampleFormat::I16
                )
        }) {
            return Ok((*config).with_sample_rate(rate));
        }
    }
    Err("Audio output has no Opus-compatible sample rate; a platform resampler is required".into())
}

fn fill_audio_buffer<T>(
    data: &mut [T],
    sample_rx: &crossbeam_channel::Receiver<PlaybackChunk>,
    mixer: &mut AudioMixer,
    map: impl Fn(f32) -> T,
) {
    // Limit callback work even if the producer is continuously feeding it.
    for _ in 0..8 {
        let Ok(chunk) = sample_rx.try_recv() else {
            break;
        };
        if chunk.samples.is_empty() {
            mixer.clear_stream(chunk.stream_id);
        } else {
            mixer.push(chunk.lane, chunk.stream_id, &chunk.samples);
        }
    }
    mixer.render(data, map);
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
    fn opus_decodes_at_the_output_device_rate_and_sources_mix_in_one_callback() {
        let mut encoder =
            opus::Encoder::new(24000, opus::Channels::Mono, opus::Application::LowDelay).unwrap();
        let mut encoded = vec![0; 4000];
        let size = encoder
            .encode_float(&vec![0.25; 480], &mut encoded)
            .unwrap();
        encoded.truncate(size);
        let config = AudioStreamConfig::remote_microphone(7, 24000, 1, 20);
        let mut decoder = AudioDecodeStream::new(config, 48000).unwrap();
        let (tx, rx) = crossbeam_channel::bounded(4);
        decoder.push_packet(
            RtpPacket {
                header: protocol::RtpHeader {
                    version: 2,
                    payload_type: 97,
                    sequence_number: 0,
                    timestamp: 0,
                    ssrc: 7,
                },
                payload: encoded,
            },
            2,
            &tx,
            AudioPlayerSettings::default(),
        );
        let chunk = rx.try_recv().unwrap();
        assert_eq!(chunk.samples.len(), 960 * 2); // 20 ms at 48 kHz, stereo.
        assert_eq!(chunk.lane, 1);
        tx.send(PlaybackChunk {
            stream_id: 8,
            lane: 0,
            samples: vec![0.2; 1920],
        })
        .unwrap();
        tx.send(PlaybackChunk {
            stream_id: 7,
            lane: 1,
            samples: vec![0.3; 1920],
        })
        .unwrap();
        let mut mixer = AudioMixer::new(2, 2880, 2);
        let mut out = [0.0; 1920];
        fill_audio_buffer(&mut out, &rx, &mut mixer, |v| v);
        assert!(out.iter().all(|v| (*v - 0.5).abs() < 1e-6));
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
