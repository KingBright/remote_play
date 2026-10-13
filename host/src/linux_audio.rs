use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use protocol::AudioSource;
use remote_core::{AudioCapturer, AudioFrame};
use std::error::Error;
use tokio::sync::mpsc;

pub struct LinuxAudioFrame {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub source: AudioSource,
}

impl AudioFrame for LinuxAudioFrame {
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

pub struct LinuxAudioCapturer {
    rx: mpsc::Receiver<LinuxAudioFrame>,
    stream: Option<cpal::Stream>,
    tx: mpsc::Sender<LinuxAudioFrame>,
    source: AudioSource,
}

impl LinuxAudioCapturer {
    /// CPAL's WASAPI backend enables AUDCLNT_STREAMFLAGS_LOOPBACK when an
    /// output endpoint is opened as an input stream. Never use an input device.
    #[cfg(target_os = "windows")]
    pub fn system() -> Self {
        let mut capturer = Self::microphone();
        capturer.source = AudioSource::RemoteSystem;
        capturer
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
impl AudioCapturer for LinuxAudioCapturer {
    type Frame = LinuxAudioFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let host = cpal::default_host();
        let system = self.source == AudioSource::RemoteSystem;
        let device = if system { host.default_output_device() } else { host.default_input_device() }
            .ok_or("No audio endpoint available")?;
        let default_config = if system { device.default_output_config()? } else { device.default_input_config()? };
        let mut config: cpal::StreamConfig = default_config.clone().into();
        config.sample_rate = 48000;
        // Explicit Opus-compatible loopback format; unsupported endpoint formats
        // fail instead of mislabelling samples or capturing the microphone.
        if system { config.channels = 2; }
        let sample_rate = config.sample_rate;
        let channels = config.channels;

        let tx = self.tx.clone();
        let source = self.source;
        let err_fn = |err| eprintln!("[Audio] stream error: {err}");

        let stream = match default_config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _: &_| {
                    let frame = LinuxAudioFrame {
                        samples: data.to_vec(),
                        sample_rate,
                        channels,
                        source,
                    };
                    let _ = tx.try_send(frame);
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _: &_| {
                    let samples: Vec<f32> =
                        data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    let frame = LinuxAudioFrame {
                        samples,
                        sample_rate,
                        channels,
                        source,
                    };
                    let _ = tx.try_send(frame);
                },
                err_fn,
                None,
            )?,
            _ => return Err("Unsupported Linux audio sample format".into()),
        };

        stream.play()?;
        self.stream = Some(stream);
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.stream = None;
        Ok(())
    }

    async fn pause(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if let Some(stream) = &self.stream {
            stream.pause()?;
        }
        while self.rx.try_recv().is_ok() {}
        Ok(())
    }

    async fn resume(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        while self.rx.try_recv().is_ok() {}
        if let Some(stream) = &self.stream {
            stream.play()?;
        } else {
            self.start().await?;
        }
        Ok(())
    }

    async fn capture_frame(&mut self) -> Result<Self::Frame, Box<dyn Error + Send + Sync>> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| "Linux audio stream closed".into())
    }
}
