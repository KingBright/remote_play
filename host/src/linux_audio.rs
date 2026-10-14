use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use protocol::AudioSource;
use remote_core::{AudioCapturer, AudioFrame};
use std::error::Error;
use tokio::sync::mpsc;

#[derive(Debug)]
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
    errors: mpsc::Receiver<String>,
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
    fn replace_channels(&mut self) -> (mpsc::Sender<LinuxAudioFrame>, mpsc::Sender<String>) {
        let (tx, rx) = mpsc::channel(100);
        let (error_tx, errors) = mpsc::channel(1);
        self.rx = rx;
        self.errors = errors;
        (tx, error_tx)
    }
    pub fn microphone() -> Self {
        let (_, rx) = mpsc::channel(100);
        let (_, errors) = mpsc::channel(1);
        Self {
            rx,
            stream: None,
            errors,
            source: AudioSource::RemoteMicrophone,
        }
    }
}

#[async_trait]
impl AudioCapturer for LinuxAudioCapturer {
    type Frame = LinuxAudioFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.stream.is_some() {
            return Err("Audio capture is already running".into());
        }
        // Each native stream owns fresh queues; late callbacks from a retired
        // endpoint cannot corrupt a replacement stream or lose errors in PCM.
        let (tx, error_tx) = self.replace_channels();
        let host = cpal::default_host();
        let system = self.source == AudioSource::RemoteSystem;
        let device = if system {
            host.default_output_device()
        } else {
            host.default_input_device()
        }
        .ok_or("No audio endpoint available")?;
        let default_config = if system {
            device.default_output_config()?
        } else {
            device.default_input_config()?
        };
        let mut config: cpal::StreamConfig = default_config.clone().into();
        config.sample_rate = 48000;
        // Explicit Opus-compatible loopback format; unsupported endpoint formats
        // fail instead of mislabelling samples or capturing the microphone.
        if system {
            config.channels = 2;
        }
        let sample_rate = config.sample_rate;
        let channels = config.channels;

        let source = self.source;
        let err_fn = move |err: cpal::StreamError| {
            let _ = error_tx.try_send(err.to_string());
        };

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
        while self.rx.try_recv().is_ok() {}
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
        tokio::select! {
            biased;
            error = self.errors.recv() => Err(error.unwrap_or_else(|| "Native audio callback ended".into()).into()),
            frame = self.rx.recv() => frame.ok_or_else(|| "Native audio stream closed".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame() -> LinuxAudioFrame {
        LinuxAudioFrame {
            samples: vec![0.; 2],
            sample_rate: 48000,
            channels: 2,
            source: AudioSource::RemoteSystem,
        }
    }
    #[tokio::test]
    async fn native_error_is_not_lost_behind_a_full_pcm_queue() {
        let mut capture = LinuxAudioCapturer::microphone();
        let (pcm, errors) = capture.replace_channels();
        for _ in 0..100 {
            pcm.try_send(frame()).unwrap();
        }
        errors.try_send("endpoint removed".into()).unwrap();
        assert_eq!(
            capture.capture_frame().await.err().unwrap().to_string(),
            "endpoint removed"
        );
    }
    #[tokio::test]
    async fn retired_native_callbacks_cannot_fail_a_replacement_stream() {
        let mut capture = LinuxAudioCapturer::microphone();
        let (old_pcm, old_errors) = capture.replace_channels();
        let (pcm, _errors) = capture.replace_channels();
        assert!(old_errors.try_send("late device error".into()).is_err());
        assert!(old_pcm.try_send(frame()).is_err());
        pcm.send(frame()).await.unwrap();
        assert_eq!(
            capture.capture_frame().await.unwrap().source,
            AudioSource::RemoteSystem
        );
    }
}
