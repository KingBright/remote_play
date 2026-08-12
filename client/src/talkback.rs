use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus::{Application, Channels, Encoder};
use protocol::{
    AudioStreamConfig, PayloadType, RtpHeader, RtpPacket, viewer_talkback_audio_stream_id,
};
use remote_core::audio::InterleavedAudioFrameChunker;
use remote_core::media_plane::{audio_stream_config_to_envelope, rtp_to_realtime_data};
use remote_core::net::UdpSender;
use remote_core::scheduled_sender::{
    ScheduledDataSendError, ScheduledDataSender, ScheduledDataSenderConfig,
};
use std::error::Error;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, watch};

const OPUS_FRAME_DURATION_MS: u32 = 20;
const TALKBACK_INPUT_QUEUE: usize = 32;

pub struct TalkbackRuntimeConfig {
    pub udp_sender: UdpSender,
    pub target: SocketAddr,
    pub session_id: u32,
    pub cancel_rx: broadcast::Receiver<()>,
    pub settings_rx: watch::Receiver<TalkbackCaptureSettings>,
}

struct TalkbackAudioFrame {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TalkbackCaptureSettings {
    pub local_mic_muted: bool,
    pub push_to_talk: bool,
    pub push_to_talk_active: bool,
}

impl Default for TalkbackCaptureSettings {
    fn default() -> Self {
        Self {
            local_mic_muted: false,
            push_to_talk: false,
            push_to_talk_active: true,
        }
    }
}

impl TalkbackCaptureSettings {
    fn should_transmit(self) -> bool {
        !self.local_mic_muted && (!self.push_to_talk || self.push_to_talk_active)
    }
}

pub async fn run_talkback_capture(
    config: TalkbackRuntimeConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let TalkbackRuntimeConfig {
        udp_sender,
        target,
        session_id,
        mut cancel_rx,
        mut settings_rx,
    } = config;

    let (frame_tx, mut frame_rx) = mpsc::channel(TALKBACK_INPUT_QUEUE);
    let (stream, sample_rate, channels) = build_input_stream(frame_tx)?;
    let mut encoder = TalkbackOpusEncoder::new(sample_rate, channels)?;
    let stream_id = viewer_talkback_audio_stream_id(session_id);
    let audio_stream_config = AudioStreamConfig::viewer_microphone_talkback(
        stream_id,
        sample_rate,
        channels,
        OPUS_FRAME_DURATION_MS as u16,
    );
    let (scheduled_sender, _scheduled_worker) = ScheduledDataSender::spawn(
        udp_sender,
        target,
        ScheduledDataSenderConfig {
            queue_capacity: 128,
            send_budget_per_tick: 64,
            tick_interval: Duration::from_millis(1),
            ..ScheduledDataSenderConfig::default()
        },
    );
    let mut config_sequence_number = 0;
    send_audio_stream_config(
        &scheduled_sender,
        &audio_stream_config,
        &mut config_sequence_number,
    )
    .await?;

    stream.play()?;
    println!(
        "Viewer microphone talkback started: stream={} {}Hz {}ch",
        stream_id, sample_rate, channels
    );

    let mut sequence_number: u16 = 0;
    let timestamp_step =
        u32::try_from(encoder.samples_per_packet_per_channel()).unwrap_or(sample_rate / 50);
    let mut config_interval = tokio::time::interval(Duration::from_secs(1));
    config_interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel_rx.recv() => {
                break;
            }
            _ = config_interval.tick() => {
                send_audio_stream_config(
                    &scheduled_sender,
                    &audio_stream_config,
                    &mut config_sequence_number,
                )
                .await?;
            }
            settings_res = settings_rx.changed() => {
                if settings_res.is_err() {
                    break;
                }
            }
            maybe_frame = frame_rx.recv() => {
                let Some(frame) = maybe_frame else {
                    break;
                };
                if !settings_rx.borrow().should_transmit() {
                    continue;
                }
                if frame.sample_rate != sample_rate || frame.channels != channels {
                    eprintln!(
                        "Ignoring talkback frame with changed layout: {}Hz {}ch",
                        frame.sample_rate, frame.channels
                    );
                    continue;
                }
                for packet in encoder.encode_samples(&frame.samples)? {
                    send_encoded_packet(
                        &scheduled_sender,
                        stream_id,
                        timestamp_step,
                        &mut sequence_number,
                        packet,
                    )?;
                }
            }
        }
    }

    let _ = stream.pause();
    println!("Viewer microphone talkback stopped.");
    Ok(())
}

fn build_input_stream(
    frame_tx: mpsc::Sender<TalkbackAudioFrame>,
) -> Result<(cpal::Stream, u32, u16), Box<dyn Error + Send + Sync>> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("No input device available for talkback")?;
    let default_config = device.default_input_config()?;
    let mut stream_config: cpal::StreamConfig = default_config.clone().into();
    stream_config.sample_rate = 48_000;
    stream_config.channels = stream_config.channels.clamp(1, 2);

    let sample_rate = stream_config.sample_rate;
    let channels = stream_config.channels;
    let device_name = device
        .description()
        .map(|description| description.name().to_string())
        .unwrap_or_else(|_| "default input".to_string());

    println!(
        "Talkback input starting on device: {} ({}Hz, {}ch)",
        device_name, sample_rate, channels
    );

    let err_fn = |err| eprintln!("talkback input stream error: {}", err);

    let stream = match default_config.sample_format() {
        cpal::SampleFormat::F32 => {
            let tx = frame_tx.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let _ = tx.try_send(TalkbackAudioFrame {
                        samples: data.to_vec(),
                        sample_rate,
                        channels,
                    });
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let tx = frame_tx.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let samples = data.iter().map(|sample| i16_to_f32(*sample)).collect();
                    let _ = tx.try_send(TalkbackAudioFrame {
                        samples,
                        sample_rate,
                        channels,
                    });
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let tx = frame_tx;
            device.build_input_stream(
                &stream_config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    let samples = data.iter().map(|sample| u16_to_f32(*sample)).collect();
                    let _ = tx.try_send(TalkbackAudioFrame {
                        samples,
                        sample_rate,
                        channels,
                    });
                },
                err_fn,
                None,
            )?
        }
        sample_format => {
            return Err(
                format!("unsupported talkback input sample format {sample_format:?}").into(),
            );
        }
    };

    Ok((stream, sample_rate, channels))
}

