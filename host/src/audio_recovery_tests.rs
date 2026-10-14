use super::*;
use crate::linux_audio::LinuxAudioFrame;
use remote_core::net::{MultiplexedPacket, UdpMultiplexer, UdpReceiver};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

struct Capture {
    starts: Arc<AtomicUsize>,
    fail_stop: bool,
    frames: tokio::sync::mpsc::Receiver<Result<LinuxAudioFrame, &'static str>>,
}
#[async_trait::async_trait]
impl AudioCapturer for Capture {
    type Frame = LinuxAudioFrame;
    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.starts.fetch_add(1, SeqCst);
        Ok(())
    }
    async fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.fail_stop {
            Err("native stop unconfirmed".into())
        } else {
            Ok(())
        }
    }
    async fn pause(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        Ok(())
    }
    async fn resume(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        Ok(())
    }
    async fn capture_frame(&mut self) -> Result<Self::Frame, Box<dyn Error + Send + Sync>> {
        self.frames
            .recv()
            .await
            .unwrap_or(Err("test input closed"))
            .map_err(Into::into)
    }
}
fn frame() -> LinuxAudioFrame {
    LinuxAudioFrame {
        samples: vec![0.1; 1920],
        sample_rate: 48000,
        channels: 2,
        source: AudioSource::RemoteSystem,
    }
}
fn settings(paused: bool) -> StreamSettings {
    StreamSettings {
        width: 1280,
        height: 720,
        fps: 30,
        bitrate_kbps: 4000,
        paused,
    }
}
async fn data(rx: &UdpReceiver) -> DataEnvelope {
    loop {
        match tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            MultiplexedPacket::Data(d, _) | MultiplexedPacket::DataWithTiming(d, _, _) => return d,
            _ => {}
        }
    }
}
async fn wait_starts(starts: &AtomicUsize, n: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while starts.load(SeqCst) < n {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn lost_initial_config_is_repeated_and_recovery_preserves_stream_sequence() {
    let source = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
    let target = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
    let (udp, _) = source.split();
    let (_, rx) = target.split();
    let starts = Arc::new(AtomicUsize::new(0));
    let (frames, frame_rx) = tokio::sync::mpsc::channel(8);
    let (cancel, cancel_rx) = broadcast::channel(4);
    let (paused, settings_rx) = watch::channel(settings(false));
    let resources = Arc::new(crate::lifecycle::Resources::default());
    let task = spawn_audio_capture_task(
        "test",
        Capture {
            fail_stop: false,
            starts: starts.clone(),
            frames: frame_rx,
        },
        AudioCaptureTaskRuntime {
            stream_id: 77,
            udp_sender: udp,
            stats: Statistics::new(),
            cancel_rx,
            client_addr: target.local_addr().unwrap(),
            use_data_plane_media: true,
            scheduled_sender: None,
            media_settings_rx: Some(settings_rx),
            resources: resources.clone(),
        },
    );
    wait_starts(&starts, 1).await;
    frames.send(Ok(frame())).await.unwrap();
    // Deliberately discard the first best-effort configuration packet.
    let first = data(&rx).await;
    let original = remote_core::media_plane::audio_stream_config_from_envelope(&first).unwrap();
    assert_eq!(original.source, AudioSource::RemoteSystem);
    let packet = remote_core::media_plane::realtime_data_to_rtp(data(&rx).await).unwrap();
    assert_eq!((packet.header.ssrc, packet.header.sequence_number), (77, 0));
    let repeated =
        remote_core::media_plane::audio_stream_config_from_envelope(&data(&rx).await).unwrap();
    assert_eq!(repeated, original);
    assert_eq!(starts.load(SeqCst), 1, "quiet capture must not restart");

    frames.send(Err("endpoint disappeared")).await.unwrap();
    wait_starts(&starts, 2).await;
    frames.send(Ok(frame())).await.unwrap();
    let config =
        remote_core::media_plane::audio_stream_config_from_envelope(&data(&rx).await).unwrap();
    assert_eq!(config, original);
    let packet = remote_core::media_plane::realtime_data_to_rtp(data(&rx).await).unwrap();
    assert_eq!((packet.header.ssrc, packet.header.sequence_number), (77, 1));

    // A failure while paused must neither reopen the device nor lose cancellation.
    paused.send_replace(settings(true));
    frames.send(Err("removed while paused")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(starts.load(SeqCst), 2);
    cancel.send(()).unwrap();
    tokio::time::timeout(Duration::from_millis(200), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resources.workers.load(SeqCst), 0);
}

#[tokio::test]
async fn cancellation_during_audio_restart_backoff_does_not_reopen_capture() {
    let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
    let (udp, _) = mux.split();
    let starts = Arc::new(AtomicUsize::new(0));
    let (frames, frame_rx) = tokio::sync::mpsc::channel(1);
    let (cancel, cancel_rx) = broadcast::channel(2);
    let (_settings, settings_rx) = watch::channel(settings(false));
    let task = spawn_audio_capture_task(
        "test",
        Capture {
            fail_stop: false,
            starts: starts.clone(),
            frames: frame_rx,
        },
        AudioCaptureTaskRuntime {
            stream_id: 77,
            udp_sender: udp,
            stats: Statistics::new(),
            cancel_rx,
            client_addr: mux.local_addr().unwrap(),
            use_data_plane_media: true,
            scheduled_sender: None,
            media_settings_rx: Some(settings_rx),
            resources: Arc::default(),
        },
    );
    wait_starts(&starts, 1).await;
    frames
        .send(Err("native error before first PCM"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel.send(()).unwrap();
    tokio::time::timeout(Duration::from_millis(200), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(starts.load(SeqCst), 1);
}

#[tokio::test]
async fn pause_before_first_frame_never_reopens_after_unconfirmed_native_stop() {
    let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
    let (udp, _) = mux.split();
    let starts = Arc::new(AtomicUsize::new(0));
    let (_frames, frame_rx) = tokio::sync::mpsc::channel(1);
    let (_cancel, cancel_rx) = broadcast::channel(2);
    let (paused, settings_rx) = watch::channel(settings(false));
    let task = spawn_audio_capture_task(
        "test",
        Capture {
            fail_stop: true,
            starts: starts.clone(),
            frames: frame_rx,
        },
        AudioCaptureTaskRuntime {
            stream_id: 77,
            udp_sender: udp,
            stats: Statistics::new(),
            cancel_rx,
            client_addr: mux.local_addr().unwrap(),
            use_data_plane_media: true,
            scheduled_sender: None,
            media_settings_rx: Some(settings_rx),
            resources: Arc::default(),
        },
    );
    wait_starts(&starts, 1).await;
    paused.send_replace(settings(true));
    tokio::time::timeout(Duration::from_millis(200), task)
        .await
        .unwrap()
        .unwrap();
    paused.send_replace(settings(false));
    assert_eq!(starts.load(SeqCst), 1);
}
