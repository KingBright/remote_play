use crate::{
    StreamingRunConfig, SubscriptionAudioConfig, env_flag_enabled, run_streaming,
    start_subscription_audio,
};
use protocol::session::{
    CaptureSource, MAX_SUBSCRIPTIONS, SESSION_VERSION, SessionCommand, SubscriptionRequest,
};
use protocol::{AudioControlTarget, ContentKind, ControlMessage, DataEnvelope, InputEvent};
use remote_core::net::{MultiplexedPacket, UdpMultiplexer, UdpSender};
use remote_core::scheduled_sender::{ScheduledDataSender, ScheduledDataSenderConfig};
use remote_core::session_crypto::{
    SessionCrypto, load_session_psk, mac_session_accept, mac_session_hello, now_unix_ms,
    random_bytes_16, require_session_auth, verify_session_mac,
};
use remote_core::{InputInjector, stats::Statistics};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;

#[derive(Clone)]
pub struct HostServiceConfig {
    pub bind_addr: SocketAddr,
    pub stats: Arc<Statistics>,
    pub enable_clipboard_sync: bool,
    pub enable_file_transfer: bool,
    pub enable_talkback: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum AudioGroupKey {
    System,
    Application(i32),
}

#[cfg(target_os = "windows")]
static WINDOWS_VIDEO_CAPTURE_RESERVED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
struct WindowsVideoCaptureReservation;

#[cfg(target_os = "windows")]
impl WindowsVideoCaptureReservation {
    fn acquire() -> Result<Self, String> {
        WINDOWS_VIDEO_CAPTURE_RESERVED
            .compare_exchange(false, true, Relaxed, Relaxed)
            .map(|_| Self)
            .map_err(|_| {
                "Windows capture is already in use. This safe build allows one video capture session at a time."
                    .to_string()
            })
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsVideoCaptureReservation {
    fn drop(&mut self) {
        WINDOWS_VIDEO_CAPTURE_RESERVED.store(false, Relaxed);
    }
}

fn validate_host_video_settings(
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
) -> Result<(), &'static str> {
    protocol::validate_video_settings(width, height, fps, bitrate_kbps)?;
    #[cfg(target_os = "windows")]
    {
        const MAX_PIXELS: u64 = 2560 * 1440;
        if u64::from(width) * u64::from(height) > MAX_PIXELS || fps > 60 || bitrate_kbps > 20_000 {
            return Err("Windows safe capture budget is 2560x1440 pixels, 60 fps, and 20000 kbps");
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputTarget {
    Desktop,
    Window { id: u32, pid: i32 },
}
fn input_target_for(
    source: CaptureSource,
    pid: Option<i32>,
    supported: bool,
) -> Option<InputTarget> {
    if !supported {
        return None;
    }
    match source {
        CaptureSource::MainDisplay | CaptureSource::Display(_) => Some(InputTarget::Desktop),
        CaptureSource::Window(id) => pid
            .filter(|pid| *pid > 0)
            .map(|pid| InputTarget::Window { id, pid }),
    }
}

struct Subscription {
    settings_revision: u64,
    request: SubscriptionRequest,
    settings: watch::Sender<crate::StreamSettings>,
    revision: u64,
    keyframe: Arc<AtomicBool>,
    cancel: broadcast::Sender<()>,
    task: tokio::task::JoinHandle<()>,
    audio_group: Option<AudioGroupKey>,
    audio_active: bool,
    supports_input: bool,
    input_target: Option<InputTarget>,
    last_input_error: Option<Instant>,
    source_revision: u32,
    video_sequence: Arc<std::sync::atomic::AtomicU16>,
    #[cfg(target_os = "windows")]
    _capture_reservation: Arc<WindowsVideoCaptureReservation>,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        self.task.abort();
    }
}
struct AudioGroup {
    owner: u32,
    settings: watch::Sender<crate::StreamSettings>,
    cancel: broadcast::Sender<()>,
    _tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for AudioGroup {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        for task in &self._tasks {
            task.abort();
        }
    }
}
struct Connection {
    id: u32,
    last_seen: Instant,
    cancel: broadcast::Sender<()>,
    sender: ScheduledDataSender,
    clipboard: Option<mpsc::Sender<DataEnvelope>>,
    clipboard_cancel: Option<broadcast::Sender<()>>,
    clipboard_enabled: Arc<std::sync::atomic::AtomicBool>,
    files: Option<mpsc::Sender<DataEnvelope>>,
    talkback: Option<mpsc::Sender<DataEnvelope>>,
    #[cfg(target_os = "macos")]
    talkback_settings: Option<watch::Sender<crate::talkback_player::TalkbackPlaybackSettings>>,
    subscriptions: HashMap<u32, Subscription>,
    retired: HashSet<u32>,
    audio_groups: HashMap<AudioGroupKey, AudioGroup>,
    _reporter: crate::ScheduledStatsReporterGuard,
}
impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(cancel) = self.clipboard_cancel.take() {
            let _ = cancel.send(());
        }
        let _ = self.cancel.send(());
    }
}

fn stream_settings(request: &SubscriptionRequest) -> crate::StreamSettings {
    crate::StreamSettings {
        width: request.width,
        height: request.height,
        fps: request.fps,
        bitrate_kbps: request.bitrate_kbps,
        paused: false,
    }
}

impl Connection {
    fn new(id: u32, addr: SocketAddr, udp: UdpSender, config: &HostServiceConfig) -> Self {
        let (cancel, cancel_rx) = broadcast::channel(8);
        let (sender, _worker) =
            ScheduledDataSender::spawn(udp, addr, ScheduledDataSenderConfig::default());
        let (files, file_inbound_rx) = channel_if(
            config.enable_file_transfer
                || env_flag_enabled("REMOTE_PLAY_FILE_CLIPBOARD")
                || std::env::var_os("REMOTE_PLAY_HOST_SEND_FILE").is_some(),
        );
        let (talkback, talkback_inbound_rx) =
            channel_if(config.enable_talkback && cfg!(target_os = "macos"));
        #[cfg(target_os = "macos")]
        let (talkback_settings, talkback_settings_rx) = if talkback.is_some() {
            let (tx, rx) =
                watch::channel(crate::talkback_player::TalkbackPlaybackSettings::default());
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        #[cfg(not(target_os = "macos"))]
        let talkback_settings_rx = None;
        let reporter = crate::start_scheduled_sender_reporter(sender.clone(), cancel.subscribe());
        let clipboard_enabled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        tokio::spawn(crate::connection_services::run(
            crate::connection_services::ConnectionServicesConfig {
                clipboard_enabled: clipboard_enabled.clone(),
                session_id: id,
                scheduled_sender: sender.clone(),
                cancel_rx,
                file_inbound_rx,
                talkback_inbound_rx,
                talkback_settings_rx,
                host_send_file: std::env::var_os("REMOTE_PLAY_HOST_SEND_FILE").map(Into::into),
            },
        ));
        Self {
            id,
            last_seen: Instant::now(),
            cancel,
            sender,
            clipboard: None,
            clipboard_cancel: None,
            clipboard_enabled,
            files,
            talkback,
            #[cfg(target_os = "macos")]
            talkback_settings,
            subscriptions: HashMap::new(),
            retired: HashSet::new(),
            audio_groups: HashMap::new(),
            _reporter: reporter,
        }
    }

    fn set_clipboard(&mut self, enabled: bool) {
        self.clipboard_enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        if self.clipboard.is_some() == enabled {
            return;
        }
        if let Some(cancel) = self.clipboard_cancel.take() {
            let _ = cancel.send(());
        }
        self.clipboard = None;
        if enabled {
            let (tx, cancel) = crate::connection_services::start_clipboard(self.sender.clone());
            self.clipboard = Some(tx);
            self.clipboard_cancel = Some(cancel);
        }
    }

    fn refresh_audio(&mut self) {
        self.audio_groups.retain(|key, group| {
            let members: Vec<_> = self
                .subscriptions
                .values()
                .filter(|s| s.audio_group == Some(*key))
                .collect();
            if members.is_empty() {
                return false;
            }
            let paused = !members.iter().any(|s| s.audio_active);
            if group.settings.borrow().paused != paused {
                group.settings.send_modify(|s| s.paused = paused);
            }
            true
        });
    }

    async fn subscribe(
        &mut self,
        request: SubscriptionRequest,
        addr: SocketAddr,
        udp: UdpSender,
        stats: Arc<Statistics>,
        legacy: bool,
    ) -> Result<SessionCommand, String> {
        self.subscribe_with_preflight(request, addr, udp, stats, legacy, || {
            #[cfg(target_os = "windows")]
            crate::capture_readiness::ffmpeg_program()?;
            Ok(())
        })
        .await
    }

    async fn subscribe_with_preflight(
        &mut self,
        request: SubscriptionRequest,
        addr: SocketAddr,
        udp: UdpSender,
        stats: Arc<Statistics>,
        legacy: bool,
        preflight: impl FnOnce() -> Result<(), String>,
    ) -> Result<SessionCommand, String> {
        validate_host_video_settings(
            request.width,
            request.height,
            request.fps,
            request.bitrate_kbps,
        )
        .map_err(str::to_owned)?;
        if self.retired.contains(&request.id) || self.retired.len() >= 4096 {
            return Err("subscription was closed; use a new ID".into());
        }
        if request.width > 8192
            || request.height > 8192
            || u64::from(request.width) * u64::from(request.height) > 33_554_432
        {
            return Err("requested size exceeds this host's capture budget".into());
        }
        if request.id == 0 || request.id >= 0x1000_0000 {
            return Err("subscription ID outside supported range".into());
        }
        if let Some(existing) = self.subscriptions.get_mut(&request.id)
            && existing.request.source == request.source
            && existing.request.audio == request.audio
            && !existing.task.is_finished()
        {
            // Updating rates on an unchanged source must not restart capture,
            // reset the packet sequence or unpause an inactive subscription.
            if existing.request != request {
                existing.request = request.clone();
                existing.settings.send_modify(|v| {
                    v.width = request.width;
                    v.height = request.height;
                    v.fps = request.fps;
                    v.bitrate_kbps = request.bitrate_kbps;
                });
                existing.keyframe.store(true, Relaxed);
            }
            return Ok(SessionCommand::Subscribed {
                id: request.id,
                audio_owner: existing
                    .audio_group
                    .and_then(|key| self.audio_groups.get(&key).map(|g| g.owner)),
                supports_input: existing.supports_input,
            });
        }
        if self.subscriptions.len() >= MAX_SUBSCRIPTIONS
            && !self.subscriptions.contains_key(&request.id)
        {
            return Err("subscription limit reached".into());
        }
        // Check the capture dependency before acquiring microphones, audio groups,
        // or subscription tasks. A permanent packaging failure must remain inert.
        if let Err(reason) = preflight() {
            eprintln!(
                "Capture preflight rejected subscription {}: {reason}",
                request.id
            );
            return Err(reason);
        }
        let sources = if request.source == CaptureSource::MainDisplay {
            Vec::new()
        } else {
            crate::capture_sources::list().map_err(|e| e.to_string())?
        };
        let selected = sources.iter().find(|s| s.source == request.source);
        if request.source != CaptureSource::MainDisplay && selected.is_none() {
            return Err("selected capture source is unavailable".into());
        }
        let supports_input = selected.map_or(request.source == CaptureSource::MainDisplay, |s| {
            s.supports_input
        });
        let input_target = input_target_for(
            request.source,
            selected.and_then(|s| s.process_id),
            supports_input,
        );
        let supports_input = supports_input && input_target.is_some();
        let mut initial_settings = stream_settings(&request);
        let mut activity_revision = 0;
        let mut settings_revision = 0;
        let mut source_revision = 0;
        let mut audio_active = true;
        let mut video_sequence = Arc::new(std::sync::atomic::AtomicU16::new(0));
        if let Some(existing) = self.subscriptions.get_mut(&request.id) {
            initial_settings.paused = existing.settings.borrow().paused;
            activity_revision = existing.revision;
            settings_revision = existing.settings_revision;
            source_revision = existing.source_revision;
            audio_active = existing.audio_active;
            video_sequence = existing.video_sequence.clone();
            // abort() alone only requests cancellation. Wait for actual teardown
            // before a replacement can touch capture/encoder resources.
            let _ = existing.cancel.send(());
            existing.task.abort();
            if tokio::time::timeout(Duration::from_secs(2), &mut existing.task)
                .await
                .is_err()
            {
                return Err("previous capture did not stop; replacement was not started".into());
            }
        }
        self.subscriptions.remove(&request.id);
        // Replacement subscriptions must release the previous Windows capture
        // reservation before acquiring the next one. Otherwise same-session
        // display/source switches fail against their own reservation.
        #[cfg(target_os = "windows")]
        let capture_reservation = Arc::new(WindowsVideoCaptureReservation::acquire()?);
        #[cfg(target_os = "windows")]
        let task_reservation = capture_reservation.clone();
        #[cfg(target_os = "windows")]
        let wants_audio_group = legacy && env_flag_enabled("REMOTE_PLAY_MICROPHONE_CAPTURE");
        #[cfg(not(target_os = "windows"))]
        let wants_audio_group = request.audio || legacy;
        let audio_key = if wants_audio_group {
            Some(
                selected
                    .and_then(|s| s.process_id)
                    .map_or(AudioGroupKey::System, AudioGroupKey::Application),
            )
        } else {
            None
        };
        if let Some(key) = audio_key {
            self.audio_groups.entry(key).or_insert_with(|| {
                let (cancel, cancel_rx) = broadcast::channel(4);
                let (settings, rx) = watch::channel(stream_settings(&request));
                let tasks = start_subscription_audio(SubscriptionAudioConfig {
                    session_id: request.id,
                    source: request.source,
                    client_addr: addr,
                    udp_sender: udp.clone(),
                    scheduled_sender: self.sender.clone(),
                    stats: stats.clone(),
                    cancel_rx,
                    stream_settings_rx: Some(rx),
                    include_audio: request.audio,
                    // Legacy screen viewing is not consent to capture the host microphone.
                    include_microphone: legacy
                        && env_flag_enabled("REMOTE_PLAY_MICROPHONE_CAPTURE"),
                });
                AudioGroup {
                    owner: request.id,
                    settings,
                    cancel,
                    _tasks: tasks,
                }
            });
        }
        let (cancel, cancel_rx) = broadcast::channel(4);
        let (settings, rx) = watch::channel(initial_settings);
        let keyframe = Arc::new(AtomicBool::new(true));
        let stream_config = StreamingRunConfig {
            session_id: request.id,
            client_addr: addr,
            width: request.width,
            height: request.height,
            fps: request.fps,
            bitrate_kbps: request.bitrate_kbps,
            udp_sender: udp.clone(),
            stats,
            cancel_rx,
            scheduled_sender: self.sender.clone(),
            source: request.source,
            stream_settings_rx: Some(rx),
            keyframe_requested: keyframe.clone(),
            video_sequence: video_sequence.clone(),
        };
        let id = request.id;
        let task = tokio::spawn(async move {
            // Stop/drop cannot release the global Windows slot until FFmpeg's
            // destructor has actually completed, even if the task is aborted.
            #[cfg(target_os = "windows")]
            let _reservation = task_reservation;
            if let Err(error) = run_streaming(stream_config).await {
                eprintln!("Capture subscription {id} failed: {error}");
                send_session(
                    &udp,
                    addr,
                    SessionCommand::Error {
                        request_id: id,
                        reason: error.to_string(),
                    },
                )
                .await;
            }
        });
        let audio_owner = audio_key.and_then(|key| self.audio_groups.get(&key).map(|g| g.owner));
        self.subscriptions.insert(
            id,
            Subscription {
                request,
                settings,
                revision: activity_revision,
                settings_revision,
                keyframe,
                cancel,
                task,
                audio_group: audio_key,
                audio_active,
                supports_input,
                input_target,
                last_input_error: None,
                source_revision,
                video_sequence,
                #[cfg(target_os = "windows")]
                _capture_reservation: capture_reservation,
            },
        );
        self.refresh_audio();
        Ok(SessionCommand::Subscribed {
            id,
            audio_owner,
            supports_input,
        })
    }

    async fn switch_source(
        &mut self,
        id: u32,
        request_id: u32,
        source: CaptureSource,
        addr: SocketAddr,
        udp: UdpSender,
        stats: Arc<Statistics>,
    ) -> Result<SessionCommand, String> {
        let current = self
            .subscriptions
            .get(&id)
            .ok_or("subscription is unavailable")?;
        if request_id == 0 || request_id < current.source_revision {
            return Err("stale capture-source request".into());
        }
        if request_id == current.source_revision {
            if source != current.request.source {
                return Err("source request ID reused".into());
            }
            return Ok(SessionCommand::SourceSwitched {
                id,
                request_id,
                source,
                supports_input: current.supports_input,
            });
        }
        let mut request = current.request.clone();
        request.source = source;
        self.subscribe(request, addr, udp, stats, false).await?;
        let current = self
            .subscriptions
            .get_mut(&id)
            .ok_or("subscription disappeared")?;
        current.source_revision = request_id;
        Ok(SessionCommand::SourceSwitched {
            id,
            request_id,
            source,
            supports_input: current.supports_input,
        })
    }

    fn set_activity(
        &mut self,
        id: u32,
        revision: u64,
        video: bool,
        audio: bool,
    ) -> Option<SessionCommand> {
        let subscription = self.subscriptions.get_mut(&id)?;
        if revision > subscription.revision {
            subscription.revision = revision;
            subscription.audio_active = audio;
            subscription.settings.send_modify(|s| s.paused = !video);
            if video {
                subscription.keyframe.store(true, Relaxed);
            }
        }
        let reply = SessionCommand::Activity {
            id,
            revision: subscription.revision,
            video: !subscription.settings.borrow().paused,
            audio: subscription.audio_active,
        };
        self.refresh_audio();
        Some(reply)
    }

    fn can_legacy_input(&self, id: u32) -> bool {
        self.can_input(id)
            && self
                .subscriptions
                .get(&id)
                .is_some_and(|s| s.input_target == Some(InputTarget::Desktop))
    }
    fn can_scoped_input(&self, id: u32, revision: u32) -> bool {
        self.can_input(id)
            && self
                .subscriptions
                .get(&id)
                .is_some_and(|s| s.source_revision == revision)
    }

    fn can_input(&self, id: u32) -> bool {
        self.subscriptions.get(&id).is_some_and(|s| {
            s.supports_input
                && s.input_target.is_some()
                && !s.task.is_finished()
                && !s.settings.borrow().paused
        })
    }
}

fn channel_if(
    enabled: bool,
) -> (
    Option<mpsc::Sender<DataEnvelope>>,
    Option<mpsc::Receiver<DataEnvelope>>,
) {
    if enabled {
        let (tx, rx) = mpsc::channel(256);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    }
}
async fn send_session(sender: &UdpSender, addr: SocketAddr, command: SessionCommand) {
    let _ = sender
        .send_control(&ControlMessage::Session(Box::new(command)), addr)
        .await;
}
async fn inject(
    injector: &LazyInputInjector,
    owner: &mut Option<(SocketAddr, u32)>,
    peer: &mut Connection,
    udp: &UdpSender,
    addr: SocketAddr,
    id: u32,
    event: InputEvent,
) {
    if !peer.can_input(id) {
        return;
    }
    let Some(target) = peer.subscriptions.get(&id).and_then(|s| s.input_target) else {
        return;
    };
    if *owner != Some((addr, id)) {
        injector.release_all_input();
        *owner = Some((addr, id));
    }
    if let Err(error) = injector.inject_target(target, event) {
        injector.release_all_input();
        *owner = None;
        if let Some(s) = peer.subscriptions.get_mut(&id)
            && s.last_input_error
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(2))
        {
            s.last_input_error = Some(Instant::now());
            send_session(
                udp,
                addr,
                SessionCommand::Error {
                    request_id: id,
                    reason: format!("INPUT_UNAVAILABLE: {error}"),
                },
            )
            .await;
        }
    }
}

type BoundInjector = (InputTarget, Box<dyn InputInjector + Send + Sync>);
#[derive(Default)]
struct LazyInputInjector(std::sync::Mutex<Option<BoundInjector>>);
impl LazyInputInjector {
    fn inject_target(
        &self,
        target: InputTarget,
        event: InputEvent,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut current = self.0.lock().map_err(|_| "Input scope lock poisoned")?;
        if current
            .as_ref()
            .is_none_or(|(previous, _)| *previous != target)
        {
            if let Some((_, previous)) = current.take() {
                previous.release_all_input();
            }
            let created: Box<dyn InputInjector + Send + Sync> = match target {
                InputTarget::Desktop => {
                    #[cfg(target_os = "macos")]
                    {
                        Box::new(crate::input_injector::MacInputInjector::new()?)
                    }
                    #[cfg(target_os = "linux")]
                    {
                        Box::new(crate::linux_input::LinuxUinputInjector::new()?)
                    }
                    #[cfg(target_os = "windows")]
                    {
                        Box::new(crate::windows_input::WindowsInputInjector::new()?)
                    }
                }
                InputTarget::Window { id, pid } => {
                    #[cfg(target_os = "macos")]
                    {
                        Box::new(crate::input_injector::MacInputInjector::for_window(
                            id, pid,
                        )?)
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        let _ = (id, pid);
                        return Err("This host does not implement window-scoped input; no desktop fallback was used".into());
                    }
                }
            };
            *current = Some((target, created));
        }
        current
            .as_ref()
            .ok_or("Input injector unavailable")?
            .1
            .inject_input(event)
    }
}
impl InputInjector for LazyInputInjector {
    fn inject_input(&self, event: InputEvent) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.inject_target(InputTarget::Desktop, event)
    }
    fn release_all_input(&self) {
        if let Ok(current) = self.0.lock()
            && let Some((_, injector)) = current.as_ref()
        {
            injector.release_all_input();
        }
    }
}

pub async fn run_host_service(
    config: HostServiceConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let multiplexer = UdpMultiplexer::bind(&config.bind_addr.to_string()).await?;
    let (udp_sender, udp_receiver) = multiplexer.split();
    let injector = LazyInputInjector::default();
    let mut peers = HashMap::<SocketAddr, Connection>::new();
    let mut authenticated = HashMap::<SocketAddr, ([u8; 16], [u8; 16], u64)>::new();
    let mut psk = load_session_psk();
    let mut require_auth = require_session_auth() || psk.is_some();
    let mut input_owner = None;
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let current_key = load_session_psk();
                if current_key != psk {
                    for addr in authenticated.keys() { udp_sender.remove_peer_crypto(*addr); }
                    peers.clear(); authenticated.clear(); psk = current_key;
                    require_auth = require_session_auth() || psk.is_some();
                }
                let expired: Vec<_> = peers.iter().filter(|(_, peer)| peer.last_seen.elapsed() > Duration::from_secs(15)).map(|(addr, _)| *addr).collect();
                for addr in expired { peers.remove(&addr); authenticated.remove(&addr); udp_sender.remove_peer_crypto(addr); }
                authenticated.retain(|addr, (_, _, ts)| {
                    let keep = peers.contains_key(addr) || now_unix_ms().saturating_sub(*ts) < 30_000;
                    if !keep { udp_sender.remove_peer_crypto(*addr); }
                    keep
                });
                for peer in peers.values_mut() { peer.subscriptions.retain(|_, s| !s.task.is_finished()); peer.refresh_audio(); }
                if input_owner.is_some_and(|(addr, id)| !peers.get(&addr).is_some_and(|p| p.can_input(id))) {
                    injector.release_all_input(); input_owner = None;
                }
            }
            received = udp_receiver.recv() => {
                let packet = match received { Ok(packet) => packet, Err(error) => { eprintln!("UDP receive: {error}"); continue; } };
                match packet {
                    MultiplexedPacket::Control(ControlMessage::SessionHello { nonce, timestamp_ms, mac }, addr) => {
                        let result = (|| -> Result<_, String> {
                            let key = psk.as_ref().ok_or("host has no session PSK configured")?;
                            verify_session_mac(&mac_session_hello(key, &nonce, timestamp_ms), &mac, timestamp_ms, now_unix_ms()).map_err(|e| e.to_string())?;
                            if let Some((previous, salt, ts)) = authenticated.get(&addr) {
                                if *previous == nonce { return Ok((*salt, *ts)); }
                                if peers.contains_key(&addr) { return Err("close the existing connection before rekeying".into()); }
                            }
                            if authenticated.len() >= 16 && !authenticated.contains_key(&addr) { return Err("authentication capacity reached".into()); }
                            let salt = random_bytes_16(); let ts = now_unix_ms();
                            udp_sender.install_peer_crypto(addr, SessionCrypto::from_psk(key, &salt).map_err(|e| e.to_string())?);
                            authenticated.insert(addr, (nonce, salt, ts));
                            Ok((salt, ts))
                        })();
                        let reply = match result { Ok((salt, timestamp_ms)) => ControlMessage::SessionAccept { salt, timestamp_ms, mac: mac_session_accept(psk.as_ref().unwrap(), &salt, timestamp_ms) }, Err(reason) => ControlMessage::SessionReject { reason } };
                        let _ = udp_sender.send_control(&reply, addr).await;
                    }
                    MultiplexedPacket::Control(message, addr) => {
                        if require_auth && !authenticated.contains_key(&addr) {
                            let _ = udp_sender.send_control(&ControlMessage::SessionReject { reason: "authenticate before opening a connection".into() }, addr).await;
                            continue;
                        }
                        if let Some(peer) = peers.get_mut(&addr) { peer.last_seen = Instant::now(); }
                        match message {
                            ControlMessage::StartStream { width, height, fps, bitrate_kbps, session_id } => {
                                if validate_host_video_settings(width, height, fps, bitrate_kbps).is_err() { continue; }
                                if peers.len() >= 8 && !peers.contains_key(&addr) { continue; }
                                let peer = peers.entry(addr).or_insert_with(|| Connection::new(session_id, addr, udp_sender.clone(), &config));
                                peer.set_clipboard(config.enable_clipboard_sync);
                                if !peer.subscriptions.contains_key(&session_id) { peer.subscriptions.clear(); peer.refresh_audio(); }
                                if let Err(reason) = peer.subscribe(SubscriptionRequest { id: session_id, source: CaptureSource::MainDisplay, width, height, fps, bitrate_kbps, audio: env_flag_enabled("REMOTE_PLAY_SYSTEM_AUDIO") }, addr, udp_sender.clone(), config.stats.clone(), true).await {
                                    send_session(&udp_sender, addr, SessionCommand::Error { request_id: session_id, reason }).await;
                                }
                            }
                            ControlMessage::Session(command) => match *command {
                                SessionCommand::ReleaseInput { id } => {
                                    if input_owner == Some((addr, id)) { injector.release_all_input(); input_owner=None; }
                                    send_session(&udp_sender,addr,SessionCommand::InputReleased {id}).await;
                                }
                                SessionCommand::Configure { id, revision, width, height, fps, bitrate_kbps } => {
                                    let reply = if validate_host_video_settings(width, height, fps, bitrate_kbps).is_err() || width > 8192 || height > 8192 || u64::from(width) * u64::from(height) > 33_554_432 {
                                        SessionCommand::Error { request_id:id, reason:"unsupported capture settings".into() }
                                    } else if let Some(s) = peers.get_mut(&addr).and_then(|p| p.subscriptions.get_mut(&id)) {
                                        if revision > s.settings_revision {
                                            s.settings_revision = revision;
                                            s.request.width = width; s.request.height = height; s.request.fps = fps; s.request.bitrate_kbps = bitrate_kbps;
                                            s.settings.send_modify(|v| { v.width=width; v.height=height; v.fps=fps; v.bitrate_kbps=bitrate_kbps; });
                                        }
                                        SessionCommand::Configured { id, revision:s.settings_revision }
                                    } else { SessionCommand::Error { request_id:id, reason:"subscription is unavailable".into() } };
                                    send_session(&udp_sender, addr, reply).await;
                                }
                                SessionCommand::SetClipboard { enabled } if peers.contains_key(&addr) => {
                                    let enabled = enabled && config.enable_clipboard_sync;
                                    if enabled { for (other, peer) in &mut peers { if *other != addr { peer.set_clipboard(false); } } }
                                    peers.get_mut(&addr).unwrap().set_clipboard(enabled);
                                    send_session(&udp_sender, addr, SessionCommand::ClipboardState { enabled }).await;
                                }
                                SessionCommand::Open { connection_id, version } => {
                                    let reply = if version != SESSION_VERSION || connection_id == 0 { SessionCommand::Error { request_id: connection_id, reason: "unsupported connection version or ID".into() } }
                                    else if peers.get(&addr).is_some_and(|p| p.id != connection_id) { SessionCommand::Error { request_id: connection_id, reason: "close the existing connection first".into() } }
                                    else if peers.len() >= 8 && !peers.contains_key(&addr) { SessionCommand::Error { request_id: connection_id, reason: "connection limit reached".into() } }
                                    else { let peer = peers.entry(addr).or_insert_with(|| Connection::new(connection_id, addr, udp_sender.clone(), &config)); SessionCommand::Opened { connection_id, version: SESSION_VERSION, max_subscriptions: MAX_SUBSCRIPTIONS as u32, files: peer.files.is_some(), clipboard: config.enable_clipboard_sync, window_capture: cfg!(target_os = "macos") } };
                                    send_session(&udp_sender, addr, reply).await;
                                }
                                SessionCommand::ListSources { request_id } if peers.contains_key(&addr) => {
                                    let reply = match crate::capture_sources::list() { Ok(mut sources) => { sources.truncate(256); SessionCommand::Sources { request_id, sources } }, Err(e) => SessionCommand::Error { request_id, reason: e.to_string() } };
                                    send_session(&udp_sender, addr, reply).await;
                                }
                                SessionCommand::Subscribe(request) => {
                                    let id = request.id;
                                    let result = match peers.get_mut(&addr) {
                                        Some(p) => p.subscribe(request, addr, udp_sender.clone(), config.stats.clone(), false).await,
                                        None => Err("open a connection first".to_owned()),
                                    };
                                    let reply = result.unwrap_or_else(|reason| SessionCommand::Error { request_id: id, reason });
                                    send_session(&udp_sender, addr, reply).await;
                                }
                                SessionCommand::SwitchSource { id, request_id, source } => {
                                    // Never carry a held button/key across capture sources.
                                    if input_owner == Some((addr, id)) { injector.release_all_input(); input_owner=None; }
                                    let result = match peers.get_mut(&addr) {
                                        Some(p) => p.switch_source(id, request_id, source, addr, udp_sender.clone(), config.stats.clone()).await,
                                        None => Err("open a connection first".to_owned()),
                                    };
                                    send_session(&udp_sender, addr, result.unwrap_or_else(|reason| SessionCommand::Error { request_id, reason })).await;
                                }
                                SessionCommand::Unsubscribe { id } => {
                                    if let Some(peer) = peers.get_mut(&addr) { peer.subscriptions.remove(&id); peer.retired.insert(id); peer.refresh_audio(); send_session(&udp_sender, addr, SessionCommand::Unsubscribed { id }).await; }
                                }
                                SessionCommand::Close { connection_id } if peers.get(&addr).is_some_and(|p| p.id == connection_id) => { peers.remove(&addr); }
                                SessionCommand::Input { id, event } => {
                                    if let Some(peer)=peers.get_mut(&addr) && peer.can_legacy_input(id) {
                                        inject(&injector, &mut input_owner, peer, &udp_sender, addr, id, event).await;
                                    }
                                }
                                SessionCommand::SourceInput { id, source_revision, event } => {
                                    if let Some(peer)=peers.get_mut(&addr) && peer.can_scoped_input(id,source_revision) {
                                        inject(&injector, &mut input_owner, peer, &udp_sender, addr, id, event).await;
                                    }
                                }
                                SessionCommand::SetActivity { id, revision, video, audio } => {
                                    if let Some(reply) = peers.get_mut(&addr).and_then(|p| p.set_activity(id, revision, video, audio)) { send_session(&udp_sender, addr, reply).await; }
                                }
                                _ => {}
                            },
                            ControlMessage::StopStream => { peers.remove(&addr); }
                            ControlMessage::Input(event) => {
                                if let Some(peer) = peers.get_mut(&addr) && peer.subscriptions.len() == 1
                                    && let Some(&id) = peer.subscriptions.keys().next() && peer.can_legacy_input(id) { inject(&injector, &mut input_owner, peer, &udp_sender, addr, id, event).await; }
                            }
                            ControlMessage::Ping { client_send_ts } if peers.contains_key(&addr) => { let now = now_unix_ms(); let _ = udp_sender.send_control(&ControlMessage::Pong { client_send_ts, host_recv_ts: now, host_send_ts: now }, addr).await; }
                            ControlMessage::SetMediaPaused { session_id, revision, paused } => {
                                if let Some(peer) = peers.get_mut(&addr) { peer.set_activity(session_id, revision, !paused, !paused);
                                    if let Some(s) = peer.subscriptions.get(&session_id) {
                                        let paused = s.settings.borrow().paused;
                                        let reply = ControlMessage::MediaPauseState { session_id, revision: s.revision, paused };
                                        let _ = udp_sender.send_control(&reply, addr).await;
                                    }
                                }
                            }
                            ControlMessage::UpdateStreamSettings { width, height, fps, bitrate_kbps, session_id } => {
                                if validate_host_video_settings(width, height, fps, bitrate_kbps).is_ok()
                                    && let Some(s) = peers.get_mut(&addr).and_then(|p| p.subscriptions.get_mut(&session_id)) {
                                    s.settings.send_modify(|value| { value.width = width; value.height = height; value.fps = fps; value.bitrate_kbps = bitrate_kbps; });
                                }
                            }
                            ControlMessage::RequestKeyframe { session_id } => { if let Some(s) = peers.get(&addr).and_then(|p| p.subscriptions.get(&session_id)) { s.keyframe.store(true, Relaxed); } }
                            #[cfg(target_os = "macos")]
                            ControlMessage::AudioControl { session_id, target: AudioControlTarget::ViewerTalkbackPlayback, muted, volume_percent } => {
                                if let Some(peer) = peers.get(&addr) && peer.id == session_id && let Some(tx) = &peer.talkback_settings {
                                    tx.send_modify(|s| { s.muted = muted; s.volume_percent = volume_percent.min(200); });
                                }
                            }
                            _ => {}
                        }
                        if input_owner.is_some_and(|(addr, id)| !peers.get(&addr).is_some_and(|p| p.can_input(id))) { injector.release_all_input(); input_owner = None; }
                    }
                    MultiplexedPacket::Data(envelope, addr) | MultiplexedPacket::DataWithTiming(envelope, _, addr) => {
                        let Some(peer) = peers.get_mut(&addr) else { continue; };
                        peer.last_seen = Instant::now();
                        let target = match envelope.header.kind {
                            ContentKind::ClipboardBundle | ContentKind::ClipboardControl => peer.clipboard.as_ref(),
                            ContentKind::FileManifest | ContentKind::FileChunk | ContentKind::FileControl => peer.files.as_ref(),
                            ContentKind::AudioStreamConfig | ContentKind::AudioOpus => peer.talkback.as_ref(),
                            _ => None,
                        };
                        if let Some(tx) = target { let _ = tx.try_send(envelope); }
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod failure_admission_tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_capture_reservation_is_exclusive_and_released() {
        WINDOWS_VIDEO_CAPTURE_RESERVED.store(false, Relaxed);
        let first = WindowsVideoCaptureReservation::acquire().unwrap();
        assert!(WindowsVideoCaptureReservation::acquire().is_err());
        drop(first);
        let second = WindowsVideoCaptureReservation::acquire().unwrap();
        drop(second);
        assert!(!WINDOWS_VIDEO_CAPTURE_RESERVED.load(Relaxed));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_safe_capture_budget_rejects_resource_spikes() {
        assert!(validate_host_video_settings(2560, 1440, 60, 20_000).is_ok());
        assert!(validate_host_video_settings(3840, 2160, 30, 12_000).is_err());
        assert!(validate_host_video_settings(1920, 1080, 120, 12_000).is_err());
        assert!(validate_host_video_settings(1920, 1080, 60, 40_000).is_err());
    }

    #[tokio::test]
    async fn repeated_preflight_failure_never_acquires_audio_or_capture() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _receiver) = mux.split();
        let addr = "127.0.0.1:39571".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        for id in 1..=16 {
            let request = SubscriptionRequest {
                id,
                source: CaptureSource::MainDisplay,
                width: 1280,
                height: 720,
                fps: 30,
                bitrate_kbps: 4000,
                audio: true,
            };
            let result = peer
                .subscribe_with_preflight(
                    request,
                    addr,
                    sender.clone(),
                    config.stats.clone(),
                    true,
                    || Err("missing capture dependency".into()),
                )
                .await;
            assert!(matches!(result, Err(ref error) if error == "missing capture dependency"));
            assert!(peer.audio_groups.is_empty());
            assert!(peer.subscriptions.is_empty());
            assert!(!peer.can_input(id));
            assert_eq!(peer.id, 1);
        }
    }

    struct StopProbe(Arc<AtomicBool>);
    impl Drop for StopProbe {
        fn drop(&mut self) {
            self.0.store(true, Relaxed);
        }
    }

    fn mock_subscription(source: CaptureSource) -> (Subscription, Arc<AtomicBool>) {
        let request = SubscriptionRequest {
            id: 100,
            source,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 4000,
            audio: false,
        };
        let mut initial = stream_settings(&request);
        initial.paused = true;
        let (settings, _) = watch::channel(initial);
        let (cancel, _) = broadcast::channel(4);
        let stopped = Arc::new(AtomicBool::new(false));
        let probe = StopProbe(stopped.clone());
        let task = tokio::spawn(async move {
            let _probe = probe;
            std::future::pending::<()>().await;
        });
        (
            Subscription {
                request,
                settings,
                revision: 7,
                settings_revision: 11,
                keyframe: Arc::new(AtomicBool::new(false)),
                cancel,
                task,
                audio_group: None,
                audio_active: false,
                supports_input: true,
                input_target: Some(InputTarget::Desktop),
                last_input_error: None,
                source_revision: 10,
                video_sequence: Arc::new(std::sync::atomic::AtomicU16::new(37)),
                #[cfg(target_os = "windows")]
                _capture_reservation: Arc::new(WindowsVideoCaptureReservation::acquire().unwrap()),
            },
            stopped,
        )
    }

    #[tokio::test]
    async fn window_input_requires_current_source_revision_and_rejects_legacy() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = mux.split();
        let addr = "127.0.0.1:39577".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender, &config);
        let (mut sub, _) = mock_subscription(CaptureSource::Window(77));
        sub.input_target = Some(InputTarget::Window { id: 77, pid: 123 });
        sub.settings.send_modify(|s| s.paused = false);
        peer.subscriptions.insert(100, sub);
        assert!(peer.can_scoped_input(100, 10));
        assert!(!peer.can_scoped_input(100, 9));
        assert!(!peer.can_scoped_input(100, 11));
        assert!(!peer.can_legacy_input(100));
        assert!(!peer.can_scoped_input(999, 10));
        peer.subscriptions.get_mut(&100).unwrap().source_revision = 11;
        assert!(!peer.can_scoped_input(100, 10));
        assert!(peer.can_scoped_input(100, 11));
        peer.subscriptions
            .get_mut(&100)
            .unwrap()
            .settings
            .send_modify(|s| s.paused = true);
        assert!(!peer.can_scoped_input(100, 11));
    }

