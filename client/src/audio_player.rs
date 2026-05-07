use crate::jitter_buffer::JitterBuffer;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus::Decoder;
use protocol::RtpPacket;
use std::error::Error;
use tokio::sync::mpsc;

pub struct AudioPlayer {
    _stream: cpal::Stream,
}

impl AudioPlayer {
    pub fn new(mut rx: mpsc::Receiver<RtpPacket>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("No output device available")?;

        let default_config = device.default_output_config()?;
        let config: cpal::StreamConfig = default_config.clone().into();

        let mut decoder = Decoder::new(48000, opus::Channels::Stereo)?;

        let (sample_tx, sample_rx) = crossbeam_channel::unbounded::<f32>();

        // Tokio task to decode RTP packets and push to sample_tx
        tokio::spawn(async move {
            let mut jitter_buffer = JitterBuffer::new(0);
            let mut expected_seq_init = false;

            while let Some(packet) = rx.recv().await {
                if !expected_seq_init {
                    jitter_buffer =
                        crate::jitter_buffer::JitterBuffer::new(packet.header.sequence_number);
                    expected_seq_init = true;
                }
                jitter_buffer.push(packet);

                while let Some(ordered) = jitter_buffer.pop() {
                    let mut decoded = vec![0.0f32; 5760 * 2]; // Max 120ms at 48kHz stereo
                    match decoder.decode_float(&ordered.payload, &mut decoded, false) {
                        Ok(len) => {
                            // len is samples per channel. Total samples = len * 2
                            for i in 0..(len * 2) {
                                let _ = sample_tx.send(decoded[i]);
                            }
                        }
                        Err(e) => eprintln!("Opus decode error: {}", e),
                    }
                }
            }
        });

        let err_fn = |err| eprintln!("an error occurred on audio output stream: {}", err);

        let stream = match default_config.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    for sample in data.iter_mut() {
                        *sample = sample_rx.try_recv().unwrap_or(0.0);
                    }
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config,
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

        Ok(Self { _stream: stream })
    }
}
