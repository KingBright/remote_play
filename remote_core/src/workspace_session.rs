//! One authenticated device connection with independent video subscriptions and
//! shared audio/file services. Media never owns the connection lifetime.
use crate::Statistics;
use crate::client_session::{
    AudioIngressEvent, EnvelopeIngress, MediaPacketHandler, SharedHostStats,
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

type VideoIngress = mpsc::Sender<(RtpPacket, FrameTimingCheckpoints)>;
enum Command {
    Control(SessionCommand),
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
        let (scheduled, worker) = ScheduledDataSender::spawn(
            sender.clone(),
            target,
            ScheduledDataSenderConfig::default(),
        );
        let runtime = tokio::spawn(run_file_transfer_runtime(
            scheduled,
            command_files,
            files_rx,
            file_events,
            cancel.subscribe(),
            FileTransferRuntimeConfig {
                receive_dir,
                active: file_activity.clone(),
                ..Default::default()
            },
        ));
        let mut actor = Actor {
            last_response_tx,
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
            clipboard: None,
        };
        let shutdown = cancel.subscribe();
        let task = tokio::spawn(async move {
            actor.run(shutdown).await;
            runtime.abort();
            worker.abort();
        });
        Ok((
            Self {
                last_response_rx,
                max_subscriptions,
                files_available,
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

    pub async fn control(&self, command: SessionCommand) -> Result<(), String> {
        self.commands
            .send(Command::Control(command))
            .await
            .map_err(|_| "connection closed".into())
    }
    pub fn try_control(&self, command: SessionCommand) -> bool {
        self.commands.try_send(Command::Control(command)).is_ok()
    }
    pub async fn subscribe(
        &self,
        request: SubscriptionRequest,
        video: VideoIngress,
        stats: Arc<Statistics>,
        host_stats: Arc<SharedHostStats>,
    ) -> Result<(), String> {
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
    handler: MediaPacketHandler,
    active: bool,
}
struct Pending {
    command: SessionCommand,
    started: Instant,
    last_sent: Instant,
}
struct Actor {
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
        SessionCommand::SetActivity { id, .. } | SessionCommand::Activity { id, .. } => {
            Some((4, *id))
        }
        _ => None,
    }
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
        self.send(&command).await;
    }
    async fn run(&mut self, mut cancel: broadcast::Receiver<()>) {
        let mut last_pong = Instant::now();
        let mut rejected_count = 0u64;
        let mut rejection_log_at: Option<Instant> = None;
        let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
        let mut retry = tokio::time::interval(Duration::from_millis(250));
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
                    let _ = self.sender.send_control(&ControlMessage::Ping { client_send_ts: now_unix_ms() }, self.target).await;
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
                    for key in expired { self.pending.remove(&key); let _ = self.control_tx.send(SessionCommand::Error { request_id: key.1, reason: "host confirmation timed out".into() }); }
                }
                _ = media.tick(), if self.subscriptions.values().any(|s| s.active) => {
                    for view in self.subscriptions.values_mut().filter(|s| s.active) {
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
                        let handler = MediaPacketHandler::new(stats, Arc::new(AtomicU32::new(request.id)), host_stats, self.audio_tx.clone(), video, None);
                        self.subscriptions.insert(request.id, View { handler, active: true });
                        self.request(SessionCommand::Subscribe(request)).await;
                    }
                    Some(Command::Clipboard(ingress)) => self.clipboard = ingress,
                    Some(Command::Control(command)) => {
                        match &command {
                            SessionCommand::Unsubscribe { id } => { self.subscriptions.remove(id); self.pending.remove(&(2, *id)); self.pending.remove(&(4, *id)); }
                            SessionCommand::SetActivity { id, video, .. } => { if let Some(view) = self.subscriptions.get_mut(id) { view.active = *video; } }
                            _ => {}
                        }
                        self.request(command).await;
                    }
                },
                packet = self.receiver.recv() => {
                    match packet {
                        Ok(MultiplexedPacket::Control(ControlMessage::Session(command), addr)) if addr == self.target => {
                            let command = *command;
                            if let Some(key) = request_key(&command) {
                                let current = self.pending.get(&key).is_none_or(|pending| match (&pending.command, &command) {
                                    (SessionCommand::SwitchSource { id, request_id, source }, SessionCommand::SourceSwitched { id: accepted_id, request_id: accepted_request, source: accepted_source, .. }) => id == accepted_id && request_id == accepted_request && source == accepted_source,
                                    (SessionCommand::Configure { revision, .. }, SessionCommand::Configured { revision:accepted, .. }) => accepted >= revision,
                                    (SessionCommand::SetActivity { revision, .. }, SessionCommand::Activity { revision: accepted, .. }) => accepted >= revision,
                                    _ => true,
                                });
                                if !current { continue; }
                                self.pending.remove(&key);
                            }
                            if let SessionCommand::Error { request_id, .. } = &command { self.pending.retain(|key, _| key.1 != *request_id); }
                            let _ = self.control_tx.send(command);
                        }
                        Ok(MultiplexedPacket::Control(ControlMessage::Pong { .. }, addr)) if addr == self.target => { last_pong = Instant::now(); self.last_response_tx.send_replace(last_pong); },
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
                if let Ok(config) = audio_stream_config_from_envelope(&envelope) {
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
            let _ = self.audio_tx.try_send(AudioIngressEvent::Packet(packet));
        } else if let Some(view) = self.subscriptions.get_mut(&packet.header.ssrc)
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
    let nonce = random_bytes_16();
    let timestamp_ms = now_unix_ms();
    let hello = ControlMessage::SessionHello {
        nonce,
        timestamp_ms,
        mac: mac_session_hello(psk, &nonce, timestamp_ms),
    };
    let mut last_error = None;
    for _ in 0..4 {
        sender
            .send_control(&hello, target)
            .await
            .map_err(|e| e.to_string())?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(750);
        loop {
            let received = tokio::time::timeout_at(deadline, receiver.recv()).await;
            let reply = match received {
                Ok(Ok(MultiplexedPacket::Control(reply, addr))) if addr == target => reply,
                Ok(Ok(_)) => continue,
                Ok(Err(error)) => {
                    last_error = Some(error.to_string());
                    continue;
                }
                Err(_) => break,
            };
            match reply {
                ControlMessage::SessionAccept {
                    salt,
                    timestamp_ms,
                    mac,
                } => {
                    verify_session_mac(
                        &mac_session_accept(psk, &salt, timestamp_ms),
                        &mac,
                        timestamp_ms,
                        now_unix_ms(),
                    )
                    .map_err(|e| e.to_string())?;
                    sender.install_peer_crypto(
                        target,
                        SessionCrypto::from_psk(psk, &salt).map_err(|e| e.to_string())?,
                    );
                    return Ok(());
                }
                ControlMessage::SessionReject { reason } => return Err(reason),
                _ => {}
            }
        }
    }
    Err(match last_error {
        Some(error) => format!("authentication timed out: {error}"),
        None => "authentication timed out".into(),
    })
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
                ControlMessage::SessionReject { reason } => return Err(reason),
                _ => {}
            }
        }
    }
    Err("peer did not accept a version 2 connection; update the peer".into())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        sender.install_peer_crypto(target, SessionCrypto::from_psk(key, b"fixture").unwrap());
        server_tx.install_peer_crypto(local, SessionCrypto::from_psk(key, b"fixture").unwrap());
        let (last_response_tx, mut last_response_rx) = tokio::sync::watch::channel(Instant::now());
        let (commands, command_rx) = mpsc::channel(128);
        let (control_tx, mut controls) = mpsc::unbounded_channel();
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (files_tx, _files_rx) = mpsc::channel(4);
        let (video_tx, mut video_rx) = mpsc::channel(2);
        let stats = Statistics::new();
        let view = View {
            active: true,
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
            clipboard: None,
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
        server_tx
            .send_control(
                &ControlMessage::Pong {
                    client_send_ts: 1,
                    host_recv_ts: 2,
                    host_send_ts: 3,
                },
                local,
            )
            .await
            .unwrap();
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
    async fn handshake_retries_loss_and_ignores_unrelated_control_without_spending_retries() {
        let server = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        let (server_tx, server_rx) = server.split();
        let client = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (client_tx, client_rx) = client.split();
        let key = b"workspace-handshake-regression-key";
        let task = tokio::spawn(async move {
            let mut first_nonce = None;
            loop {
                if let MultiplexedPacket::Control(
                    ControlMessage::SessionHello {
                        nonce,
                        timestamp_ms,
                        mac,
                    },
                    addr,
                ) = server_rx.recv().await.unwrap()
                {
                    verify_session_mac(
                        &mac_session_hello(key, &nonce, timestamp_ms),
                        &mac,
                        timestamp_ms,
                        now_unix_ms(),
                    )
                    .unwrap();
                    if first_nonce.is_none() {
                        first_nonce = Some(nonce);
                        continue;
                    }
                    assert_eq!(first_nonce, Some(nonce));
                    for _ in 0..6 {
                        server_tx
                            .send_control(&ControlMessage::Ping { client_send_ts: 1 }, addr)
                            .await
                            .unwrap();
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let salt = random_bytes_16();
                    let timestamp_ms = now_unix_ms();
                    server_tx
                        .send_control(
                            &ControlMessage::SessionAccept {
                                salt,
                                timestamp_ms,
                                mac: mac_session_accept(key, &salt, timestamp_ms),
                            },
                            addr,
                        )
                        .await
                        .unwrap();
                    break;
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
        let (server_tx, _) = server.split();
        let client = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let local = client.local_addr().unwrap();
        let (sender, receiver) = client.split();
        let key = b"local-only-rejected-datagram-regression";
        sender.install_peer_crypto(target, SessionCrypto::from_psk(key, b"test-salt").unwrap());
        server_tx.install_peer_crypto(local, SessionCrypto::from_psk(key, b"test-salt").unwrap());
        let (last_response_tx, mut response) = tokio::sync::watch::channel(Instant::now());
        let (commands, commands_rx) = mpsc::channel(8);
        let (control_tx, mut controls) = mpsc::unbounded_channel();
        let (audio_tx, _) = mpsc::channel(2);
        let (files_tx, _) = mpsc::channel(2);
        let mut actor = Actor {
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
            clipboard: None,
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
        server_tx
            .send_control(
                &ControlMessage::Pong {
                    client_send_ts: 1,
                    host_recv_ts: 2,
                    host_send_ts: 3,
                },
                local,
            )
            .await
            .unwrap();
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
        sender.install_peer_crypto(target, SessionCrypto::from_psk(key, b"salt").unwrap());
        server_tx.install_peer_crypto(local, SessionCrypto::from_psk(key, b"salt").unwrap());
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
        sender.install_peer_crypto(target, SessionCrypto::from_psk(key, b"salt").unwrap());
        server_tx.install_peer_crypto(local, SessionCrypto::from_psk(key, b"salt").unwrap());
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
