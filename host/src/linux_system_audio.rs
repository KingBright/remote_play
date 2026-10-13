//! Native PipeWire sink monitor. This deliberately opens no microphone endpoint.
//! It is separate from portal video capture: a display's request.audio flag is
//! the system-audio request; a window-only share never enters this worker.
use crate::linux_audio::LinuxAudioFrame;
use async_trait::async_trait;
use remote_core::AudioCapturer;
use std::{
    error::Error,
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;
type CaptureError = Box<dyn Error + Send + Sync>;

pub struct SystemAudioCapturer {
    rx: mpsc::Receiver<LinuxAudioFrame>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    resources: Arc<crate::lifecycle::Resources>,
}
impl SystemAudioCapturer {
    pub(crate) fn new(resources: Arc<crate::lifecycle::Resources>) -> Self {
        let (_, rx) = mpsc::channel(8);
        Self {
            rx,
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            resources,
        }
    }
    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}
#[async_trait]
impl AudioCapturer for SystemAudioCapturer {
    type Frame = LinuxAudioFrame;
    async fn start(&mut self) -> Result<(), CaptureError> {
        if self.worker.is_some() {
            return Err("System audio worker is already running".into());
        }
        self.stop = Arc::new(AtomicBool::new(false));
        let stop = self.stop.clone();
        let (tx, rx) = mpsc::channel(8);
        self.rx = rx;
        // Retain the connection's worker count until all PipeWire objects have
        // dropped, even if the async owner is aborted during shutdown.
        let resource_guard = crate::lifecycle::WorkerGuard::new(self.resources.clone());
        self.worker = Some(
            std::thread::Builder::new()
                .name("rp-system-audio".into())
                .spawn(move || {
                    let _resource_guard = resource_guard;
                    if let Err(error) = run(tx, stop) {
                        eprintln!("PipeWire system audio: {error}");
                    }
                })?,
        );
        Ok(())
    }
    async fn stop(&mut self) -> Result<(), CaptureError> {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            tokio::task::spawn_blocking(move || worker.join())
                .await?
                .map_err(|_| "System audio worker panicked")?;
        }
        while self.rx.try_recv().is_ok() {}
        Ok(())
    }
    async fn pause(&mut self) -> Result<(), CaptureError> {
        self.stop().await
    }
    async fn resume(&mut self) -> Result<(), CaptureError> {
        self.start().await
    }
    async fn capture_frame(&mut self) -> Result<Self::Frame, CaptureError> {
        // Bound missing/silent-device waits; the owner also races cancellation.
        tokio::time::timeout(Duration::from_secs(10), self.rx.recv())
            .await?
            .ok_or_else(|| "System audio stream closed".into())
    }
}
impl Drop for SystemAudioCapturer {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            // Native objects remain on their owning thread until its 20ms timer
            // sees stop, including task cancellation before the first sample.
            let _ = std::thread::Builder::new()
                .name("rp-audio-reap".into())
                .spawn(move || {
                    let _ = worker.join();
                });
        }
    }
}

fn run(tx: mpsc::Sender<LinuxAudioFrame>, stop: Arc<AtomicBool>) -> Result<(), Box<dyn Error>> {
    use pipewire as pw;
    use pw::{properties::properties, spa};
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(pw::init);
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let error_loop = mainloop.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |_, _, _, _| error_loop.quit())
        .register();
    let stream = pw::stream::StreamBox::new(
        &core,
        "RemotePlay system audio",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio", *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen", "media.class" => "Stream/Input/Audio",
            "stream.capture.sink" => "true", "node.name" => "remoteplay-system-audio",
            "audio.position" => "[ FL FR ]",
        },
    )?;
    struct Data {
        tx: mpsc::Sender<LinuxAudioFrame>,
        valid: bool,
        loop_: pw::main_loop::MainLoopRc,
    }
    let _listener = stream
        .add_local_listener_with_user_data(Data {
            tx,
            valid: false,
            loop_: mainloop.clone(),
        })
        .state_changed(|_, data, _, state| {
            if matches!(
                state,
                pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected
            ) {
                data.loop_.quit();
            }
        })
        .param_changed(|_, data, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            data.valid = false;
            let Some(param) = param else {
                return;
            };
            let mut info = spa::param::audio::AudioInfoRaw::default();
            if info.parse(param).is_err()
                || info.format() != spa::param::audio::AudioFormat::F32LE
                || info.rate() != 48_000
                || info.channels() != 2
            {
                data.loop_.quit();
                return;
            }
            data.valid = true;
        })
        .process(|stream, data| {
            if !data.valid {
                return;
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let planes = buffer.datas_mut();
            if planes.len() != 1 {
                return;
            }
            let plane = &mut planes[0];
            let raw = plane.as_raw();
            if raw.data.is_null() || raw.chunk.is_null() || raw.maxsize > 1024 * 1024 {
                return;
            }
            let chunk = plane.chunk();
            let offset = chunk.offset() as usize;
            let size = chunk.size() as usize;
            if chunk.flags().bits() & 0b11 != 0
                || size == 0
                || size % 8 != 0
                || offset
                    .checked_add(size)
                    .is_none_or(|end| end > raw.maxsize as usize)
            {
                return;
            }
            let bytes =
                unsafe { std::slice::from_raw_parts(raw.data.cast::<u8>().add(offset), size) };
            let samples = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            let _ = data.tx.try_send(LinuxAudioFrame {
                samples,
                sample_rate: 48_000,
                channels: 2,
                source: protocol::AudioSource::RemoteSystem,
            });
        })
        .register()?;
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Audio
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::AudioFormat,
            Id,
            spa::param::audio::AudioFormat::F32LE
        ),
        spa::pod::property!(spa::param::format::FormatProperties::AudioRate, Int, 48_000),
        spa::pod::property!(spa::param::format::FormatProperties::AudioChannels, Int, 2)
    );
    let bytes = spa::pod::serialize::PodSerializer::serialize(
        Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )?
    .0
    .into_inner();
    let pod = spa::pod::Pod::from_bytes(&bytes).ok_or("Invalid audio format pod")?;
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut [pod],
    )?;
    let timer_loop = mainloop.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop.load(Ordering::Acquire) {
            timer_loop.quit();
        }
    });
    timer
        .update_timer(
            Some(Duration::from_millis(1)),
            Some(Duration::from_millis(20)),
        )
        .into_result()?;
    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}
