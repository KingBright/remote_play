//! One authenticated device connection with independent video subscriptions and
//! shared audio/file services. Media never owns the connection lifetime.
use crate::Statistics;
use crate::workspace_input::InputState;
use crate::client_session::{
    AudioIngressEvent, EnvelopeIngress, MediaPacketHandler, SharedHostStats, VideoPacket,
};
use crate::file_transfer_runtime::{
    FileTransferCommand, FileTransferEvent, FileTransferRuntimeConfig, run_file_transfer_runtime,
};
use crate::media_plane::{audio_stream_config_from_envelope, realtime_data_to_rtp};
use crate::net::{MultiplexedPacket, UdpMultiplexer, UdpReceiver, UdpSender};
use crate::scheduled_sender::{ScheduledDataSender, ScheduledDataSenderConfig};
use crate::session_crypto::*;
use protocol::session::{SESSION_VERSION, SessionCommand, SubscriptionRequest};
use protocol::{ContentKind, ControlMessage, DataEnvelope, FrameTimingCheckpoints, RtpPacket};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;

type VideoIngress = mpsc::Sender<VideoPacket>;
enum Command {
    Control(SessionCommand),
    Realtime(protocol::realtime_input::RealtimeInput, Instant),
    Subscribe(
        SubscriptionRequest,
        VideoIngress,
        Arc<Statistics>,
        Arc<SharedHostStats>,
    ),
    Clipboard(Option<Arc<dyn EnvelopeIngress>>),
}

pub struct WorkspaceEvents {
    pub control: mpsc::UnboundedReceiver<SessionCommand>,
    pub audio: mpsc::Receiver<AudioIngressEvent>,
    pub files: mpsc::UnboundedReceiver<FileTransferEvent>,
}

pub struct WorkspaceConnection {
    last_response_rx: tokio::sync::watch::Receiver<Instant>,
    input: Arc<std::sync::Mutex<InputState>>,
    pub max_subscriptions: usize,
    pub files_available: bool,
    pub clipboard_available: bool,
    settings_revision: std::sync::atomic::AtomicU64,
    pub id: u32,
    pub sender: UdpSender,
    pub target: SocketAddr,
    pub file_commands: mpsc::Sender<FileTransferCommand>,
    pub file_activity: Arc<std::sync::atomic::AtomicBool>,
    commands: mpsc::Sender<Command>,
    cancel: broadcast::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for WorkspaceConnection {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        // The actor gets a chance to send Close, releasing remote captures promptly.
        let _ = &self.task;
    }
}

impl WorkspaceConnection {
    /// Authenticated connection health is independent of screen dirtiness.
    /// An unchanged desktop is allowed to stop producing new image frames.
    pub fn peer_is_responsive(&self) -> bool {
        !self.task.is_finished()
            && self.last_response_rx.borrow().elapsed() <= Duration::from_secs(6)
    }

    pub async fn connect(
        target: SocketAddr,
        receive_dir: std::path::PathBuf,
    ) -> Result<(Self, WorkspaceEvents), String> {
        Self::connect_inner(target, Some(receive_dir)).await
    }

    /// The single-viewport client does not opt into receiving files.
    pub async fn connect_media(target: SocketAddr) -> Result<(Self, WorkspaceEvents), String> {
        Self::connect_inner(target, None).await
    }

    async fn connect_inner(target: SocketAddr, receive_dir: Option<std::path::PathBuf>) -> Result<(Self, WorkspaceEvents), String> {
        let mux = UdpMultiplexer::bind(if target.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        })
        .await
        .map_err(|e| e.to_string())?;
        let (sender, receiver) = mux.split();
        authenticate(&sender, &receiver, target).await?;
        let id = rand::random::<u32>().max(1);
        let open = SessionCommand::Open {
            connection_id: id,
            version: SESSION_VERSION,
        };
        let reply = initial_open(&sender, &receiver, target, open).await?;
        let (max_subscriptions, files_available, clipboard_available) = match &reply {
            SessionCommand::Opened {
                max_subscriptions,
                files,
                clipboard,
                ..
            } => (
                (*max_subscriptions as usize).min(protocol::session::MAX_SUBSCRIPTIONS),
                *files,
                *clipboard,
            ),
            _ => return Err("peer did not advertise session capabilities".into()),
        };
        let (last_response_tx, last_response_rx) = tokio::sync::watch::channel(Instant::now());
        let (commands, command_rx) = mpsc::channel(128);
        let (control_tx, control) = mpsc::unbounded_channel();
        let _ = control_tx.send(reply);
        let (audio_tx, audio) = mpsc::channel(64);
        let (file_commands, command_files) = mpsc::channel(16);
        let file_activity = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (files_tx, files_rx) = mpsc::channel(128);
        let (file_events, files) = mpsc::unbounded_channel();
        let (cancel, _) = broadcast::channel(4);
        let allow_files = receive_dir.is_some();
        let workers = receive_dir.map(|receive_dir| {
            let (scheduled, worker) = ScheduledDataSender::spawn(sender.clone(), target, ScheduledDataSenderConfig::default());
            let runtime = tokio::spawn(run_file_transfer_runtime(scheduled, command_files, files_rx, file_events, cancel.subscribe(), FileTransferRuntimeConfig {
                receive_dir, active: file_activity.clone(), ..Default::default()
            }));
            (runtime, worker)
        });
        let input = Arc::new(std::sync::Mutex::new(InputState::default()));
        let mut actor = Actor {
            last_response_tx,
            input: input.clone(),
            id,
            target,
            sender: sender.clone(),
            receiver,
            commands: command_rx,
            control_tx,
            audio_tx,
            files_tx,
            subscriptions: HashMap::new(),
            pending: HashMap::new(),
            clipboard: None, audio_groups: HashMap::new(), pending_audio_activity: std::collections::BTreeMap::new(),
        };
        let shutdown = cancel.subscribe();
        let task = tokio::spawn(async move {
            actor.run(shutdown).await;
            if let Some((runtime, worker)) = workers { runtime.abort(); worker.abort(); }
        });
        Ok((
            Self {
                last_response_rx,
                input,
                max_subscriptions,
                files_available: files_available && allow_files,
                clipboard_available,
                settings_revision: std::sync::atomic::AtomicU64::new(0),
                id,
                sender,
                target,
                file_commands,
                file_activity,
                commands,
                cancel,
                task,
            },
            WorkspaceEvents {
                control,
                audio,
                files,
            },
        ))
    }

    /// Retry Close while the receiver is alive; a lost first datagram must not
    /// strand a capture. Bounded shutdown still relies on host lease expiry if offline.
    pub async fn close(&self) {
        let _ = self.control(SessionCommand::Close { connection_id: self.id }).await;
        let _ = tokio::time::timeout(Duration::from_millis(1600), async {
            while !self.task.is_finished() { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await;
        let _ = self.cancel.send(());
    }

    fn prepare_command(&self, command: SessionCommand) -> Result<Command, String> {
        let command = self.input.lock().unwrap().seal(command)?;
        Ok(match command {
            SessionCommand::RealtimeInput(packet) => Command::Realtime(packet, Instant::now()),
            other => Command::Control(other),
        })
    }
    pub fn input_ready(&self, id:u32, source_revision:u32) -> bool {
        self.input.lock().unwrap().ready(id,source_revision)
    }
    pub fn input_binding(&self, id: u32, source_revision: u32) -> Option<protocol::realtime_input::InputBinding> {
        self.input.lock().unwrap().binding(id, source_revision)
    }
    /// Local frontend liveness for this exact grant. Call only from a healthy,
    /// visible input UI (or its validated surface heartbeat), never an actor timer.
    pub fn refresh_bound_input(&self,binding:protocol::realtime_input::InputBinding)->bool {
        self.input.lock().unwrap().refresh_bound(binding)
    }
    /// Bind a gesture to the exact grant observed before translation, including its epoch.
    /// None releases that grant's held state; a newer grant is never substituted.
    pub fn try_bound_input(&self, binding: protocol::realtime_input::InputBinding, event: Option<protocol::InputEvent>) -> bool {
        let mut input = self.input.lock().unwrap();
        let Ok(SessionCommand::RealtimeInput(packet)) = input.seal_bound(binding, event) else { return false; };
        self.commands.try_send(Command::Realtime(packet, Instant::now())).is_ok()
    }
    pub fn control(&self, command: SessionCommand) -> impl std::future::Future<Output=Result<(),String>> + Send + '_ {
        // async fn bodies are lazy: seal at API invocation, before the returned
        // future can wait or a newer source can replace the current binding.
        let command=self.prepare_command(command);
        async move {
            let command=command?;
            self.commands.send(command).await.map_err(|_| "connection closed".into())
        }
    }
    pub fn try_control(&self, command: SessionCommand) -> bool {
        self.prepare_command(command).is_ok_and(|command|self.commands.try_send(command).is_ok())
    }
    pub async fn subscribe(
        &self,
        request: SubscriptionRequest,
        video: VideoIngress,
        stats: Arc<Statistics>,
        host_stats: Arc<SharedHostStats>,
    ) -> Result<(), String> {
        self.input.lock().unwrap().source(request.id,0);
        self.commands
            .send(Command::Subscribe(request, video, stats, host_stats))
            .await
            .map_err(|_| "connection closed".into())
    }
    pub async fn update_settings(&self, request: &SubscriptionRequest) -> Result<(), String> {
        self.commands
            .send(Command::Control(SessionCommand::Configure {
                id: request.id,
                revision: self
                    .settings_revision
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1,
                width: request.width,
                height: request.height,
                fps: request.fps,
                bitrate_kbps: request.bitrate_kbps,
            }))
            .await
            .map_err(|_| "connection closed".into())
    }
    pub async fn clipboard(&self, ingress: Option<Arc<dyn EnvelopeIngress>>) -> Result<(), String> {
        self.commands
            .send(Command::Clipboard(ingress))
            .await
            .map_err(|_| "connection closed".into())
    }
}

struct View {
    host_stats: Arc<SharedHostStats>,
    handler: MediaPacketHandler,
    accepted: bool,
    active: bool,
    audio_active: bool,
    audio_owner: Option<u32>,
    wanted: crate::media_demand::MediaDemand,
    activity_revision: u64,
}
struct Pending {
    command: SessionCommand,
    started: Instant,
    last_sent: Instant,
}
struct Actor {
    input: Arc<std::sync::Mutex<InputState>>,
    last_response_tx: tokio::sync::watch::Sender<Instant>,
    id: u32,
    target: SocketAddr,
    sender: UdpSender,
    receiver: UdpReceiver,
    commands: mpsc::Receiver<Command>,
    control_tx: mpsc::UnboundedSender<SessionCommand>,
    audio_tx: mpsc::Sender<AudioIngressEvent>,
    files_tx: mpsc::Sender<DataEnvelope>,
    subscriptions: HashMap<u32, View>,
    pending: HashMap<(u8, u32), Pending>,
    clipboard: Option<Arc<dyn EnvelopeIngress>>,
    audio_groups: HashMap<u32, bool>,
    pending_audio_activity: std::collections::BTreeMap<u32, bool>,
}

#[cfg(test)]
mod input_api_tests {
    use super::*;
    #[tokio::test]
    async fn unpolled_input_future_cannot_be_rebound_to_a_new_generation() {
        let mux=UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target=mux.local_addr().unwrap();let (sender,_)=mux.split();
        let input=Arc::new(std::sync::Mutex::new(InputState::default()));
        let grant=|epoch:u8| {
            let mut state=input.lock().unwrap();state.source(7,0);
            let SessionCommand::RequestInputBinding{request,..}=state.requests(1).remove(0) else{panic!()};
            state.grant(1,protocol::realtime_input::InputBinding{connection_id:1,subscription_id:7,source_revision:0,epoch:[epoch;16]},request,now_unix_ms());
        };
        grant(1);
        let (_,last_response_rx)=tokio::sync::watch::channel(Instant::now());
        let (file_commands,_)=mpsc::channel(1);let (commands,mut queued)=mpsc::channel(1);
        let (cancel,_)=broadcast::channel(1);
        let conn=WorkspaceConnection {
            last_response_rx,input:input.clone(),max_subscriptions:8,files_available:false,clipboard_available:false,
            settings_revision:std::sync::atomic::AtomicU64::new(0),id:1,sender,target,file_commands,
            file_activity:Arc::new(std::sync::atomic::AtomicBool::new(false)),commands,cancel,
            task:tokio::spawn(std::future::pending()),
        };
        let delayed=conn.control(SessionCommand::Input{id:7,event:protocol::InputEvent::MouseDown(0)});
        // Replace the binding while the caller has not polled its future yet.
        grant(2);delayed.await.unwrap();
        let Command::Realtime(packet,_)=queued.recv().await.unwrap() else{panic!()};
        assert_eq!(packet.binding.epoch,[1;16]);
        conn.task.abort();
    }
}

fn request_key(command: &SessionCommand) -> Option<(u8, u32)> {
    match command {
        SessionCommand::ReleaseInput { id } | SessionCommand::InputReleased { id } => {
            Some((7, *id))
        }
        SessionCommand::Configure { id, .. } | SessionCommand::Configured { id, .. } => {
            Some((6, *id))
        }
        SessionCommand::SetClipboard { .. } | SessionCommand::ClipboardState { .. } => Some((5, 0)),
        SessionCommand::SwitchSource { request_id, .. }
        | SessionCommand::SourceSwitched { request_id, .. } => Some((8, *request_id)),
        SessionCommand::ListSources { request_id } | SessionCommand::Sources { request_id, .. } => {
            Some((1, *request_id))
        }
        SessionCommand::Subscribe(request) => Some((2, request.id)),
        SessionCommand::Subscribed { id, .. } => Some((2, *id)),
        SessionCommand::Unsubscribe { id } | SessionCommand::Unsubscribed { id } => Some((3, *id)),
        SessionCommand::Close { connection_id } | SessionCommand::Closed { connection_id, .. } => {
            Some((9, *connection_id))
        }
        SessionCommand::SetActivity { id, .. } | SessionCommand::Activity { id, .. } => {
            Some((4, *id))
        }
        _ => None,
    }
}
fn confirmation_timeout_reason(category:u8) -> &'static str {
    match category {
        1=>"host source list confirmation timed out", 2=>"host video subscription confirmation timed out",
        3=>"host video stop confirmation timed out", 4=>"host media activity confirmation timed out",
        5=>"host clipboard confirmation timed out", 6=>"host video settings confirmation timed out",
        7=>"host input release confirmation timed out", 8=>"host source switch confirmation timed out",
        9=>"host connection close confirmation timed out", _=>"host confirmation timed out",
    }
}
/// Classify only the local, category-preserving timeout messages emitted below.
/// Other authenticated peer errors keep their existing handling. No wire change.
pub fn confirmation_timeout_affects_media(reason:&str) -> Option<bool> {
    (1..=9).find(|category|confirmation_timeout_reason(*category)==reason)
        .map(|category|matches!(category,2|4|8))
}

