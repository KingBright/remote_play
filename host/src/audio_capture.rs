use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use remote_core::{AudioCapturer, AudioFrame};
use std::error::Error;
use tokio::sync::mpsc;

pub struct MacAudioFrame {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
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
}

pub struct MacAudioCapturer {
    rx: mpsc::Receiver<MacAudioFrame>,
    stream: Option<cpal::Stream>,
    tx: mpsc::Sender<MacAudioFrame>,
}

impl MacAudioCapturer {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(100);
        Self {
            rx,
            stream: None,
            tx,
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
            device.name()?,
            sample_rate,
            channels
        );

        let tx = self.tx.clone();

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

impl Drop for MacAudioCapturer {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.pause();
        }
    }
}
