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

    fn subscribe(
        &mut self,
        request: SubscriptionRequest,
        addr: SocketAddr,
        udp: UdpSender,
        stats: Arc<Statistics>,
        legacy: bool,
    ) -> Result<SessionCommand, String> {
        protocol::validate_video_settings(
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
        if let Some(existing) = self.subscriptions.get(&request.id) {
            if existing.request.source != request.source || existing.request.audio != request.audio
            {
                return Err("subscription ID already has a different source/settings".into());
            }
            return Ok(SessionCommand::Subscribed {
                id: request.id,
                audio_owner: existing
                    .audio_group
                    .and_then(|key| self.audio_groups.get(&key).map(|g| g.owner)),
                supports_input: existing.supports_input,
            });
        }
        if self.subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return Err("subscription limit reached".into());
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
        let audio_key = if request.audio || legacy {
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
                    include_microphone: legacy,
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
        let (settings, rx) = watch::channel(stream_settings(&request));
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
        };
        let id = request.id;
        let task = tokio::spawn(async move {
            if let Err(error) = run_streaming(stream_config).await {
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
                revision: 0,
                settings_revision: 0,
                keyframe,
                cancel,
                task,
                audio_group: audio_key,
                audio_active: true,
                supports_input,
            },
        );
        self.refresh_audio();
        Ok(SessionCommand::Subscribed {
            id,
            audio_owner,
            supports_input,
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

    fn can_input(&self, id: u32) -> bool {
        self.subscriptions
            .get(&id)
            .is_some_and(|s| s.supports_input && !s.settings.borrow().paused)
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
fn inject(
    injector: &dyn InputInjector,
    owner: &mut Option<(SocketAddr, u32)>,
    addr: SocketAddr,
    id: u32,
    event: InputEvent,
) {
    if *owner != Some((addr, id)) {
        injector.release_all_input();
        *owner = Some((addr, id));
    }
    let _ = injector.inject_input(event);
}

#[derive(Default)]
struct LazyInputInjector(std::sync::OnceLock<Option<Box<dyn InputInjector + Send + Sync>>>);
impl InputInjector for LazyInputInjector {
    fn inject_input(&self, event: InputEvent) -> Result<(), Box<dyn Error + Send + Sync>> {
        let injector = self.0.get_or_init(|| {
            let make =
                || -> Result<Box<dyn InputInjector + Send + Sync>, Box<dyn Error + Send + Sync>> {
                    #[cfg(target_os = "macos")]
                    {
                        Ok(Box::new(crate::input_injector::MacInputInjector::new()?))
                    }
                    #[cfg(target_os = "linux")]
                    {
                        Ok(Box::new(crate::linux_input::LinuxUinputInjector::new()?))
                    }
                    #[cfg(target_os = "windows")]
                    {
                        Ok(Box::new(crate::windows_input::WindowsInputInjector::new()?))
                    }
                };
            make()
                .map_err(|e| eprintln!("Input control unavailable: {e}"))
                .ok()
        });
        injector
            .as_ref()
            .ok_or("input control is unavailable")?
            .inject_input(event)
    }
    fn release_all_input(&self) {
        if let Some(Some(injector)) = self.0.get() {
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
                                if protocol::validate_video_settings(width, height, fps, bitrate_kbps).is_err() { continue; }
                                if peers.len() >= 8 && !peers.contains_key(&addr) { continue; }
                                let peer = peers.entry(addr).or_insert_with(|| Connection::new(session_id, addr, udp_sender.clone(), &config));
                                peer.set_clipboard(config.enable_clipboard_sync);
                                if !peer.subscriptions.contains_key(&session_id) { peer.subscriptions.clear(); peer.refresh_audio(); }
                                if let Err(reason) = peer.subscribe(SubscriptionRequest { id: session_id, source: CaptureSource::MainDisplay, width, height, fps, bitrate_kbps, audio: env_flag_enabled("REMOTE_PLAY_SYSTEM_AUDIO") }, addr, udp_sender.clone(), config.stats.clone(), true) {
                                    send_session(&udp_sender, addr, SessionCommand::Error { request_id: session_id, reason }).await;
                                }
                            }
                            ControlMessage::Session(command) => match *command {
                                SessionCommand::ReleaseInput { id } => {
                                    if input_owner == Some((addr, id)) { injector.release_all_input(); input_owner=None; }
                                    send_session(&udp_sender,addr,SessionCommand::InputReleased {id}).await;
                                }
                                SessionCommand::Configure { id, revision, width, height, fps, bitrate_kbps } => {
                                    let reply = if protocol::validate_video_settings(width, height, fps, bitrate_kbps).is_err() || width > 8192 || height > 8192 || u64::from(width) * u64::from(height) > 33_554_432 {
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
                                    let reply = peers.get_mut(&addr).ok_or_else(|| "open a connection first".to_owned()).and_then(|p| p.subscribe(request, addr, udp_sender.clone(), config.stats.clone(), false)).unwrap_or_else(|reason| SessionCommand::Error { request_id: id, reason });
                                    send_session(&udp_sender, addr, reply).await;
                                }
                                SessionCommand::Unsubscribe { id } => {
                                    if let Some(peer) = peers.get_mut(&addr) { peer.subscriptions.remove(&id); peer.retired.insert(id); peer.refresh_audio(); send_session(&udp_sender, addr, SessionCommand::Unsubscribed { id }).await; }
                                }
                                SessionCommand::Close { connection_id } => { if peers.get(&addr).is_some_and(|p| p.id == connection_id) { peers.remove(&addr); } }
                                SessionCommand::Input { id, event } => { if peers.get(&addr).is_some_and(|p| p.can_input(id)) { inject(&injector, &mut input_owner, addr, id, event); } }
                                SessionCommand::SetActivity { id, revision, video, audio } => {
                                    if let Some(reply) = peers.get_mut(&addr).and_then(|p| p.set_activity(id, revision, video, audio)) { send_session(&udp_sender, addr, reply).await; }
                                }
                                _ => {}
                            },
                            ControlMessage::StopStream => { peers.remove(&addr); }
                            ControlMessage::Input(event) => {
                                if let Some(peer) = peers.get(&addr) && peer.subscriptions.len() == 1
                                    && let Some(&id) = peer.subscriptions.keys().next() && peer.can_input(id) { inject(&injector, &mut input_owner, addr, id, event); }
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
                                if protocol::validate_video_settings(width, height, fps, bitrate_kbps).is_ok()
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