/// Opt-in scalar diagnostics; never serialize a command or its private payload.
pub fn trace_control_event(connection_id: u32, stage: &str, command: &SessionCommand,
    elapsed: Option<Duration>, gui_pending: Option<bool>) {
    if std::env::var("WEAVELO_SESSION_TRACE").as_deref() != Ok("1") { return; }
    let Some((category, request_id)) = request_key(command).or_else(|| match command {
        SessionCommand::Error { request_id, .. } => Some((0,*request_id)), _=>None }) else { return; };
    let category = match category {0=>"error",1=>"sources",2=>"subscribe",3=>"unsubscribe",4=>"activity",
        5=>"clipboard",6=>"configure",7=>"input_release",8=>"source_switch",9=>"close",_=>"unknown"};
    eprintln!("SESSION_CONTROL_TRACE {}", serde_json::json!({"at_unix_ms":now_unix_ms(),
        "connection_id":connection_id,"request_id":request_id,"category":category,"stage":stage,
        "elapsed_ms":elapsed.map(|v|v.as_millis() as u64),"gui_pending":gui_pending}));
}
impl Actor {
    async fn send(&self, command: &SessionCommand) {
        let _ = self
            .sender
            .send_control(
                &ControlMessage::Session(Box::new(command.clone())),
                self.target,
            )
            .await;
    }
    async fn request(&mut self, command: SessionCommand) {
        if let SessionCommand::SwitchSource { id, .. } = &command {
            // Only the latest intent for this subscription may reopen video.
            // Late acknowledgements of superseded requests are ignored.
            self.pending.retain(|_, pending| {
                !matches!(&pending.command, SessionCommand::SwitchSource { id: previous, .. } if previous == id)
            });
        }
        if let Some(key) = request_key(&command) {
            let now = Instant::now();
            self.pending.insert(
                key,
                Pending {
                    command: command.clone(),
                    started: now,
                    last_sent: now,
                },
            );
        }
        trace_control_event(self.id,"request",&command,Some(Duration::ZERO),None);
        self.send(&command).await;
    }
    fn source_switch_pending(&self, id: u32) -> bool {
        self.pending.values().any(|pending|
            matches!(pending.command, SessionCommand::SwitchSource { id: requested, .. } if requested == id))
    }
    fn prepare_control(&mut self, command: &SessionCommand) {
        match command {
            SessionCommand::Close { .. } => {
                *self.input.lock().unwrap() = InputState::default();
                self.pending.clear();
                self.subscriptions.clear();
            }
            SessionCommand::Unsubscribe { id } => {
                self.subscriptions.remove(id);
                self.pending.remove(&(2, *id));
                self.pending.remove(&(4, *id));
                self.pending.retain(|_, pending|
                    !matches!(pending.command, SessionCommand::SwitchSource { id: requested, .. } if requested == *id));
            }
            SessionCommand::SetActivity {
                id,
                revision,
                video,
                audio,
            } => {
                if let Some(view) = self.subscriptions.get_mut(id)
                    && *revision > view.activity_revision
                {
                    view.activity_revision = *revision;
                    view.wanted = crate::media_demand::MediaDemand {
                        video: *video,
                        audio: *audio,
                    };
                    // Stop ingress immediately; reopening waits for this exact
                    // activity revision to be acknowledged by the host.
                    if !video {
                        view.active = false;
                        view.handler.suspend_video_source();
                    }
                    if !audio {
                        view.audio_active = false;
                    }
                }
            }
            _ => {}
        }
    }
    fn refresh_audio_activity(&mut self) {
        let mut groups = HashMap::<u32, bool>::new();
        for view in self.subscriptions.values() {
            if let Some(owner) = view.audio_owner {
                *groups.entry(owner).or_default() |= view.audio_active;
            }
        }
        for (owner, previous) in &self.audio_groups {
            if *previous && !groups.contains_key(owner) {
                for stream_id in [protocol::remote_system_audio_stream_id(*owner), protocol::remote_microphone_audio_stream_id(*owner)] {
                    self.pending_audio_activity.insert(stream_id, false);
                }
            }
        }
        for (owner, active) in &groups {
            if self.audio_groups.get(owner).copied().unwrap_or(false) != *active {
                for stream_id in [protocol::remote_system_audio_stream_id(*owner), protocol::remote_microphone_audio_stream_id(*owner)] {
                    self.pending_audio_activity.insert(stream_id, *active);
                }
            }
        }
        self.audio_groups = groups;
        self.flush_audio_activity();
    }
    fn audio_payload_active(&self, stream_id: u32) -> bool {
        self.audio_groups.iter().any(|(owner, active)| {
            *active
                && [
                    protocol::remote_system_audio_stream_id(*owner),
                    protocol::remote_microphone_audio_stream_id(*owner),
                ]
                .contains(&stream_id)
        })
    }
    fn flush_audio_activity(&mut self) -> bool {
        while let Some((&stream_id, &active)) = self.pending_audio_activity.first_key_value() {
            if self
                .audio_tx
                .try_send(AudioIngressEvent::PayloadActivity { stream_id, active })
                .is_err()
            {
                return false;
            }
            self.pending_audio_activity.remove(&stream_id);
        }
        true
    }
    fn reject_request(&mut self, request_id: u32) {
        let video: Vec<_> = self
            .pending
            .values()
            .filter_map(|pending| match pending.command {
                SessionCommand::SwitchSource {
                    id,
                    request_id: requested,
                    ..
                } if requested == request_id => Some(id),
                _ => None,
            })
            .collect();
        self.pending.retain(|key, _| key.1 != request_id);
        // Video was gated while waiting, so even a rejected switch has skipped
        // compressed references. Recover the retained source from a keyframe.
        for id in video {
            if let Some(view) = self.subscriptions.get_mut(&id) {
                view.handler.reset_video_source();
            }
        }
    }
    fn accept_control(&mut self, command: &SessionCommand) -> bool {
        if let SessionCommand::Activity { id, .. } = command
            && (self.pending.contains_key(&(2, *id))
                || !self.subscriptions.get(id).is_some_and(|view| view.accepted))
        {
            // An activity reply cannot stand in for the initial subscription
            // acknowledgement or resurrect a removed recovery target.
            return false;
        }
        if let Some(key) = request_key(command) {
            let pending = self.pending.get(&key);
            trace_control_event(
                self.id,
                "ack_received",
                command,
                pending.map(|p| p.started.elapsed()),
                None,
            );
            let current = match command {
                SessionCommand::Subscribed { id, .. } => self.subscriptions.contains_key(id)
                    && pending.is_some_and(|pending| matches!(&pending.command,
                        SessionCommand::Subscribe(request) if request.id == *id)),
                SessionCommand::SourceSwitched { .. } => pending.is_some_and(|pending| {
                    matches!((&pending.command, command),
                        (SessionCommand::SwitchSource { id, request_id, source },
                         SessionCommand::SourceSwitched { id: accepted_id, request_id: accepted_request, source: accepted_source, .. })
                        if id == accepted_id && request_id == accepted_request && source == accepted_source)
                }),
                _ => pending.is_none_or(|pending| match (&pending.command, command) {
                    (SessionCommand::Configure { revision, .. }, SessionCommand::Configured { revision: accepted, .. }) => accepted >= revision,
                    (SessionCommand::SetActivity { revision, .. }, SessionCommand::Activity { revision: accepted, .. }) => accepted >= revision,
                    _ => true,
                }),
            };
            if !current {
                trace_control_event(
                    self.id,
                    "ack_rejected",
                    command,
                    pending.map(|p| p.started.elapsed()),
                    None,
                );
                return false;
            }
            trace_control_event(
                self.id,
                "ack_accepted",
                command,
                pending.map(|p| p.started.elapsed()),
                None,
            );
            if let SessionCommand::SourceSwitched { id, .. } = command {
                let Some(view) = self.subscriptions.get_mut(id) else {
                    return false;
                };
                view.handler.reset_video_source();
            }
            self.pending.remove(&key);
        }
        if let SessionCommand::Subscribed {
            id, audio_owner, ..
        } = command
        {
            if let Some(view) = self.subscriptions.get_mut(id) {
                view.accepted = true;
                view.audio_owner = *audio_owner;
                view.active = view.wanted.video;
                view.audio_active = view.wanted.audio;
                if view.active {
                    view.handler.activate_video_source(self.target);
                } else {
                    view.handler.suspend_video_source();
                }
            }
            self.refresh_audio_activity();
        }
        if let SessionCommand::Activity {
            id,
            revision,
            video,
            audio,
        } = command
        {
            if let Some(view) = self.subscriptions.get_mut(id) {
                if *revision < view.activity_revision {
                    return false;
                }
                if *video && view.wanted.video && !view.active {
                    view.handler.activate_video_source(self.target);
                }
                if !video && view.active {
                    view.handler.suspend_video_source();
                }
                view.active = *video && view.wanted.video;
                view.audio_active = *audio && view.wanted.audio;
            }
            self.refresh_audio_activity();
        }
        if let SessionCommand::Error { request_id, .. } = command {
            self.reject_request(*request_id);
        }
        true
    }
    async fn expire_request(&mut self, key: (u8, u32)) {
        // The local retry timer knows the exact command category. A numeric
        // subscription/request ID alone can also name an input release or a
        // settings request whose independent acknowledgement is still pending.
        let Some(pending) = self.pending.remove(&key) else { return; };
        trace_control_event(self.id,"expired",&pending.command,Some(pending.started.elapsed()),None);
        if let SessionCommand::Subscribe(request) = &pending.command {
            // A timed-out initial request is terminal for this subscription ID.
            // Retire ingress before reporting failure; late native startup/acks
            // cannot keep invisible capture or audio alive behind a failed GUI.
            let stop = SessionCommand::Unsubscribe { id:request.id };
            self.prepare_control(&stop);
            self.input.lock().unwrap().remove(request.id);
            self.refresh_audio_activity();
            self.request(stop).await;
        }
        if let SessionCommand::SwitchSource { id, .. } = pending.command
            && let Some(view) = self.subscriptions.get_mut(&id) {
            view.handler.reset_video_source();
        }
        let _ = self.control_tx.send(SessionCommand::Error {
            request_id: key.1,
            reason: confirmation_timeout_reason(key.0).into(),
        });
    }
    async fn run(&mut self, mut cancel: broadcast::Receiver<()>) {
        let mut last_pong = Instant::now();
        let mut rejected_count = 0u64;
        let mut rejection_log_at: Option<Instant> = None;
        let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
        let mut clock = crate::clock_sync::ClockSynchronizer::new();
        let mut clock_continuity=crate::clock_sync::ClockContinuity::new();
        let mut probe_id = 0u64;
        let mut last_probe=Instant::now()-Duration::from_secs(5);
        let mut retry = tokio::time::interval(Duration::from_millis(250));
        let mut input_tick = tokio::time::interval(Duration::from_millis(protocol::realtime_input::INPUT_REFRESH_INTERVAL_MS));
        input_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut input_requests_at = Instant::now() - Duration::from_secs(1);
        let mut media = tokio::time::interval(Duration::from_millis(10));
        for interval in [&mut heartbeat, &mut retry, &mut media] {
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        }
        loop {
            tokio::select! {
                _ = cancel.recv() => break,
                _ = heartbeat.tick() => {
                    if last_pong.elapsed() > Duration::from_secs(20) {
                        let _ = self.control_tx.send(SessionCommand::Closed { connection_id:self.id, reason:"device stopped responding".into() });
                        break;
                    }
                    let now=crate::timing::quanta_now_us();
                    if !clock_continuity.observe(now,std::time::SystemTime::now()){clock.invalidate_measurements();}
                    let estimate=clock.estimate(now);
                    for view in self.subscriptions.values_mut(){view.handler.update_clock_estimate(estimate.clone());}
                    let cadence=if self.subscriptions.values().any(|view|view.active){Duration::from_secs(1)}else{Duration::from_secs(5)};
                    if last_probe.elapsed()<cadence{continue;}last_probe=Instant::now();
                    if let Some(session_id)=self.sender.peer_session_id(self.target) {
                        let Some(next)=probe_id.checked_add(1) else { break; };probe_id=next;
                        if clock.begin_probe(probe_id,now).is_ok() {
                            let probe=protocol::session_auth::ClockProbe{session_id,probe_id,client_send_us:now};
                            if self.sender.send_control(&ControlMessage::ClockProbe(probe),self.target).await.is_err(){clock.cancel_probe(probe_id);}
                        }
                    } else {
                        // Unpaired development transport can retain a heartbeat but
                        // never obtains a cross-device clock estimate.
                        let _=self.sender.send_control(&ControlMessage::Ping{client_send_ts:0},self.target).await;
                    }
                }
                _ = retry.tick(), if !self.pending.is_empty() => {
                    let mut expired = Vec::new();
                    for (key, pending) in &mut self.pending {
                        if pending.started.elapsed() > Duration::from_secs(8) { expired.push(*key); continue; }
                        if pending.last_sent.elapsed() >= Duration::from_millis(500) {
                            let _ = self.sender.send_control(&ControlMessage::Session(Box::new(pending.command.clone())), self.target).await;
                            pending.last_sent = Instant::now();
                        }
                    }
                    for key in expired { self.expire_request(key).await; }
                }
                _ = input_tick.tick() => {
                    if input_requests_at.elapsed() >= Duration::from_millis(300) {
                        let requests=self.input.lock().unwrap().requests(self.id);
                        for request in requests { self.send(&request).await; }
                        input_requests_at=Instant::now();
                    }
                    let snapshots=self.input.lock().unwrap().snapshots();
                    for packet in snapshots { self.send(&SessionCommand::RealtimeInput(packet)).await; }
                }
                _ = media.tick(), if self.subscriptions.values().any(|s| s.active) || !self.pending_audio_activity.is_empty() => {
                    self.flush_audio_activity();
                    for (id, view) in &mut self.subscriptions {
                        if !view.active || self.pending.values().any(|pending|
                            matches!(pending.command, SessionCommand::SwitchSource { id: requested, .. } if requested == *id)) { continue; }
                        view.handler.drain_video();
                        if let Some((target, session_id)) = view.handler.take_keyframe_request() {
                            let _ = self.sender.send_control(&ControlMessage::RequestKeyframe { session_id }, target).await;
                        }
                    }
                }
                command = self.commands.recv() => match command {
                    None => break,
                    Some(Command::Subscribe(request, video, stats, host_stats)) => {
                        if self.subscriptions.len() >= protocol::session::MAX_SUBSCRIPTIONS && !self.subscriptions.contains_key(&request.id) {
                            let _ = self.control_tx.send(SessionCommand::Error { request_id: request.id, reason: "decoder subscription limit reached".into() }); continue;
                        }
                        let handler = MediaPacketHandler::new(stats, Arc::new(AtomicU32::new(request.id)), host_stats.clone(), self.audio_tx.clone(), video, None);
                        self.subscriptions.insert(request.id, View { handler, host_stats, accepted: false, active: false, audio_active: false, audio_owner: None, wanted: crate::media_demand::MediaDemand { video: true, audio: request.audio }, activity_revision: 0 });
                        self.refresh_audio_activity();
                        self.request(SessionCommand::Subscribe(request)).await;
                    }
                    Some(Command::Realtime(packet, queued)) => {
                        // Enqueue-time identity is immutable across awaits and switches.
                        if queued.elapsed().as_millis() <= protocol::realtime_input::MAX_INPUT_AGE_MS as u128
                            && self.input.lock().unwrap().ready(packet.binding.subscription_id,packet.binding.source_revision) {
                            self.send(&SessionCommand::RealtimeInput(packet)).await;
                        }
                    }
                    Some(Command::Clipboard(ingress)) => self.clipboard = ingress,
                    Some(Command::Control(command)) => {
                        self.prepare_control(&command);
                        self.refresh_audio_activity();
                        self.request(command).await;
                    }
                },
                packet = self.receiver.recv() => {
                    match packet {
                        Ok(MultiplexedPacket::Control(ControlMessage::Session(command), addr)) if addr == self.target => {
                            let command = *command;
                            match &command {
                                SessionCommand::InputBinding{binding,request,host_time_ms} => {
                                    self.input.lock().unwrap().grant(self.id,*binding,*request,*host_time_ms);
                                    continue;
                                }
                                SessionCommand::InputHandled{binding,sequence} => {
                                    if self.input.lock().unwrap().release_ack(*binding,*sequence) {
                                        let _=self.control_tx.send(SessionCommand::InputReleased{id:binding.subscription_id});
                                    }
                                    continue;
                                }
                                _=>{}
                            }
                            if matches!(command, SessionCommand::Closed { connection_id, .. } if connection_id != self.id) { continue; }
                            if !self.accept_control(&command) { continue; }
                            let closed = matches!(command, SessionCommand::Closed { connection_id, .. } if connection_id == self.id);
                            if let SessionCommand::Activity { id, revision, video, .. } = &command {
                                if let Some(view) = self.subscriptions.get(id) { view.host_stats.media_pause.acknowledge(self.target, *id, *revision, !video); }
                            }
                            let _ = self.control_tx.send(command);
                            if closed { return; }
                        }
                        Ok(MultiplexedPacket::ClockReply(reply,received_us,addr)) if addr==self.target => {
                            // Liveness follows authenticated session traffic, never clock fit quality.
                            last_pong=Instant::now();self.last_response_tx.send_replace(last_pong);
                            if clock.complete_probe(reply.probe.probe_id,reply.probe.client_send_us,reply.host_recv_us,reply.host_send_us,received_us).is_ok(){
                                let estimate=clock.estimate(received_us);
                                for view in self.subscriptions.values_mut(){view.handler.update_clock_estimate(estimate.clone());}
                            }
                        },
                        Ok(MultiplexedPacket::Control(ControlMessage::Pong { .. }, addr)) if addr==self.target && self.sender.peer_session_id(self.target).is_none()=>{
                            last_pong=Instant::now();self.last_response_tx.send_replace(last_pong);
                        },
                        Ok(MultiplexedPacket::Control(ControlMessage::HostTelemetry { fps, encode_latency_ms, jitter_ms, bitrate_kbps }, addr)) if addr == self.target => {
                            for view in self.subscriptions.values() { view.host_stats.apply_host_telemetry(fps,encode_latency_ms,jitter_ms,bitrate_kbps); }
                        },
                        Ok(MultiplexedPacket::Control(ControlMessage::PipelineTelemetry(report), addr)) if addr == self.target => {
                            for view in self.subscriptions.values() { view.host_stats.set_pipeline_report((*report).clone()); }
                        },
                        Ok(MultiplexedPacket::Data(envelope, addr)) if addr == self.target => self.envelope(envelope).await,
                        Ok(MultiplexedPacket::DataWithTiming(mut envelope, timing, addr)) if addr == self.target => { envelope.transport_timing = timing.or(envelope.transport_timing); self.envelope(envelope).await; },
                        Ok(MultiplexedPacket::Rtp(packet, addr)) if addr == self.target => self.packet(packet, FrameTimingCheckpoints::default()).await,
                        Ok(_) => {},
                        Err(error) if error.is::<crate::net::UdpPacketRejection>() => {
                            rejected_count=rejected_count.saturating_add(1);
                            if rejection_log_at.is_none_or(|t|t.elapsed()>=Duration::from_secs(5)) {
                                eprintln!("Discarded invalid session datagram (total {rejected_count}): {error}");
                                rejection_log_at=Some(Instant::now());
                            }
                            // Never deliver rejected bytes as host errors, keepalive,
                            // source changes, or input acknowledgements.
                        }
                        Err(error) => { let _ = self.control_tx.send(SessionCommand::Error { request_id: self.id, reason: error.to_string() }); }
                    }
                }
            }
        }
        self.send(&SessionCommand::Close {
            connection_id: self.id,
        })
        .await;
    }
    async fn envelope(&mut self, envelope: DataEnvelope) {
        match envelope.header.kind {
            ContentKind::FileManifest | ContentKind::FileChunk | ContentKind::FileControl => {
                let _ = self.files_tx.try_send(envelope);
            }
            ContentKind::ClipboardBundle | ContentKind::ClipboardControl => {
                if let Some(clipboard) = &self.clipboard {
                    clipboard.route_inbound(envelope);
                }
            }
            ContentKind::AudioStreamConfig => {
                if self.flush_audio_activity()
                    && let Ok(config) = audio_stream_config_from_envelope(&envelope) {
                    let _ = self
                        .audio_tx
                        .try_send(AudioIngressEvent::StreamConfig(config));
                }
            }
            ContentKind::VideoH265 | ContentKind::AudioOpus => {
                let timing = envelope.transport_timing.unwrap_or_default();
                if let Ok(packet) = realtime_data_to_rtp(envelope) {
                    self.packet(packet, timing).await;
                }
            }
            _ => {}
        }
    }
    async fn packet(&mut self, packet: RtpPacket, timing: FrameTimingCheckpoints) {
        if packet.header.payload_type == protocol::PayloadType::AudioOpus as u8 {
            if self.flush_audio_activity() && self.audio_payload_active(packet.header.ssrc) {
                let _ = self.audio_tx.try_send(AudioIngressEvent::Packet(packet));
            }
        } else if !self.source_switch_pending(packet.header.ssrc)
            && let Some(view) = self.subscriptions.get_mut(&packet.header.ssrc)
            && view.active
        {
            view.handler.remember_video_source(&packet, self.target);
            let size = packet.payload.len() as u64;
            view.handler
                .handle_with_host_timing(packet, size, timing)
                .await;
        }
    }
}

