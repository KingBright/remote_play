use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus::Decoder;
use protocol::{
    AudioCodec, AudioDirection, AudioSource, AudioStreamConfig, ContentKind, DataEnvelope,
    RtpPacket, viewer_talkback_audio_stream_id,
};
use remote_core::jitter_buffer::JitterBuffer;
use remote_core::media_plane::{audio_stream_config_from_envelope, realtime_data_to_rtp};
use std::collections::HashMap;
use std::error::Error;
use std::sync::mpsc;
use tokio::sync::{mpsc as tokio_mpsc, watch};

const TALKBACK_SAMPLE_QUEUE: usize = 96_000;

pub struct TalkbackPlayer {
    _stream: cpal::Stream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TalkbackPlaybackSettings {
    pub muted: bool,
    pub volume_percent: u8,
}

impl Default for TalkbackPlaybackSettings {
    fn default() -> Self {
        Self {
            muted: false,
            volume_percent: 100,
        }
    }
}

impl TalkbackPlaybackSettings {
    fn gain(self) -> f32 {
        if self.muted {
            0.0
        } else {
            f32::from(self.volume_percent.min(200)) / 100.0
        }
    }
}

impl TalkbackPlayer {
    pub fn new(
        session_id: u32,
        mut rx: tokio_mpsc::Receiver<DataEnvelope>,
        mut settings_rx: watch::Receiver<TalkbackPlaybackSettings>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("No output device available for talkback")?;

        let default_config = device.default_output_config()?;
        let output_config: cpal::StreamConfig = default_config.clone().into();
        let output_channels = usize::from(output_config.channels.max(1));
        let (sample_tx, sample_rx) = mpsc::sync_channel::<f32>(TALKBACK_SAMPLE_QUEUE);

        tokio::spawn(async move {
            let mut streams = HashMap::<u32, AudioDecodeStream>::new();
            let mut settings = *settings_rx.borrow();

            loop {
                tokio::select! {
                    settings_res = settings_rx.changed() => {
                        if settings_res.is_err() {
                            break;
                        }
                        settings = *settings_rx.borrow();
                    }
                    maybe_envelope = rx.recv() => {
                        let Some(envelope) = maybe_envelope else {
                            break;
                        };
                        match envelope.header.kind {
                            ContentKind::AudioStreamConfig => {
                                let config = match audio_stream_config_from_envelope(&envelope) {
                                    Ok(config) => config,
                                    Err(err) => {
                                        eprintln!("Ignoring invalid talkback audio config: {}", err);
                                        continue;
                                    }
                                };
                                if !is_viewer_talkback_config(&config, session_id) {
                                    continue;
                                }
                                match AudioDecodeStream::new(config.clone()) {
                                    Ok(stream) => {
                                        streams.insert(config.stream_id, stream);
                                        println!(
                                            "Viewer talkback playback configured: stream={} {}Hz {}ch",
                                            config.stream_id, config.sample_rate_hz, config.channels
                                        );
                                    }
                                    Err(err) => {
                                        eprintln!("Ignoring unsupported talkback stream config: {}", err);
                                    }
                                }
                            }
                            ContentKind::AudioOpus => {
                                let packet = match realtime_data_to_rtp(envelope) {
                                    Ok(packet) => packet,
                                    Err(err) => {
                                        eprintln!("Ignoring invalid talkback audio packet: {}", err);
                                        continue;
                                    }
                                };
                                if packet.header.ssrc != viewer_talkback_audio_stream_id(session_id) {
                                    continue;
                                }
                                if let Some(stream) = streams.get_mut(&packet.header.ssrc) {
                                    stream.push_packet(packet, output_channels, &sample_tx, settings);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        let err_fn = |err| eprintln!("talkback output stream error: {}", err);

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
                        *sample = f32_to_i16(sample_rx.try_recv().unwrap_or(0.0));
                    }
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::U16 => device.build_output_stream(
                &output_config,
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                    for sample in data.iter_mut() {
                        *sample = f32_to_u16(sample_rx.try_recv().unwrap_or(0.0));
                    }
                },
                err_fn,
                None,
            )?,
            sample_format => {
                return Err(
                    format!("unsupported talkback output sample format {sample_format:?}").into(),
                );
            }
        };

        stream.play()?;
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
        sample_tx: &mpsc::SyncSender<f32>,
        settings: TalkbackPlaybackSettings,
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
                    let gain = settings.gain();
                    if gain == 0.0 {
                        continue;
                    }
                    for_each_output_sample(
                        &decoded,
                        samples_per_channel,
                        source_channels,
                        output_channels,
                        |sample| {
                            let _ = sample_tx.try_send(sample * gain);
                        },
                    );
                }
                Err(err) => eprintln!("Talkback Opus decode error: {}", err),
            }
        }
    }
}

fn is_viewer_talkback_config(config: &AudioStreamConfig, session_id: u32) -> bool {
    config.stream_id == viewer_talkback_audio_stream_id(session_id)
        && config.source == AudioSource::ViewerMicrophoneTalkback
        && config.direction == AudioDirection::ClientToHost
        && config.codec == AudioCodec::Opus
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

fn f32_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

fn f32_to_u16(sample: f32) -> u16 {
    ((sample.clamp(-1.0, 1.0) + 1.0) * 0.5 * u16::MAX as f32) as u16
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
    fn recognizes_only_viewer_talkback_for_session() {
        let session_id = 50;
        let stream_id = viewer_talkback_audio_stream_id(session_id);
        let config = AudioStreamConfig::viewer_microphone_talkback(stream_id, 48_000, 1, 20);

        assert!(is_viewer_talkback_config(&config, session_id));
        assert!(!is_viewer_talkback_config(
            &AudioStreamConfig::remote_microphone(stream_id, 48_000, 1, 20),
            session_id
        ));
        assert!(!is_viewer_talkback_config(
            &AudioStreamConfig::viewer_microphone_talkback(stream_id + 1, 48_000, 1, 20),
            session_id
        ));
    }

    #[test]
    fn talkback_mono_duplicates_to_stereo_output() {
        assert_eq!(remap(&[0.25, -0.5], 2, 1, 2), vec![0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn talkback_stereo_downmixes_to_mono_output() {
        assert_eq!(remap(&[0.25, 0.75, -0.5, 0.25], 2, 2, 1), vec![0.5, -0.125]);
    }

    #[test]
    fn output_sample_conversions_are_clamped() {
        assert_eq!(f32_to_i16(2.0), i16::MAX);
        assert_eq!(f32_to_i16(-2.0), -i16::MAX);
        assert_eq!(f32_to_u16(2.0), u16::MAX);
        assert_eq!(f32_to_u16(-2.0), 0);
    }

    #[test]
    fn talkback_playback_settings_gate_volume() {
        assert_eq!(
            TalkbackPlaybackSettings {
                muted: true,
                volume_percent: 100,
            }
            .gain(),
            0.0
        );
        assert_eq!(
            TalkbackPlaybackSettings {
                muted: false,
                volume_percent: 150,
            }
            .gain(),
            1.5
        );
        assert_eq!(
            TalkbackPlaybackSettings {
                muted: false,
                volume_percent: 255,
            }
            .gain(),
            2.0
        );
    }

    #[test]
    fn talkback_decoder_outputs_samples_from_realtime_opus_packet() {
        let stream_id = viewer_talkback_audio_stream_id(50);
        let config = AudioStreamConfig::viewer_microphone_talkback(stream_id, 48_000, 1, 20);
        let mut decode_stream = AudioDecodeStream::new(config).unwrap();
        let mut encoder =
            opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::LowDelay).unwrap();
        let samples = vec![0.0f32; 960];
        let mut payload = vec![0u8; 4_000];
        let size = encoder.encode_float(&samples, &mut payload).unwrap();
        payload.truncate(size);

        let packet = RtpPacket {
            header: protocol::RtpHeader {
                version: 2,
                payload_type: protocol::PayloadType::AudioOpus as u8,
                sequence_number: 1,
                timestamp: 960,
                ssrc: stream_id,
            },
            payload,
        };
        let packet = remote_core::media_plane::realtime_data_to_rtp(
            remote_core::media_plane::rtp_to_realtime_data(&packet).unwrap(),
        )
        .unwrap();
        let (sample_tx, sample_rx) = mpsc::sync_channel(2_000);

        decode_stream.push_packet(packet, 1, &sample_tx, TalkbackPlaybackSettings::default());

        assert!(
            sample_rx.try_iter().count() > 0,
            "decoded talkback packet should emit playback samples"
        );
    }
}