    #[tokio::test]
    async fn replacing_source_awaits_teardown_and_keeps_activity_and_packet_sequence() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _rx) = mux.split();
        let addr = "127.0.0.1:39574".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        let (old, stopped) = mock_subscription(CaptureSource::Display(77));
        peer.subscriptions.insert(100, old);
        let request = SubscriptionRequest {
            id: 100,
            source: CaptureSource::MainDisplay,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 4000,
            audio: false,
        };
        peer.subscribe_with_preflight(
            request,
            addr,
            sender,
            config.stats.clone(),
            false,
            || Ok(()),
        )
        .await
        .unwrap();
        assert!(
            stopped.load(Relaxed),
            "old capture must finish before replacement admission"
        );
        let new = peer.subscriptions.get(&100).unwrap();
        assert_eq!(new.request.source, CaptureSource::MainDisplay);
        assert_eq!(new.revision, 7);
        assert_eq!(new.settings_revision, 11);
        assert!(new.settings.borrow().paused);
        assert!(!new.audio_active);
        assert_eq!(new.video_sequence.load(Relaxed), 37);
        // The current-thread test never polls the replacement capture future.
        drop(peer);
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn source_switch_request_ids_are_monotonic_and_duplicate_safe() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _rx) = mux.split();
        let addr = "127.0.0.1:39575".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        let (old, stopped) = mock_subscription(CaptureSource::MainDisplay);
        peer.subscriptions.insert(100, old);
        for request_id in [11, 11] {
            let result = peer
                .switch_source(
                    100,
                    request_id,
                    CaptureSource::MainDisplay,
                    addr,
                    sender.clone(),
                    config.stats.clone(),
                )
                .await
                .unwrap();
            assert!(matches!(
                result,
                SessionCommand::SourceSwitched { request_id: 11, .. }
            ));
            assert!(
                !stopped.load(Relaxed),
                "duplicate/same-source requests must not restart capture"
            );
        }
        assert!(
            peer.switch_source(
                100,
                9,
                CaptureSource::Display(77),
                addr,
                sender,
                config.stats
            )
            .await
            .is_err()
        );
        drop(peer);
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn resubscribe_allows_capture_source_switch() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _receiver) = mux.split();
        let addr = "127.0.0.1:39572".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        let first_request = SubscriptionRequest {
            id: 100,
            source: CaptureSource::MainDisplay,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 4000,
            audio: false,
        };
        let res1 = peer
            .subscribe_with_preflight(
                first_request,
                addr,
                sender.clone(),
                config.stats.clone(),
                false,
                || Ok(()),
            )
            .await;
        assert!(res1.is_ok());
        assert!(peer.subscriptions.contains_key(&100));

        // Re-subscribing with same settings is idempotent
        let res1_repeat = peer
            .subscribe_with_preflight(
                SubscriptionRequest {
                    id: 100,
                    source: CaptureSource::MainDisplay,
                    width: 1280,
                    height: 720,
                    fps: 30,
                    bitrate_kbps: 4000,
                    audio: false,
                },
                addr,
                sender.clone(),
                config.stats.clone(),
                false,
                || Ok(()),
            )
            .await;
        assert!(res1_repeat.is_ok());

        // Re-subscribing with updated settings/audio cleanly switches the active stream
        let res2 = peer
            .subscribe_with_preflight(
                SubscriptionRequest {
                    id: 100,
                    source: CaptureSource::MainDisplay,
                    width: 1920,
                    height: 1080,
                    fps: 60,
                    bitrate_kbps: 8000,
                    audio: true,
                },
                addr,
                sender.clone(),
                config.stats.clone(),
                false,
                || Ok(()),
            )
            .await;
        assert!(res2.is_ok());
        let active = peer
            .subscriptions
            .get(&100)
            .expect("replacement subscription");
        assert_eq!(active.request.width, 1920);
        assert_eq!(active.request.height, 1080);
        assert_eq!(active.request.fps, 60);
        assert_eq!(active.request.bitrate_kbps, 8000);
        assert!(active.request.audio);
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn windows_resubscribe_releases_previous_capture_reservation_first() {
        WINDOWS_VIDEO_CAPTURE_RESERVED.store(false, Relaxed);
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _receiver) = mux.split();
        let addr = "127.0.0.1:39573".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        let first = SubscriptionRequest {
            id: 101,
            source: CaptureSource::MainDisplay,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 4000,
            audio: false,
        };
        peer.subscribe_with_preflight(
            first,
            addr,
            sender.clone(),
            config.stats.clone(),
            false,
            || Ok(()),
        )
        .await
        .unwrap();

        let replacement = peer
            .subscribe_with_preflight(
                SubscriptionRequest {
                    id: 101,
                    source: CaptureSource::MainDisplay,
                    width: 1920,
                    height: 1080,
                    fps: 60,
                    bitrate_kbps: 8000,
                    audio: true,
                },
                addr,
                sender,
                config.stats.clone(),
                false,
                || Ok(()),
            )
            .await;
        assert!(replacement.is_ok());
        assert!(WINDOWS_VIDEO_CAPTURE_RESERVED.load(Relaxed));
        drop(peer);
        tokio::task::yield_now().await;
        assert!(!WINDOWS_VIDEO_CAPTURE_RESERVED.load(Relaxed));
    }
}

#[cfg(test)]
mod input_target_tests {
    use super::*;
    #[test]
    fn windows_are_bound_to_server_selected_owner() {
        assert_eq!(
            input_target_for(CaptureSource::Window(42), Some(123), true),
            Some(InputTarget::Window { id: 42, pid: 123 })
        );
        for pid in [None, Some(0), Some(-1)] {
            assert_eq!(input_target_for(CaptureSource::Window(42), pid, true), None);
        }
    }
    #[test]
    fn readonly_never_falls_back_to_desktop() {
        assert_eq!(
            input_target_for(CaptureSource::Window(42), Some(123), false),
            None
        );
        assert_eq!(
            input_target_for(CaptureSource::Display(77), None, false),
            None
        );
        assert_eq!(
            input_target_for(CaptureSource::MainDisplay, None, true),
            Some(InputTarget::Desktop)
        );
    }
}