async fn authenticate(
    sender: &UdpSender,
    receiver: &UdpReceiver,
    target: SocketAddr,
) -> Result<(), String> {
    let Some(psk) = load_session_psk() else {
        return Ok(());
    };
    authenticate_with_key(sender, receiver, target, &psk).await
}

async fn authenticate_with_key(
    sender: &UdpSender,
    receiver: &UdpReceiver,
    target: SocketAddr,
    psk: &[u8],
) -> Result<(), String> {
    crate::session_auth::authenticate_with_key(sender,receiver,target,psk).await.map(|_|())
}

async fn initial_open(
    sender: &UdpSender,
    receiver: &UdpReceiver,
    target: SocketAddr,
    open: SessionCommand,
) -> Result<SessionCommand, String> {
    let expected_id = match open {
        SessionCommand::Open { connection_id, .. } => connection_id,
        _ => return Err("expected connection open".into()),
    };
    for _ in 0..4 {
        sender
            .send_control(&ControlMessage::Session(Box::new(open.clone())), target)
            .await
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_millis(750);
        loop {
            let (message, addr) = match tokio::time::timeout_at(deadline, receiver.recv()).await {
                Ok(Ok(MultiplexedPacket::Control(message, addr))) => (message, addr),
                Ok(Ok(_)) => continue,
                Ok(Err(error)) if error.is::<crate::net::UdpPacketRejection>() => continue,
                Ok(Err(error)) => return Err(error.to_string()),
                Err(_) => break,
            };
            if addr != target {
                continue;
            }
            match message {
                ControlMessage::Session(reply) => match *reply {
                    SessionCommand::Opened {
                        version: SESSION_VERSION,
                        connection_id,
                        ..
                    } if connection_id == expected_id => return Ok(*reply),
                    SessionCommand::Error { reason, .. } => return Err(reason),
                    _ => {}
                },
                _ => {}
            }
        }
    }
    Err("peer did not accept a version 2 connection; update the peer".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::session::CaptureSource;

    #[tokio::test]
    async fn close_revokes_held_input_before_waiting_for_a_full_command_queue() {
        let (actor, _audio) = source_switch_actor().await;
        let input = actor.input.clone();
        let (commands, mut queued) = mpsc::channel(1);
        commands.try_send(Command::Clipboard(None)).unwrap();
        let (file_commands, _) = mpsc::channel(1);
        let (_, last_response_rx) = tokio::sync::watch::channel(Instant::now());
        let connection = WorkspaceConnection {
            last_response_rx, input: input.clone(), max_subscriptions: 1,
            files_available: false, clipboard_available: false,
            settings_revision: std::sync::atomic::AtomicU64::new(0), id: 42,
            sender: actor.sender.clone(), target: actor.target, file_commands,
            file_activity: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            commands, cancel: broadcast::channel(1).0, task: tokio::spawn(async {}),
        };
        let mut binding = protocol::realtime_input::InputBinding {
            connection_id: 42, subscription_id: 7, source_revision: 11, epoch: [1; 16],
        };
        let request = {
            let mut state = input.lock().unwrap(); state.source(7, 11);
            let SessionCommand::RequestInputBinding { request, .. } = state.requests(42).remove(0) else { panic!() };
            state.grant(42, binding, request, 1000);
            state.seal_bound(binding, Some(protocol::InputEvent::MouseDown(0))).unwrap();
            request
        };
        // control seals synchronously, even if its returned future is never polled.
        let close = connection.control(SessionCommand::Close { connection_id: 42 });
        {
            let mut state = input.lock().unwrap();
            assert!(!state.ready(7, 11), "Close must revoke input before actor dequeue");
            assert!(state.snapshots().is_empty());
            assert!(!state.refresh_bound(binding));
            state.grant(42, binding, request, 1001);
            assert!(!state.ready(7, 11), "late old grant cannot revive a closing slot");
        }
        assert!(matches!(queued.try_recv().unwrap(), Command::Clipboard(None)));
        assert!(queued.try_recv().is_err(), "unpolled Close has not reached the actor");
        drop(close);
        // Reusing the subscription after reconnect cannot accept the old request
        // or epoch, nor recreate its held button from an acknowledgement.
        let mut state = input.lock().unwrap(); state.source(7, 11);
        let SessionCommand::RequestInputBinding { request: next, .. } = state.requests(42).remove(0) else { panic!() };
        assert_ne!(next, request);
        state.grant(42, binding, request, 1002); assert!(!state.ready(7, 11));
        binding.epoch = [2; 16]; state.grant(42, binding, next, 1003);
        assert!(matches!(&state.snapshots()[0].payload, protocol::realtime_input::InputPayload::Held(held) if held.is_empty()));
    }

    #[tokio::test]
    async fn authenticated_open_ignores_plaintext_retired_reject_and_handshake(){
        let server=UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();let target=server.local_addr().unwrap();let (tx,rx)=server.split();
        let client=UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();let local=client.local_addr().unwrap();let(sender,receiver)=client.split();
        sender.install_peer_crypto(target,SessionCrypto::from_psk(b"fixture",b"open",SessionRole::Client).unwrap());tx.install_peer_crypto(local,SessionCrypto::from_psk(b"fixture",b"open",SessionRole::Host).unwrap());
        let task=tokio::spawn(async move{
            assert!(matches!(rx.recv().await.unwrap(),MultiplexedPacket::Control(ControlMessage::Session(_),_)));
            tx.send_control(&ControlMessage::SessionReject{reason:"forged abort".into()},local).await.unwrap();
            tx.send_control(&ControlMessage::SessionAccept{salt:[0;16],timestamp_ms:0,mac:[0;32]},local).await.unwrap();
            let unrelated=crate::session_auth::ClientHandshake::new(crate::session_auth::AuthTime::now()).message(b"wrong-key");
            tx.send_control(&ControlMessage::SessionHandshake(Box::new(unrelated)),local).await.unwrap();
            tx.send_control(&ControlMessage::Session(Box::new(SessionCommand::Opened{connection_id:55,version:SESSION_VERSION,max_subscriptions:1,files:false,clipboard:false,window_capture:false})),local).await.unwrap();
        });
        assert!(matches!(initial_open(&sender,&receiver,target,SessionCommand::Open{connection_id:55,version:SESSION_VERSION}).await.unwrap(),SessionCommand::Opened{connection_id:55,..}));task.await.unwrap();
    }

    async fn source_switch_actor() -> (Actor, mpsc::Receiver<AudioIngressEvent>) {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target = mux.local_addr().unwrap();
        let (sender, receiver) = mux.split();
        let (last_response_tx, _) = tokio::sync::watch::channel(Instant::now());
        let (_, commands) = mpsc::channel(1);
        let (control_tx, _) = mpsc::unbounded_channel();
        let (audio_tx, audio_rx) = mpsc::channel(8);
        let (files_tx, _) = mpsc::channel(1);
        (
            Actor {
                input: Arc::new(std::sync::Mutex::new(InputState::default())),
                last_response_tx,
                id: 90108,
                target,
                sender,
                receiver,
                commands,
                control_tx,
                audio_tx,
                files_tx,
                subscriptions: HashMap::new(),
                pending: HashMap::new(),
                clipboard: None,
                audio_groups: HashMap::new(),
                pending_audio_activity: std::collections::BTreeMap::new(),
            },
            audio_rx,
        )
    }
    fn source_switch_view(actor: &mut Actor, id: u32) -> mpsc::Receiver<VideoPacket> {
        let (decode_tx, decode_rx) = mpsc::channel(8);
        let handler = MediaPacketHandler::new(
            Statistics::new(),
            Arc::new(AtomicU32::new(id)),
            Arc::new(SharedHostStats::default()),
            actor.audio_tx.clone(),
            decode_tx,
            None,
        );
        actor.audio_groups.insert(id, true);
        actor.subscriptions.insert(
            id,
            View {
                host_stats: Arc::new(SharedHostStats::default()),
                handler,
                accepted: true,
                active: true,
                audio_active: true,
                audio_owner: Some(id),
                wanted: crate::media_demand::MediaDemand::BOTH,
                activity_revision: 0,
            },
        );
        decode_rx
    }
    fn source_video(id: u32, seq: u16) -> RtpPacket {
        RtpPacket {
            header: protocol::RtpHeader {
                version: 2,
                payload_type: protocol::PayloadType::VideoH265 as u8,
                sequence_number: seq,
                timestamp: 1,
                ssrc: id,
            },
            payload: vec![0, 0, 1, 0x26, 1, seq as u8],
        }
    }

    #[tokio::test]
    async fn accepted_first_packet_loss_uses_real_retries_and_pause_unsubscribe_stop_them() {
        let server = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        let (server_tx, server_rx) = server.split();
        let (mut actor, _) = source_switch_actor().await;
        let local = actor.target;
        actor.target = target;
        let key = b"first-packet-recovery-fixture";
        actor
            .sender
            .install_peer_crypto(target, SessionCrypto::from_psk(key, b"test", SessionRole::Client).unwrap());
        server_tx.install_peer_crypto(local, SessionCrypto::from_psk(key, b"test", SessionRole::Host).unwrap());
        let (commands, rx) = mpsc::channel(8);
        actor.commands = rx;
        let (cancel, cancel_rx) = broadcast::channel(1);
        let task = tokio::spawn(async move {
            Box::pin(actor.run(cancel_rx)).await;
        });
        let (video, _video_rx) = mpsc::channel(1);
        let stats = Statistics::new();
        stats.video_recovery.track_output();
        let request = SubscriptionRequest {
            id: 7,
            source: CaptureSource::MainDisplay,
            width: 640,
            height: 360,
            fps: 30,
            bitrate_kbps: 2000,
            audio: false,
        };
        commands
            .send(Command::Subscribe(
                request,
                video,
                stats.clone(),
                Arc::new(SharedHostStats::default()),
            ))
            .await
            .unwrap();
        async fn next_control(rx: &UdpReceiver, category: u8) {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let MultiplexedPacket::Control(ControlMessage::Session(command), _) =
                        rx.recv().await.unwrap()
                        && request_key(&command).is_some_and(|key| key.0 == category)
                    {
                        break;
                    }
                }
            })
            .await
            .unwrap();
        }
        async fn next_recovery(rx: &UdpReceiver) {
            loop {
                if matches!(
                    rx.recv().await.unwrap(),
                    MultiplexedPacket::Control(
                        ControlMessage::RequestKeyframe { session_id: 7 },
                        _
                    )
                ) {
                    break;
                }
            }
        }
        next_control(&server_rx, 2).await;
        server_tx
            .send_control(
                &ControlMessage::Session(Box::new(SessionCommand::Activity {
                    id: 7,
                    revision: 0,
                    video: true,
                    audio: false,
                })),
                local,
            )
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                Box::pin(next_recovery(&server_rx))
            )
            .await
            .is_err()
        );
        server_tx
            .send_control(
                &ControlMessage::Session(Box::new(SessionCommand::Subscribed {
                    id: 7,
                    audio_owner: None,
                    supports_input: true,
                })),
                local,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), Box::pin(next_recovery(&server_rx)))
            .await
            .unwrap();
        let first = Instant::now();
        tokio::time::timeout(Duration::from_secs(1), Box::pin(next_recovery(&server_rx)))
            .await
            .unwrap();
        assert!(
            first.elapsed() >= Duration::from_millis(200),
            "retry must be rate limited"
        );
        assert_eq!(
            stats
                .udp_packets_recv
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );

        commands
            .send(Command::Control(SessionCommand::SetActivity {
                id: 7,
                revision: 1,
                video: false,
                audio: false,
            }))
            .await
            .unwrap();
        next_control(&server_rx, 4).await;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(300),
                Box::pin(next_recovery(&server_rx))
            )
            .await
            .is_err()
        );
        commands
            .send(Command::Control(SessionCommand::SetActivity {
                id: 7,
                revision: 2,
                video: true,
                audio: false,
            }))
            .await
            .unwrap();
        next_control(&server_rx, 4).await;
        server_tx
            .send_control(
                &ControlMessage::Session(Box::new(SessionCommand::Activity {
                    id: 7,
                    revision: 1,
                    video: true,
                    audio: false,
                })),
                local,
            )
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                Box::pin(next_recovery(&server_rx))
            )
            .await
            .is_err(),
            "stale activity ACK"
        );
        server_tx
            .send_control(
                &ControlMessage::Session(Box::new(SessionCommand::Activity {
                    id: 7,
                    revision: 2,
                    video: true,
                    audio: false,
                })),
                local,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), Box::pin(next_recovery(&server_rx)))
            .await
            .unwrap();
        commands
            .send(Command::Control(SessionCommand::Unsubscribe { id: 7 }))
            .await
            .unwrap();
        next_control(&server_rx, 3).await;
        server_tx
            .send_control(
                &ControlMessage::Session(Box::new(SessionCommand::Subscribed {
                    id: 7,
                    audio_owner: None,
                    supports_input: true,
                })),
                local,
            )
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(300),
                Box::pin(next_recovery(&server_rx))
            )
            .await
            .is_err()
        );
        cancel.send(()).unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn source_epoch_is_fixed_before_enqueue_and_duplicate_audio_ack_preserves_it() {
        let (mut actor, _) = source_switch_actor().await;
        let mut packets = source_switch_view(&mut actor, 7);
        actor
            .packet(source_video(7, 10), FrameTimingCheckpoints::default())
            .await;
        actor
            .request(SessionCommand::SwitchSource {
                id: 7,
                request_id: 100,
                source: CaptureSource::Display(88),
            })
            .await;
        assert!(actor.accept_control(&SessionCommand::SourceSwitched {
            id: 7,
            request_id: 100,
            source: CaptureSource::Display(88),
            supports_input: true
        }));
        actor
            .packet(source_video(7, 20), FrameTimingCheckpoints::default())
            .await;
        let old = packets.try_recv().unwrap();
        let current = packets.try_recv().unwrap();
        assert_ne!(old.2, current.2, "queued old IDR must retain its old epoch");
        assert!(actor.accept_control(&SessionCommand::Activity {
            id: 7,
            revision: 1,
            video: true,
            audio: false
        }));
        assert!(actor.accept_control(&SessionCommand::Activity {
            id: 7,
            revision: 1,
            video: true,
            audio: false
        }));
        actor
            .packet(source_video(7, 21), FrameTimingCheckpoints::default())
            .await;
        assert_eq!(
            packets.try_recv().unwrap().2,
            current.2,
            "audio/duplicate ACK must not invalidate video"
        );
        actor.prepare_control(&SessionCommand::Close {
            connection_id: actor.id,
        });
        assert!(
            actor.subscriptions.is_empty(),
            "closing removes recovery targets immediately"
        );
    }

    #[tokio::test]
    async fn confirmation_accepts_current_ack_once_and_preserves_paused_demand() {
        let (mut actor, _audio) = source_switch_actor().await;
        actor.id = 90109;
        let _video = source_switch_view(&mut actor, 7);
        {
            let view = actor.subscriptions.get_mut(&7).unwrap();
            view.active = false;
            view.audio_active = false;
            view.audio_owner = None;
            view.wanted = crate::media_demand::MediaDemand {
                video: false,
                audio: false,
            };
        }
        actor.audio_groups.clear();
        actor
            .request(SessionCommand::Subscribe(SubscriptionRequest {
                id: 7,
                source: CaptureSource::Window(123),
                width: 640,
                height: 360,
                fps: 30,
                bitrate_kbps: 2000,
                audio: true,
            }))
            .await;
        let ack = SessionCommand::Subscribed {
            id: 7,
            audio_owner: Some(7),
            supports_input: true,
        };
        assert!(actor.accept_control(&ack));
        assert!(!actor.subscriptions[&7].active);
        assert!(!actor.subscriptions[&7].audio_active);
        assert!(
            !actor.accept_control(&ack),
            "duplicate ack must not reopen a paused subscription"
        );
    }

    #[tokio::test]
    async fn activity_cannot_replace_a_rejected_initial_subscription_ack() {
        let (mut actor, _audio) = source_switch_actor().await;
        let _video = source_switch_view(&mut actor, 7);
        let view = actor.subscriptions.get_mut(&7).unwrap();
        view.accepted = false;
        view.active = false;
        view.audio_active = false;
        view.handler.suspend_video_source();
        actor
            .request(SessionCommand::Subscribe(SubscriptionRequest {
                id: 7,
                source: CaptureSource::MainDisplay,
                width: 640,
                height: 360,
                fps: 30,
                bitrate_kbps: 2000,
                audio: false,
            }))
            .await;
        assert!(actor.accept_control(&SessionCommand::Error {
            request_id: 7,
            reason: "source unavailable".into()
        }));
        assert!(!actor.pending.contains_key(&(2, 7)));
        assert!(!actor.accept_control(&SessionCommand::Activity {
            id: 7,
            revision: 1,
            video: true,
            audio: false
        }));
        let view = actor.subscriptions.get_mut(&7).unwrap();
        assert!(!view.active);
        assert!(view.handler.take_keyframe_request().is_none());
    }

    #[test]
    fn confirmation_timeout_retains_category_despite_equal_numeric_ids() {
        for category in 1..=9 {
            assert_eq!(confirmation_timeout_affects_media(confirmation_timeout_reason(category)),
                Some(matches!(category,2|4|8)));
        }
        assert_ne!(confirmation_timeout_reason(2),confirmation_timeout_reason(6));
        assert_eq!(confirmation_timeout_affects_media("capture permission denied"),None);
    }

    #[tokio::test]
    async fn confirmation_late_subscribed_cannot_reopen_expired_media() {
        let (mut actor, _audio) = source_switch_actor().await;
        let _video = source_switch_view(&mut actor, 7);
        actor.subscriptions.get_mut(&7).unwrap().audio_owner = None;
        actor.audio_groups.clear();
        let _other = source_switch_view(&mut actor, 8);
        actor.request(SessionCommand::Subscribe(SubscriptionRequest { id:7,
            source:CaptureSource::Window(123),width:640,height:360,fps:30,bitrate_kbps:2000,audio:true })).await;
        actor.pending.get_mut(&(2,7)).unwrap().started -= Duration::from_secs(9);
        actor.expire_request((2,7)).await;
        let late = SessionCommand::Subscribed { id:7,audio_owner:Some(7),supports_input:true };
        assert!(!actor.accept_control(&late), "expired subscription acknowledgement must not reopen media");
        assert!(!actor.subscriptions.contains_key(&7), "expired subscription must retire its media ingress");
        assert!(!actor.audio_groups.contains_key(&7));
        assert!(actor.subscriptions[&8].active, "unrelated confirmed video must remain active");
        assert!(actor.audio_groups[&8], "unrelated audio must remain active");
        assert!(actor.pending.contains_key(&(3,7)), "remote teardown must retain acknowledgement/retry tracking");
    }

    #[tokio::test]
    async fn source_switch_gates_own_video_discards_old_reorder_and_preserves_other_media() {
        let (mut actor,mut audio)=source_switch_actor().await;
        let mut ours=source_switch_view(&mut actor,7);
        let mut other=source_switch_view(&mut actor,8);
        actor.packet(source_video(7,10),FrameTimingCheckpoints::default()).await;
        assert_eq!(ours.try_recv().unwrap().0.header.sequence_number,10);
        actor.packet(source_video(7,12),FrameTimingCheckpoints::default()).await;
        assert!(ours.try_recv().is_err()); // Missing 11 leaves old video in reorder.
        actor.request(SessionCommand::SwitchSource{id:7,request_id:100,source:CaptureSource::Display(88)}).await;
        actor.packet(source_video(7,13),FrameTimingCheckpoints::default()).await;
        actor.packet(source_video(8,1),FrameTimingCheckpoints::default()).await;
        assert_eq!(other.try_recv().unwrap().0.header.ssrc,8);
        assert!(ours.try_recv().is_err());
        let mut sound=source_video(protocol::remote_system_audio_stream_id(7),1);
        sound.header.payload_type=protocol::PayloadType::AudioOpus as u8;
        actor.packet(sound.clone(),FrameTimingCheckpoints::default()).await;
        assert_eq!(audio.try_recv().unwrap(),AudioIngressEvent::Packet(sound));
        for (id,request_id,source) in [(8,100,88),(7,99,88),(7,100,77)] {
            assert!(!actor.accept_control(&SessionCommand::SourceSwitched{id,request_id,
                source:CaptureSource::Display(source),supports_input:true}));
            assert!(actor.source_switch_pending(7));
        }
        let accepted=SessionCommand::SourceSwitched{id:7,request_id:100,source:CaptureSource::Display(88),supports_input:true};
        assert!(actor.accept_control(&accepted));
        assert!(!actor.source_switch_pending(7));
        assert!(!actor.accept_control(&accepted)); // Duplicate cannot reset the new epoch.
        actor.packet(source_video(7,20),FrameTimingCheckpoints::default()).await;
        assert_eq!(ours.try_recv().unwrap().0.header.sequence_number,20);
        tokio::time::sleep(Duration::from_millis(25)).await;
        actor.subscriptions.get_mut(&7).unwrap().handler.drain_video();
        assert!(ours.try_recv().is_err(),"old queued 12/13 reached the decoder after switch");
    }

    #[tokio::test]
    async fn source_switch_latest_intent_and_unsubscribe_reject_stale_acknowledgements() {
        let (mut actor,_)=source_switch_actor().await;
        let _ours=source_switch_view(&mut actor,7);
        let _other=source_switch_view(&mut actor,8);
        for (request_id,display) in [(100,88),(101,99)] {
            actor.request(SessionCommand::SwitchSource{id:7,request_id,source:CaptureSource::Display(display)}).await;
        }
        assert_eq!(actor.pending.len(),1);
        assert!(!actor.accept_control(&SessionCommand::SourceSwitched{id:7,request_id:100,source:CaptureSource::Display(88),supports_input:true}));
        assert!(actor.source_switch_pending(7));
        actor.prepare_control(&SessionCommand::Unsubscribe{id:7});
        assert!(!actor.source_switch_pending(7));
        assert!(!actor.accept_control(&SessionCommand::SourceSwitched{id:7,request_id:101,source:CaptureSource::Display(99),supports_input:true}));
        assert!(!actor.subscriptions.contains_key(&7));
        assert!(actor.subscriptions.contains_key(&8));
    }

    #[tokio::test]
    async fn source_switch_rejection_resumes_old_source_and_success_keeps_activity_paused() {
        let (mut actor,_)=source_switch_actor().await;
        let mut video=source_switch_view(&mut actor,7);
        actor.packet(source_video(7,10),FrameTimingCheckpoints::default()).await;
        video.try_recv().unwrap();
        actor.request(SessionCommand::SwitchSource{id:7,request_id:100,source:CaptureSource::Display(88)}).await;
        actor.packet(source_video(7,11),FrameTimingCheckpoints::default()).await;
        assert!(video.try_recv().is_err());
        assert!(actor.accept_control(&SessionCommand::Error{request_id:100,reason:"source unavailable".into()}));
        assert!(!actor.source_switch_pending(7));
        assert_eq!(actor.subscriptions.get_mut(&7).unwrap().handler.take_keyframe_request(),Some((actor.target,7)));
        actor.packet(source_video(7,12),FrameTimingCheckpoints::default()).await;
        assert!(video.try_recv().is_ok(),"a rejected selection must preserve the existing stream");
        actor.prepare_control(&SessionCommand::SetActivity{id:7,revision:2,video:false,audio:false});
        actor.request(SessionCommand::SwitchSource{id:7,request_id:101,source:CaptureSource::Display(99)}).await;
        assert!(actor.accept_control(&SessionCommand::SourceSwitched{id:7,request_id:101,source:CaptureSource::Display(99),supports_input:true}));
        assert!(!actor.subscriptions[&7].active);
        actor.packet(source_video(7,11),FrameTimingCheckpoints::default()).await;
        assert!(video.try_recv().is_err(),"source acknowledgement must not unpause video");
    }

    #[tokio::test]
    async fn request_expiry_keeps_input_release_configure_and_switch_with_same_numeric_id() {
        let (mut actor, _) = source_switch_actor().await;
        let _video = source_switch_view(&mut actor, 7);
        for command in [
            SessionCommand::SetActivity { id: 7, revision: 1, video: true, audio: false },
            SessionCommand::ReleaseInput { id: 7 },
            SessionCommand::Configure { id: 7, revision: 2, width: 640, height: 360, fps: 30, bitrate_kbps: 2000 },
            SessionCommand::SwitchSource { id: 7, request_id: 7, source: CaptureSource::Display(88) },
        ] {
            actor.request(command).await;
        }
        let mut sent = std::collections::HashSet::new();
        for _ in 0..4 {
            match tokio::time::timeout(Duration::from_secs(1), actor.receiver.recv()).await.unwrap().unwrap() {
                MultiplexedPacket::Control(ControlMessage::Session(command), addr) => {
                    assert_eq!(addr, actor.target);
                    sent.insert(request_key(&command).unwrap());
                }
                _ => panic!("expected actual loopback control datagram"),
            }
        }
        assert_eq!(sent, [(4, 7), (7, 7), (6, 7), (8, 7)].into_iter().collect());
        actor.expire_request((4, 7)).await;
        assert!(!actor.pending.contains_key(&(4, 7)));
        assert!(actor.pending.contains_key(&(7, 7)), "activity timeout must retain input-release retransmission");
        assert!(actor.pending.contains_key(&(6, 7)), "activity timeout must retain settings confirmation");
        assert!(actor.source_switch_pending(7), "an unrelated timeout must not reopen old video");
        assert!(actor.accept_control(&SessionCommand::InputReleased { id: 7 }));
        assert!(!actor.pending.contains_key(&(7, 7)));
        assert!(actor.accept_control(&SessionCommand::Configured { id: 7, revision: 2 }));
        assert!(actor.accept_control(&SessionCommand::SourceSwitched { id: 7, request_id: 7, source: CaptureSource::Display(88), supports_input: true }));
        assert!(actor.pending.is_empty());
    }

    #[tokio::test]
    async fn request_expiry_keeps_actual_release_retransmission_after_activity_timeout() {
        let server = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        let (_, receiver) = server.split();
        let (mut actor, _) = source_switch_actor().await;
        actor.target = target;
        let (commands, command_rx) = mpsc::channel(1);
        actor.commands = command_rx; // Keep the real actor alive for its retry timer.
        actor.request(SessionCommand::SetActivity { id: 7, revision: 1, video: true, audio: false }).await;
        actor.pending.get_mut(&(4, 7)).unwrap().started = Instant::now() - Duration::from_secs(9);
        actor.request(SessionCommand::ReleaseInput { id: 7 }).await;
        let (cancel, rx) = broadcast::channel(1);
        let task = tokio::spawn(async move { actor.run(rx).await; });
        let repeated_release = tokio::time::timeout(Duration::from_secs(2), async {
            let mut count = 0;
            while count < 2 {
                if let MultiplexedPacket::Control(ControlMessage::Session(command), _) = receiver.recv().await.unwrap()
                    && matches!(*command, SessionCommand::ReleaseInput { id: 7 }) {
                    count += 1;
                }
            }
        }).await;
        cancel.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap();
        drop(commands);
        assert!(repeated_release.is_ok(), "the real retry timer must retransmit ReleaseInput after an unrelated activity request expires");
    }

    #[tokio::test]
    async fn request_expiry_of_source_switch_recovers_video_without_cancelling_input_release() {
        let (mut actor, _) = source_switch_actor().await;
        let mut video = source_switch_view(&mut actor, 7);
        actor.packet(source_video(7, 10), FrameTimingCheckpoints::default()).await;
        video.try_recv().unwrap();
        actor.request(SessionCommand::SwitchSource { id: 7, request_id: 7, source: CaptureSource::Display(88) }).await;
        actor.request(SessionCommand::ReleaseInput { id: 7 }).await;
        actor.packet(source_video(7, 11), FrameTimingCheckpoints::default()).await;
        assert!(video.try_recv().is_err());
        actor.expire_request((8, 7)).await;
        assert!(!actor.source_switch_pending(7));
        assert_eq!(actor.subscriptions.get_mut(&7).unwrap().handler.take_keyframe_request(), Some((actor.target, 7)));
        assert!(actor.pending.contains_key(&(7, 7)), "source timeout must retain input-release retransmission");
        actor.packet(source_video(7, 12), FrameTimingCheckpoints::default()).await;
        assert!(video.try_recv().is_ok(), "recover the retained source after a known switch timeout");
    }

    #[tokio::test]
    async fn source_switch_timeout_recovers_only_the_affected_subscription() {
        let (mut actor,_)=source_switch_actor().await;
        let mut ours=source_switch_view(&mut actor,7);
        let mut other=source_switch_view(&mut actor,8);
        actor.packet(source_video(7,10),FrameTimingCheckpoints::default()).await;
        actor.packet(source_video(8,10),FrameTimingCheckpoints::default()).await;
        ours.try_recv().unwrap();other.try_recv().unwrap();
        actor.request(SessionCommand::SwitchSource{id:7,request_id:100,source:CaptureSource::Display(88)}).await;
        actor.expire_request((8, 100)).await; // Same transition used by the bounded retry timer.
        assert!(!actor.source_switch_pending(7));
        assert_eq!(actor.subscriptions.get_mut(&7).unwrap().handler.take_keyframe_request(),Some((actor.target,7)));
        assert_eq!(actor.subscriptions.get_mut(&8).unwrap().handler.take_keyframe_request(),None);
        assert!(!actor.accept_control(&SessionCommand::SourceSwitched{id:7,request_id:100,source:CaptureSource::Display(88),supports_input:true}));
    }

    /// Protocol-only regression. No capture, decoder, account profile or system
    /// input is used: synthetic encrypted datagrams exercise the real actor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_decoder_backpressure_cannot_stall_control_or_datagram_ingress() {
        let server = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        let (server_tx, server_rx) = server.split();
        let client = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let local = client.local_addr().unwrap();
        let (sender, receiver) = client.split();
        let key = b"local-actor-backpressure-regression";
        sender.install_peer_crypto(target, SessionCrypto::from_psk(key, b"fixture", SessionRole::Client).unwrap());
        server_tx.install_peer_crypto(local, SessionCrypto::from_psk(key, b"fixture", SessionRole::Host).unwrap());
        let (last_response_tx, mut last_response_rx) = tokio::sync::watch::channel(Instant::now());
        let (commands, command_rx) = mpsc::channel(128);
        let (control_tx, mut controls) = mpsc::unbounded_channel();
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (files_tx, _files_rx) = mpsc::channel(4);
        let (video_tx, mut video_rx) = mpsc::channel(2);
        let stats = Statistics::new();
        let view = View {
            host_stats: Arc::new(SharedHostStats::default()),
            accepted: true,
            active: true,
            audio_active: true,
            audio_owner: Some(7),
            wanted: crate::media_demand::MediaDemand::BOTH,
            activity_revision: 0,
            handler: MediaPacketHandler::new(
                stats.clone(),
                Arc::new(AtomicU32::new(7)),
                Arc::new(SharedHostStats::default()),
                audio_tx.clone(),
                video_tx,
                None,
            ),
        };
        let mut actor = Actor {
            input:Arc::new(std::sync::Mutex::new(InputState::default())),
            last_response_tx,
            id: 55,
            target,
            sender,
            receiver,
            commands: command_rx,
            control_tx,
            audio_tx,
            files_tx,
            subscriptions: HashMap::from([(7, view)]),
            pending: HashMap::new(),
            clipboard: None, audio_groups: HashMap::new(), pending_audio_activity: std::collections::BTreeMap::new(),
        };
        let (cancel, _) = broadcast::channel(2);
        let cancel_rx = cancel.subscribe();
        let task = tokio::spawn(async move { actor.run(cancel_rx).await });
        // Keep the decoder queue deliberately full while fragmented ingress and
        // encrypted control replies continue through the same UDP socket.
        for seq in 0..30u16 {
            let mut payload = vec![0x44; 5000];
            payload[..5].copy_from_slice(&[0, 0, 1, 0x26, 1]);
            let packet = RtpPacket {
                header: protocol::RtpHeader {
                    version: 2,
                    payload_type: protocol::PayloadType::VideoH265 as u8,
                    sequence_number: seq,
                    timestamp: now_unix_ms() as u32,
                    ssrc: 7,
                },
                payload,
            };
            server_tx.send_rtp(&packet, local).await.unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while stats
            .video_jitter_buffer_push
            .load(std::sync::atomic::Ordering::Relaxed)
            < 30
        {
            assert!(!task.is_finished(), "actor exited during queued video");
            assert!(
                Instant::now() < deadline,
                "ingress stopped with a full decoder queue"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            stats
                .video_decode_queue_dropped
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0
        );
        let (probe,host_recv_us)=tokio::time::timeout(Duration::from_secs(2),async{
            loop{if let MultiplexedPacket::ClockProbe(probe,received,from)=server_rx.recv().await.unwrap(){assert_eq!(from,local);break(probe,received);}}
        }).await.unwrap();
        server_tx.send_control(&ControlMessage::ClockReply(protocol::session_auth::ClockReply{probe,host_recv_us,host_send_us:crate::timing::quanta_now_us()}),local).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), last_response_rx.changed())
            .await
            .unwrap()
            .unwrap();
        commands
            .send(Command::Control(SessionCommand::ListSources {
                request_id: 77,
            }))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1),async {
            loop {if matches!(server_rx.recv().await.unwrap(),MultiplexedPacket::Control(ControlMessage::Session(cmd),_) if matches!(*cmd,SessionCommand::ListSources{request_id:77})){break;}}
        }).await.unwrap();
        server_tx
            .send_control(
                &ControlMessage::Session(Box::new(SessionCommand::Sources {
                    request_id: 77,
                    sources: vec![],
                })),
                local,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(
                    controls.recv().await,
                    Some(SessionCommand::Sources { request_id: 77, .. })
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        while video_rx.try_recv().is_ok() {}
        cancel.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn close_retries_until_authenticated_closed_then_removes_pending() {
        let server = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        let (server_tx, server_rx) = server.split();
        let client = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let local = client.local_addr().unwrap();
        let (sender, receiver) = client.split();
        let key = b"isolated-close-retry-fixture";
        sender.install_peer_crypto(target, SessionCrypto::from_psk(key, b"fixture", SessionRole::Client).unwrap());
        server_tx.install_peer_crypto(local, SessionCrypto::from_psk(key, b"fixture", SessionRole::Host).unwrap());
        let (last_response_tx, _) = tokio::sync::watch::channel(Instant::now());
        let (commands, command_rx) = mpsc::channel(8);
        let (control_tx, mut controls) = mpsc::unbounded_channel();
        let (audio_tx, _) = mpsc::channel(1);
        let (files_tx, _) = mpsc::channel(1);
        let mut actor = Actor {
            input:Arc::new(std::sync::Mutex::new(InputState::default())),
            last_response_tx,
            id: 90100,
            target,
            sender,
            receiver,
            commands: command_rx,
            control_tx,
            audio_tx,
            files_tx,
            subscriptions: HashMap::new(),
            pending: HashMap::new(),
            clipboard: None, audio_groups: HashMap::new(), pending_audio_activity: std::collections::BTreeMap::new(),
        };
        let (cancel, cancel_rx) = broadcast::channel(1);
        let task = tokio::spawn(async move {
            actor.run(cancel_rx).await;
            actor.pending
        });
        commands
            .send(Command::Control(SessionCommand::Close {
                connection_id: 90100,
            }))
            .await
            .unwrap();
        async fn next_close(receiver: &UdpReceiver) {
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let MultiplexedPacket::Control(ControlMessage::Session(command), _) =
                        receiver.recv().await.unwrap()
                    {
                        if matches!(
                            *command,
                            SessionCommand::Close {
                                connection_id: 90100
                            }
                        ) {
                            break;
                        }
                    }
                }
            })
            .await
            .unwrap();
        }
        next_close(&server_rx).await;
        next_close(&server_rx).await;
        server_tx
            .send_control(
                &ControlMessage::Session(Box::new(SessionCommand::Closed {
                    connection_id: 90100,
                    reason: "fixture native stop completed".into(),
                })),
                local,
            )
            .await
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), controls.recv())
                .await
                .unwrap()
                .unwrap(),
            SessionCommand::Closed {
                connection_id: 90100,
                ..
            }
        ));
        let _ = cancel.send(());
        assert!(task.await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn handshake_retries_loss_and_ignores_unrelated_control_without_spending_retries() {
        let server = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        let (server_tx, server_rx) = server.split();
        let client = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (client_tx, client_rx) = client.split();
        let key = b"workspace-handshake-regression-key";
        let task = tokio::spawn(async move {
            let mut first_nonce=None;
            let mut state=crate::session_auth::ServerHandshake::default();
            let mut dropped_finish=false;
            loop {
                if let MultiplexedPacket::Control(ControlMessage::SessionHandshake(message),addr)=server_rx.recv().await.unwrap(){
                    if let protocol::session_auth::HandshakeMessage::Hello{client_nonce,..}=&*message {
                        if first_nonce.is_none(){first_nonce=Some(*client_nonce);continue;}
                        assert_eq!(first_nonce,Some(*client_nonce));
                        for _ in 0..6 {server_tx.send_control(&ControlMessage::Ping{client_send_ts:1},addr).await.unwrap();}
                    }
                    let reply=state.receive(key,&message,addr,crate::session_auth::AuthTime::now(),true).unwrap();
                    if let Some(established)=reply.established {server_tx.install_peer_crypto(addr,established.crypto);}
                    if matches!(reply.message,protocol::session_auth::HandshakeMessage::ServerFinish{..})&&!dropped_finish{dropped_finish=true;continue;}
                    let done=matches!(reply.message,protocol::session_auth::HandshakeMessage::ServerFinish{..});
                    server_tx.send_control(&ControlMessage::SessionHandshake(Box::new(reply.message)),addr).await.unwrap();
                    if done {break;}
                }
            }
        });
        tokio::time::timeout(
            Duration::from_secs(4),
            authenticate_with_key(&client_tx, &client_rx, target, key),
        )
        .await
        .unwrap()
        .unwrap();
        task.await.unwrap();
    }
}