async fn send_audio_stream_config(
    scheduled_sender: &ScheduledDataSender,
    config: &AudioStreamConfig,
    sequence_number: &mut u64,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let envelope = audio_stream_config_to_envelope(config, *sequence_number, now_ms())?;
    *sequence_number = (*sequence_number).wrapping_add(1);
    scheduled_sender.send(envelope).await?;
    Ok(())
}

fn send_encoded_packet(
    scheduled_sender: &ScheduledDataSender,
    stream_id: u32,
    timestamp_step: u32,
    sequence_number: &mut u16,
    payload: Vec<u8>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let packet = RtpPacket {
        header: RtpHeader {
            version: 2,
            payload_type: PayloadType::AudioOpus as u8,
            sequence_number: *sequence_number,
            timestamp: (*sequence_number as u32).wrapping_mul(timestamp_step),
            ssrc: stream_id,
        },
        payload,
    };
    *sequence_number = (*sequence_number).wrapping_add(1);

    let envelope = rtp_to_realtime_data(&packet)?;
    match scheduled_sender.try_send(envelope) {
        Ok(()) | Err(ScheduledDataSendError::Full) => Ok(()),
        Err(err @ ScheduledDataSendError::Closed) => Err(Box::new(err)),
    }
}

struct TalkbackOpusEncoder {
    encoder: Encoder,
    chunker: InterleavedAudioFrameChunker,
}

impl TalkbackOpusEncoder {
    fn new(sample_rate: u32, channels: u16) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let opus_channels = match channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            other => return Err(format!("unsupported talkback channel count {other}").into()),
        };
        let encoder = Encoder::new(sample_rate, opus_channels, Application::LowDelay)?;
        let chunker =
            InterleavedAudioFrameChunker::new(sample_rate, channels, OPUS_FRAME_DURATION_MS)?;
        Ok(Self { encoder, chunker })
    }

    fn samples_per_packet_per_channel(&self) -> usize {
        self.chunker.samples_per_channel_per_frame()
    }

    fn encode_samples(
        &mut self,
        samples: &[f32],
    ) -> Result<Vec<Vec<u8>>, Box<dyn Error + Send + Sync>> {
        let mut packets = Vec::new();
        for chunk in self.chunker.push(samples) {
            let mut output = vec![0u8; 4_000];
            let size = self.encoder.encode_float(&chunk, &mut output)?;
            output.truncate(size);
            if !output.is_empty() {
                packets.push(output);
            }
        }
        Ok(packets)
    }
}

fn i16_to_f32(sample: i16) -> f32 {
    sample as f32 / i16::MAX as f32
}

fn u16_to_f32(sample: u16) -> f32 {
    (sample as f32 / u16::MAX as f32) * 2.0 - 1.0
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_pcm_is_centered_around_zero() {
        assert!((u16_to_f32(0) + 1.0).abs() < 0.0001);
        assert!(u16_to_f32(u16::MAX / 2).abs() < 0.0001);
        assert!((u16_to_f32(u16::MAX) - 1.0).abs() < 0.0001);
    }

    #[test]
    fn signed_pcm_uses_full_scale() {
        assert!((i16_to_f32(i16::MAX) - 1.0).abs() < 0.0001);
        assert!(i16_to_f32(i16::MIN) <= -1.0);
    }

    #[test]
    fn talkback_settings_gate_transmission() {
        assert!(TalkbackCaptureSettings::default().should_transmit());
        assert!(
            !TalkbackCaptureSettings {
                local_mic_muted: true,
                ..TalkbackCaptureSettings::default()
            }
            .should_transmit()
        );
        assert!(
            !TalkbackCaptureSettings {
                push_to_talk: true,
                push_to_talk_active: false,
                ..TalkbackCaptureSettings::default()
            }
            .should_transmit()
        );
        assert!(
            TalkbackCaptureSettings {
                push_to_talk: true,
                push_to_talk_active: true,
                ..TalkbackCaptureSettings::default()
            }
            .should_transmit()
        );
    }

    #[test]
    fn talkback_encoder_waits_for_complete_opus_frame() {
        let mut encoder = TalkbackOpusEncoder::new(48_000, 1).unwrap();

        assert!(encoder.encode_samples(&vec![0.0; 480]).unwrap().is_empty());

        let packets = encoder.encode_samples(&vec![0.0; 480]).unwrap();
        assert_eq!(packets.len(), 1);
        assert!(!packets[0].is_empty());
    }

    #[test]
    fn talkback_rejects_unsupported_channel_counts() {
        let err = match TalkbackOpusEncoder::new(48_000, 6) {
            Ok(_) => panic!("surround input needs explicit downmix"),
            Err(err) => err,
        };

        assert!(
            err.to_string()
                .contains("unsupported talkback channel count"),
            "unexpected error: {err}"
        );
    }
}