#[cfg(test)]
mod rejected_datagram_tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejected_plaintext_cannot_poison_an_authenticated_session() {
        let server = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        // Independent encryption state over the same test socket simulates a
        // delayed plaintext packet carrying a forged session-level command.
        let (plain, _) = server.split();
        let (server_tx, server_rx) = server.split();
        let client = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let local = client.local_addr().unwrap();
        let (sender, receiver) = client.split();
        let key = b"local-only-rejected-datagram-regression";
        sender.install_peer_crypto(target, SessionCrypto::from_psk(key, b"test-salt", SessionRole::Client).unwrap());
        server_tx.install_peer_crypto(local, SessionCrypto::from_psk(key, b"test-salt", SessionRole::Host).unwrap());
        let (last_response_tx, mut response) = tokio::sync::watch::channel(Instant::now());
        let (commands, commands_rx) = mpsc::channel(8);
        let (control_tx, mut controls) = mpsc::unbounded_channel();
        let (audio_tx, _) = mpsc::channel(2);
        let (files_tx, _) = mpsc::channel(2);
        let mut actor = Actor {
            input:Arc::new(std::sync::Mutex::new(InputState::default())),
            last_response_tx,
            id: 91,
            target,
            sender,
            receiver,
            commands: commands_rx,
            control_tx,
            audio_tx,
            files_tx,
            subscriptions: HashMap::new(),
            pending: HashMap::new(),
            clipboard: None, audio_groups: HashMap::new(), pending_audio_activity: std::collections::BTreeMap::new(),
        };
        let (cancel, _) = broadcast::channel(1);
        let cancel_rx = cancel.subscribe();
        let task = tokio::spawn(async move { actor.run(cancel_rx).await });
        for _ in 0..8 {
            plain
                .send_control(
                    &ControlMessage::Session(Box::new(SessionCommand::Closed {
                        connection_id: 91,
                        reason: "forged untrusted close".into(),
                    })),
                    local,
                )
                .await
                .unwrap();
        }
        let (probe,host_recv_us)=tokio::time::timeout(Duration::from_secs(2),async{
            loop{if let MultiplexedPacket::ClockProbe(probe,received,from)=server_rx.recv().await.unwrap(){assert_eq!(from,local);break(probe,received);}}
        }).await.unwrap();
        server_tx.send_control(&ControlMessage::ClockReply(protocol::session_auth::ClockReply{probe,host_recv_us,host_send_us:crate::timing::quanta_now_us()}),local).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), response.changed())
            .await
            .unwrap()
            .unwrap();
        let delivered = controls.try_recv().ok();
        let alive = !task.is_finished();
        // Authenticated errors are still real product events. We must not hide
        // those merely to eliminate noise from rejected transport datagrams.
        server_tx
            .send_control(
                &ControlMessage::Session(Box::new(SessionCommand::Error {
                    request_id: 77,
                    reason: "authentic source unavailable".into(),
                })),
                local,
            )
            .await
            .unwrap();
        let trusted = tokio::time::timeout(Duration::from_secs(2), controls.recv())
            .await
            .unwrap();
        cancel.send(()).unwrap();
        drop(commands);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            alive,
            "a rejected datagram must not close the session actor"
        );
        assert!(
            delivered.is_none(),
            "unauthenticated packet failure leaked into product control state: {delivered:?}"
        );
        assert!(
            matches!(trusted,Some(SessionCommand::Error{request_id:77,reason}) if reason=="authentic source unavailable")
        );
    }
}

#[cfg(test)]
mod open_packet_isolation_tests {
    use super::*;
    #[tokio::test]
    async fn rejected_datagrams_do_not_consume_connection_open_retry_budget() {
        let server = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        let client = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let local = client.local_addr().unwrap();
        let (plain, _) = server.split();
        let (server_tx, _) = server.split();
        let (sender, receiver) = client.split();
        let key = b"synthetic-open-retry-isolation";
        sender.install_peer_crypto(target, SessionCrypto::from_psk(key, b"salt", SessionRole::Client).unwrap());
        server_tx.install_peer_crypto(local, SessionCrypto::from_psk(key, b"salt", SessionRole::Host).unwrap());
        for _ in 0..8 {
            plain
                .send_control(&ControlMessage::Heartbeat, local)
                .await
                .unwrap();
        }
        server_tx
            .send_control(
                &ControlMessage::Session(Box::new(SessionCommand::Opened {
                    connection_id: 44,
                    version: SESSION_VERSION,
                    max_subscriptions: 4,
                    files: true,
                    clipboard: false,
                    window_capture: true,
                })),
                local,
            )
            .await
            .unwrap();
        let result = initial_open(
            &sender,
            &receiver,
            target,
            SessionCommand::Open {
                connection_id: 44,
                version: SESSION_VERSION,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            result,
            SessionCommand::Opened {
                connection_id: 44,
                ..
            }
        ));
    }
    #[tokio::test]
    async fn an_authenticated_open_error_remains_an_error() {
        let server = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        let client = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let local = client.local_addr().unwrap();
        let (server_tx, _) = server.split();
        let (sender, receiver) = client.split();
        let key = b"synthetic-open-error-test";
        sender.install_peer_crypto(target, SessionCrypto::from_psk(key, b"salt", SessionRole::Client).unwrap());
        server_tx.install_peer_crypto(local, SessionCrypto::from_psk(key, b"salt", SessionRole::Host).unwrap());
        server_tx
            .send_control(
                &ControlMessage::Session(Box::new(SessionCommand::Error {
                    request_id: 44,
                    reason: "connection limit reached".into(),
                })),
                local,
            )
            .await
            .unwrap();
        let error = initial_open(
            &sender,
            &receiver,
            target,
            SessionCommand::Open {
                connection_id: 44,
                version: SESSION_VERSION,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error, "connection limit reached");
    }
}
