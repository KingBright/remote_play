//! Android's user-authorized MediaProjection feeds encoded frames into the shared
//! transport. A projection exposes one selected source; it cannot enumerate or
//! silently switch to arbitrary third-party application windows.
use protocol::{ControlMessage, RtpHeader, RtpPacket, session::*};
use remote_core::{
    file_transfer_runtime::*,
    net::{MultiplexedPacket, UdpMultiplexer, UdpSender},
    scheduled_sender::ScheduledDataSender,
    session_crypto::*,
};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, mpsc, watch};

#[derive(Clone, serde::Serialize)]
pub struct CaptureDemand {
    pub active: bool,
    pub audio: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub keyframe: u64,
}
impl Default for CaptureDemand {
    fn default() -> Self {
        Self {
            active: false,
            audio: false,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 5000,
            keyframe: 0,
        }
    }
}
#[derive(Clone, Copy)]
struct MediaPermission {
    enabled: bool,
    audio: bool,
    epoch: u64,
}
struct Frame {
    epoch: u64,
    bytes: Vec<u8>,
    timestamp: u32,
    audio: bool,
}
pub struct MobilePublisher {
    pub address: SocketAddr,
    demand: Arc<Mutex<CaptureDemand>>,
    source: Arc<Mutex<(u32, u32)>>,
    frames: mpsc::Sender<Frame>,
    errors: mpsc::Sender<String>,
    permission: watch::Sender<MediaPermission>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for MobilePublisher {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct Peer {
    addr: SocketAddr,
    id: u32,
    seen: Instant,
    subscription: Option<SubscriptionRequest>,
    retired: std::collections::HashSet<u32>,
    activity_revision: u64,
    settings_revision: u64,
    video_sequence: u16,
    audio_sequence: u16,
    scheduled: ScheduledDataSender,
    files: mpsc::Sender<protocol::DataEnvelope>,
    cancel: broadcast::Sender<()>,
    tasks: Vec<tokio::task::AbortHandle>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl MobilePublisher {
    pub async fn start(
        bind: SocketAddr,
        directory: PathBuf,
        allow_audio: bool,
    ) -> Result<Self, String> {
        let key = load_session_psk().ok_or("Pair this device before starting screen sharing")?;
        Self::start_with_key(bind, directory, key, allow_audio).await
    }
    async fn start_with_key(
        bind: SocketAddr,
        directory: PathBuf,
        key: Vec<u8>,
        allow_audio: bool,
    ) -> Result<Self, String> {
        Self::start_with_permissions(bind, directory, key, true, allow_audio).await
    }
    /// Accept paired file sessions without requesting or exposing screen capture.
    pub async fn start_receiver(bind: SocketAddr, directory: PathBuf) -> Result<Self, String> {
        let key = load_session_psk().ok_or("Join a device network before receiving files")?;
        Self::start_with_permissions(bind, directory, key, false, false).await
    }
    async fn start_with_permissions(
        bind: SocketAddr,
        directory: PathBuf,
        key: Vec<u8>,
        capture_enabled: bool,
        allow_audio: bool,
    ) -> Result<Self, String> {
        let mux = UdpMultiplexer::bind(&bind.to_string())
            .await
            .map_err(|e| e.to_string())?;
        let address = mux.local_addr().map_err(|e| e.to_string())?;
        let (sender, receiver) = mux.split();
        let demand = Arc::new(Mutex::new(CaptureDemand::default()));
        let source = Arc::new(Mutex::new((1080, 1920)));
        let shared = demand.clone();
        let geometry = source.clone();
        let (frames, mut frame_rx) = mpsc::channel::<Frame>(4);
        let (errors, mut error_rx) = mpsc::channel::<String>(1);
        let (permission, mut permission_rx) = watch::channel(MediaPermission {
            enabled: capture_enabled,
            audio: capture_enabled && allow_audio,
            epoch: 1,
        });
        let task = tokio::spawn(async move {
            let mut peer: Option<Peer> = None;
            let mut authenticated: Option<(SocketAddr, [u8; 16], [u8; 16], u64)> = None;
            let mut maintenance = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    biased;
                    changed = permission_rx.changed() => {
                        if changed.is_err() { break; }
                        let permission = *permission_rx.borrow_and_update();
                        // A new authorization must never inherit subscriptions or queued media.
                        while frame_rx.try_recv().is_ok() {}
                        *shared.lock().unwrap() = CaptureDemand::default();
                        if let Some(p) = peer.as_mut() {
                            if let Some(subscription) = p.subscription.take() {
                                p.retired.insert(subscription.id);
                                p.activity_revision = 0;
                                p.settings_revision = 0;
                                reply(&sender, p.addr, SessionCommand::Unsubscribed { id: subscription.id }).await;
                            }
                            let (width, height) = *geometry.lock().unwrap();
                            reply(&sender, p.addr, SessionCommand::Sources {
                                request_id: 0,
                                sources: available_sources(permission, width, height),
                            }).await;
                        }
                    }
                    Some(reason) = error_rx.recv() => {
                        if let Some(p) = &peer { reply(&sender,p.addr,SessionCommand::Closed {connection_id:p.id,reason}).await; }
                        break;
                    }
                    _ = maintenance.tick() => {
                        if peer.as_ref().is_some_and(|p| p.seen.elapsed() > Duration::from_secs(30)) {
                            if let Some(p) = peer.take() { sender.remove_peer_crypto(p.addr); }
                            authenticated = None; shared.lock().unwrap().active = false; shared.lock().unwrap().audio = false;
                        }
                        if let Some(p) = &peer && shared.lock().unwrap().audio && let Some(subscription) = &p.subscription {
                            let config = protocol::AudioStreamConfig::remote_system(protocol::remote_system_audio_stream_id(subscription.id), 48000, 2, 20);
                            if let Ok(envelope) = remote_core::media_plane::audio_stream_config_to_envelope(&config, 0, now_unix_ms()) { let _ = p.scheduled.send(envelope).await; }
                        }
                    }
                    Some(frame) = frame_rx.recv() => {
                        let permission = *permission_rx.borrow();
                        if !permission.enabled || frame.epoch != permission.epoch || (frame.audio && !permission.audio) { continue; }
                        let Some(p) = peer.as_mut() else { continue; };
                        let Some(subscription) = &p.subscription else { continue; };
                        let wanted = shared.lock().unwrap().clone();
                        if (frame.audio && !wanted.audio) || (!frame.audio && !wanted.active) { continue; }
                        let (id, payload_type, sequence) = if frame.audio {
                            (protocol::remote_system_audio_stream_id(subscription.id), protocol::PayloadType::AudioOpus as u8, &mut p.audio_sequence)
                        } else { (subscription.id, 96, &mut p.video_sequence) };
                        let packet = RtpPacket { header:RtpHeader { version:2, payload_type, sequence_number:*sequence, timestamp:frame.timestamp, ssrc:id }, payload:frame.bytes };
                        *sequence = sequence.wrapping_add(1);
                        if let Ok(envelope) = remote_core::media_plane::rtp_to_realtime_data(&packet) { let _ = p.scheduled.send(envelope).await; }
                    }
                    packet = receiver.recv() => {
                        let Ok(packet) = packet else { continue; };
                        match packet {
                            MultiplexedPacket::Control(ControlMessage::SessionHello { nonce, timestamp_ms, mac }, addr) => {
                                if peer.as_ref().is_some_and(|p| p.addr != addr) { continue; }
                                if verify_session_mac(&mac_session_hello(&key, &nonce, timestamp_ms), &mac, timestamp_ms, now_unix_ms()).is_err() { continue; }
                                let (salt, timestamp) = if let Some((a, n, salt, timestamp)) = authenticated && a == addr && n == nonce { (salt, timestamp) }
                                else {
                                    if peer.is_some() { continue; }
                                    if let Some((previous, ..)) = authenticated.take() { sender.remove_peer_crypto(previous); }
                                    let salt = random_bytes_16(); let timestamp = now_unix_ms();
                                    let Ok(crypto) = SessionCrypto::from_psk(&key, &salt) else { continue; };
                                    sender.install_peer_crypto(addr, crypto); authenticated = Some((addr, nonce, salt, timestamp)); (salt, timestamp)
                                };
                                let _ = sender.send_control(&ControlMessage::SessionAccept { salt, timestamp_ms:timestamp, mac:mac_session_accept(&key, &salt, timestamp) }, addr).await;
                            }
                            MultiplexedPacket::Control(message, addr) => {
                                if authenticated.is_none_or(|(a, ..)| a != addr) { continue; }
                                if let Some(p) = peer.as_mut() { p.seen = Instant::now(); }
                                match message {
                                    ControlMessage::Session(command) => match *command {
                                        SessionCommand::Open { connection_id, version:SESSION_VERSION } => {
                                            if peer.as_ref().is_some_and(|p| p.id != connection_id) { reply(&sender, addr, SessionCommand::Error { request_id:connection_id, reason:"close the previous connection first".into() }).await; continue; }
                                            if peer.is_none() { peer = Some(Peer::new(addr, connection_id, &sender, directory.clone())); }
                                            reply(&sender, addr, SessionCommand::Opened { connection_id, version:SESSION_VERSION, max_subscriptions:1, files:true, clipboard:false, window_capture:false }).await;
                                        }
                                        SessionCommand::ListSources { request_id } if peer.is_some() => {
                                            let (width, height) = *geometry.lock().unwrap();
                                            let permission = *permission_rx.borrow();
                                            reply(&sender, addr, SessionCommand::Sources { request_id, sources:available_sources(permission, width, height) }).await;
                                        }
                                        SessionCommand::Subscribe(request) if peer.is_some() => {
                                            let permission = *permission_rx.borrow();
                                            if !permission.enabled {
                                                reply(&sender, addr, SessionCommand::Error { request_id: request.id, reason: "Screen sharing is not authorized on this device; files remain available".into() }).await;
                                                continue;
                                            }
                                            let p = peer.as_mut().unwrap();
                                            if request.source != CaptureSource::MainDisplay || !valid(&request) || p.retired.contains(&request.id) || p.retired.len() >= 4096 || p.subscription.as_ref().is_some_and(|s| s.id != request.id) {
                                                reply(&sender, addr, SessionCommand::Error { request_id:request.id, reason:"Android projection supports one authorized source".into() }).await; continue;
                                            }
                                            if p.subscription.is_none() { let mut d = shared.lock().unwrap(); set_rates(&mut d, &request); d.active=true; d.audio=request.audio && permission.audio; d.keyframe+=1; }
                                            let id=request.id; if p.subscription.is_none() { p.subscription=Some(request); }
                                            reply(&sender, addr, SessionCommand::Subscribed { id, audio_owner:permission.audio.then_some(id), supports_input:false }).await;
                                        }
                                        SessionCommand::SetActivity { id, revision, video, audio } if peer.is_some() => {
                                            let p=peer.as_mut().unwrap(); if p.subscription.as_ref().is_none_or(|s| s.id != id) { continue; }
                                            let (video, audio) = { let mut d=shared.lock().unwrap(); if revision > p.activity_revision { p.activity_revision=revision; if video && !d.active { d.keyframe+=1; } d.active=video; d.audio=audio && permission_rx.borrow().audio; } (d.active,d.audio) };
                                            reply(&sender, addr, SessionCommand::Activity { id, revision:p.activity_revision, video, audio }).await;
                                        }
                                        SessionCommand::Configure { id, revision, width, height, fps, bitrate_kbps } if peer.is_some() => {
                                            let p=peer.as_mut().unwrap();
                                            if let Some(s)=p.subscription.as_mut().filter(|s| s.id==id) {
                                                let updated=SubscriptionRequest { width,height,fps,bitrate_kbps,..s.clone() };
                                                if !valid(&updated) { reply(&sender,addr,SessionCommand::Error {request_id:id,reason:"unsupported stream settings".into()}).await; continue; }
                                                if revision > p.settings_revision { p.settings_revision=revision; *s=updated; set_rates(&mut shared.lock().unwrap(),s); }
                                                reply(&sender,addr,SessionCommand::Configured {id,revision:p.settings_revision}).await;
                                            }
                                        }
                                        SessionCommand::Unsubscribe { id } if peer.is_some() => {
                                            let p=peer.as_mut().unwrap(); p.retired.insert(id); if p.subscription.as_ref().is_some_and(|s| s.id==id) { p.subscription=None; p.activity_revision=0; p.settings_revision=0; shared.lock().unwrap().active=false; shared.lock().unwrap().audio=false; }
                                            reply(&sender,addr,SessionCommand::Unsubscribed {id}).await;
                                        }
                                        SessionCommand::Close { connection_id } if peer.as_ref().is_some_and(|p| p.id==connection_id) => { peer=None; shared.lock().unwrap().active=false; shared.lock().unwrap().audio=false; }
                                        _ => {}
                                    },
                                    ControlMessage::Ping { client_send_ts } if peer.is_some() => { let now=now_unix_ms(); let _=sender.send_control(&ControlMessage::Pong { client_send_ts,host_recv_ts:now,host_send_ts:now },addr).await; }
                                    ControlMessage::RequestKeyframe { .. } => { shared.lock().unwrap().keyframe+=1; }
                                    _ => {}
                                }
                            }
                            MultiplexedPacket::Data(envelope, addr) | MultiplexedPacket::DataWithTiming(envelope, _, addr) => {
                                if let Some(p)=peer.as_mut().filter(|p| p.addr==addr) { p.seen=Instant::now(); let _=p.files.try_send(envelope); }
                            }
                            _ => {}
                        }
                    }
                }
            }
        });
        Ok(Self {
            address,
            demand,
            source,
            frames,
            errors,
            permission,
            task,
        })
    }
    /// Detach or attach media without replacing the authenticated file connection.
    pub fn set_capture_available(&self, enabled: bool, audio: bool) {
        self.permission.send_if_modified(|permission| {
            let audio = enabled && audio;
            if permission.enabled == enabled && permission.audio == audio {
                return false;
            }
            permission.enabled = enabled;
            permission.audio = audio;
            permission.epoch = permission.epoch.wrapping_add(1);
            true
        });
        if !enabled {
            *self.demand.lock().unwrap() = CaptureDemand::default();
        }
    }
    pub fn demand(&self) -> CaptureDemand {
        let mut demand = self.demand.lock().unwrap().clone();
        let permission = *self.permission.borrow();
        demand.active &= permission.enabled;
        demand.audio &= permission.enabled && permission.audio;
        demand
    }
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
    pub fn set_source_size(&self, width: u32, height: u32) {
        *self.source.lock().unwrap() = (width, height);
    }
    pub fn frame(&self, bytes: Vec<u8>, _timestamp_us: i64, audio: bool) {
        let permission = *self.permission.borrow();
        if !permission.enabled || (audio && !permission.audio) || bytes.len() > 8 * 1024 * 1024 {
            return;
        }
        if self
            .frames
            .try_send(Frame {
                epoch: permission.epoch,
                bytes,
                timestamp: now_unix_ms() as u32,
                audio,
            })
            .is_err()
            && !audio
        {
            self.demand.lock().unwrap().keyframe += 1;
        }
    }
    pub fn fail(&self, reason: String) {
        let _ = self.errors.try_send(reason);
    }
}
fn available_sources(
    permission: MediaPermission,
    width: u32,
    height: u32,
) -> Vec<CaptureSourceInfo> {
    if !permission.enabled {
        return Vec::new();
    }
    vec![CaptureSourceInfo {
        source: CaptureSource::MainDisplay,
        title: "Android selected screen or app".into(),
        application: "MediaProjection".into(),
        process_id: None,
        width,
        height,
        supports_input: false,
    }]
}
fn valid(s: &SubscriptionRequest) -> bool {
    protocol::validate_video_settings(s.width, s.height, s.fps, s.bitrate_kbps).is_ok()
        && s.width <= 4096
        && s.height <= 4096
        && u64::from(s.width) * u64::from(s.height) <= 8_388_608
}
fn set_rates(d: &mut CaptureDemand, s: &SubscriptionRequest) {
    d.width = s.width;
    d.height = s.height;
    d.fps = s.fps;
    d.bitrate_kbps = s.bitrate_kbps;
}
async fn reply(sender: &UdpSender, addr: SocketAddr, command: SessionCommand) {
    let _ = sender
        .send_control(&ControlMessage::Session(Box::new(command)), addr)
        .await;
}
impl Peer {
    fn new(addr: SocketAddr, id: u32, sender: &UdpSender, directory: PathBuf) -> Self {
        let (scheduled, worker) =
            ScheduledDataSender::spawn(sender.clone(), addr, Default::default());
        let (files, inbound) = mpsc::channel(128);
        let (commands, command_rx) = mpsc::channel(16);
        let (events, mut event_rx) = mpsc::unbounded_channel();
        let (cancel, rx) = broadcast::channel(1);
        let file_task = tokio::spawn(run_file_transfer_runtime(
            scheduled.clone(),
            command_rx,
            inbound,
            events,
            rx,
            FileTransferRuntimeConfig {
                receive_dir: directory,
                ..Default::default()
            },
        ));
        let event_task = tokio::spawn(async move {
            let _commands = commands;
            while event_rx.recv().await.is_some() {}
        });
        Self {
            addr,
            id,
            seen: Instant::now(),
            subscription: None,
            retired: Default::default(),
            activity_revision: 0,
            settings_revision: 0,
            video_sequence: 0,
            audio_sequence: 0,
            scheduled,
            files,
            cancel,
            tasks: vec![
                worker.abort_handle(),
                file_task.abort_handle(),
                event_task.abort_handle(),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn command(sender: &UdpSender, target: SocketAddr, command: SessionCommand) {
        sender
            .send_control(&ControlMessage::Session(Box::new(command)), target)
            .await
            .unwrap();
    }
    async fn reply(receiver: &remote_core::net::UdpReceiver) -> SessionCommand {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let MultiplexedPacket::Control(ControlMessage::Session(message), _) =
                    receiver.recv().await.unwrap()
                {
                    return *message;
                }
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn publisher_requires_pairing_and_preserves_pause_revision_and_closed_ids() {
        let key = b"projection-test-key".to_vec();
        let directory =
            std::env::temp_dir().join(format!("remote-play-publisher-{}", std::process::id()));
        let publisher = MobilePublisher::start_with_key(
            "127.0.0.1:0".parse().unwrap(),
            directory,
            key.clone(),
            false,
        )
        .await
        .unwrap();
        let client = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, receiver) = client.split();
        let target = publisher.address;
        command(
            &sender,
            target,
            SessionCommand::Open {
                connection_id: 9,
                version: SESSION_VERSION,
            },
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), receiver.recv())
                .await
                .is_err()
        );
        let nonce = random_bytes_16();
        let timestamp_ms = now_unix_ms();
        sender
            .send_control(
                &ControlMessage::SessionHello {
                    nonce,
                    timestamp_ms,
                    mac: mac_session_hello(&key, &nonce, timestamp_ms),
                },
                target,
            )
            .await
            .unwrap();
        let MultiplexedPacket::Control(
            ControlMessage::SessionAccept {
                salt,
                timestamp_ms,
                mac,
            },
            _,
        ) = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("authentication reply missing")
        };
        verify_session_mac(
            &mac_session_accept(&key, &salt, timestamp_ms),
            &mac,
            timestamp_ms,
            now_unix_ms(),
        )
        .unwrap();
        sender.install_peer_crypto(target, SessionCrypto::from_psk(&key, &salt).unwrap());
        command(
            &sender,
            target,
            SessionCommand::Open {
                connection_id: 9,
                version: SESSION_VERSION,
            },
        )
        .await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Opened {
                max_subscriptions: 1,
                ..
            }
        ));
        let request = SubscriptionRequest {
            id: 256,
            source: CaptureSource::MainDisplay,
            width: 720,
            height: 1280,
            fps: 37,
            bitrate_kbps: 3500,
            audio: true,
        };
        command(&sender, target, SessionCommand::Subscribe(request.clone())).await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Subscribed {
                audio_owner: None,
                supports_input: false,
                ..
            }
        ));
        assert!(publisher.demand().active);
        assert!(!publisher.demand().audio);
        publisher.frame(vec![0, 0, 0, 1, 0x26, 0x01, 1, 2, 3], 0, false);
        let media = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let MultiplexedPacket::Data(envelope, _) = media else {
            panic!("expected encoded frame")
        };
        assert_eq!(envelope.header.stream_id, 256);
        assert_eq!(envelope.payload, vec![0, 0, 0, 1, 0x26, 0x01, 1, 2, 3]);
        command(
            &sender,
            target,
            SessionCommand::SetActivity {
                id: 256,
                revision: 2,
                video: false,
                audio: false,
            },
        )
        .await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Activity {
                revision: 2,
                video: false,
                ..
            }
        ));
        command(
            &sender,
            target,
            SessionCommand::SetActivity {
                id: 256,
                revision: 1,
                video: true,
                audio: true,
            },
        )
        .await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Activity {
                revision: 2,
                video: false,
                ..
            }
        ));
        assert!(!publisher.demand().active);
        command(&sender, target, SessionCommand::Unsubscribe { id: 256 }).await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Unsubscribed { .. }
        ));
        command(&sender, target, SessionCommand::Subscribe(request)).await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Error { .. }
        ));
        assert!(!publisher.demand().active);
    }

    struct ReceiverTestDir(PathBuf);
    impl ReceiverTestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "remote-play-receiver-{:032x}",
                u128::from_le_bytes(random_bytes_16())
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for ReceiverTestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    async fn receiver_client(
        publisher: &MobilePublisher,
        key: &[u8],
    ) -> (UdpSender, remote_core::net::UdpReceiver) {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, receiver) = mux.split();
        let nonce = random_bytes_16();
        let timestamp_ms = now_unix_ms();
        sender
            .send_control(
                &ControlMessage::SessionHello {
                    nonce,
                    timestamp_ms,
                    mac: mac_session_hello(key, &nonce, timestamp_ms),
                },
                publisher.address,
            )
            .await
            .unwrap();
        let packet = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let MultiplexedPacket::Control(
            ControlMessage::SessionAccept {
                salt,
                timestamp_ms,
                mac,
            },
            _,
        ) = packet
        else {
            panic!("missing handshake");
        };
        verify_session_mac(
            &mac_session_accept(key, &salt, timestamp_ms),
            &mac,
            timestamp_ms,
            now_unix_ms(),
        )
        .unwrap();
        sender.install_peer_crypto(
            publisher.address,
            SessionCrypto::from_psk(key, &salt).unwrap(),
        );
        command(
            &sender,
            publisher.address,
            SessionCommand::Open {
                connection_id: 17,
                version: SESSION_VERSION,
            },
        )
        .await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Opened {
                connection_id: 17,
                files: true,
                ..
            }
        ));
        (sender, receiver)
    }
    fn receiver_subscription(id: u32) -> SubscriptionRequest {
        SubscriptionRequest {
            id,
            source: CaptureSource::MainDisplay,
            width: 720,
            height: 1280,
            fps: 30,
            bitrate_kbps: 3500,
            audio: true,
        }
    }
    struct FileTestSender {
        commands: mpsc::Sender<FileTransferCommand>,
        incoming: mpsc::Sender<protocol::DataEnvelope>,
        events: mpsc::UnboundedReceiver<FileTransferEvent>,
        target: SocketAddr,
        cancel: broadcast::Sender<()>,
        tasks: Vec<tokio::task::AbortHandle>,
    }
    impl Drop for FileTestSender {
        fn drop(&mut self) {
            let _ = self.cancel.send(());
            for task in &self.tasks {
                task.abort();
            }
        }
    }
    impl FileTestSender {
        fn new(sender: &UdpSender, target: SocketAddr) -> Self {
            let (scheduled, worker) =
                ScheduledDataSender::spawn(sender.clone(), target, Default::default());
            let (commands, command_rx) = mpsc::channel(8);
            let (incoming, inbound) = mpsc::channel(128);
            let (events, event_rx) = mpsc::unbounded_channel();
            let (cancel, cancel_rx) = broadcast::channel(1);
            let task = tokio::spawn(run_file_transfer_runtime(
                scheduled,
                command_rx,
                inbound,
                events,
                cancel_rx,
                FileTransferRuntimeConfig::default(),
            ));
            Self {
                commands,
                incoming,
                events: event_rx,
                target,
                cancel,
                tasks: vec![worker.abort_handle(), task.abort_handle()],
            }
        }
        async fn deliver(&mut self, receiver: &remote_core::net::UdpReceiver, source: PathBuf) {
            self.commands
                .send(FileTransferCommand::SendFile {
                    path: source,
                    mime_type: None,
                })
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    tokio::select! {
                        event = self.events.recv() => match event {
                            Some(FileTransferEvent::OutgoingCompleted { .. }) => break,
                            Some(FileTransferEvent::Error {message,..}) => panic!("file transfer failed: {message}"),
                            None => panic!("file worker closed before receiver confirmation"),
                            _ => {}
                        },
                        packet = receiver.recv() => match packet.unwrap() {
                            MultiplexedPacket::Data(e,a) | MultiplexedPacket::DataWithTiming(e,_,a) if a == self.target => { self.incoming.send(e).await.unwrap(); },
                            _ => {}
                        }
                    }
                }
            }).await.expect("file receipt deadline");
        }
    }

    #[tokio::test]
    async fn receiver_requires_authentication_and_never_exposes_unauthorized_capture() {
        let directory = ReceiverTestDir::new();
        let key = b"file-receiver-test-key".to_vec();
        let publisher = MobilePublisher::start_with_permissions(
            "127.0.0.1:0".parse().unwrap(),
            directory.0.clone(),
            key.clone(),
            false,
            false,
        )
        .await
        .unwrap();
        let bad_mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (bad_sender, bad_receiver) = bad_mux.split();
        command(
            &bad_sender,
            publisher.address,
            SessionCommand::Open {
                connection_id: 7,
                version: SESSION_VERSION,
            },
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), bad_receiver.recv())
                .await
                .is_err()
        );
        let (sender, receiver) = receiver_client(&publisher, &key).await;
        command(
            &sender,
            publisher.address,
            SessionCommand::ListSources { request_id: 3 },
        )
        .await;
        assert!(
            matches!(reply(&receiver).await, SessionCommand::Sources {sources,..} if sources.is_empty())
        );
        command(
            &sender,
            publisher.address,
            SessionCommand::Subscribe(receiver_subscription(256)),
        )
        .await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Error {
                request_id: 256,
                ..
            }
        ));
        publisher.frame(vec![1, 2, 3], 0, false);
        publisher.frame(vec![1, 2, 3], 0, true);
        assert!(!publisher.demand().active);
        assert!(!publisher.demand().audio);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), receiver.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn receiver_files_survive_projection_start_and_stop() {
        let directory = ReceiverTestDir::new();
        let receive = directory.0.join("received");
        let key = b"receiver-lifecycle-test".to_vec();
        let publisher = MobilePublisher::start_with_permissions(
            "127.0.0.1:0".parse().unwrap(),
            receive.clone(),
            key.clone(),
            false,
            false,
        )
        .await
        .unwrap();
        let address = publisher.address;
        let (sender, receiver) = receiver_client(&publisher, &key).await;
        let contents: Vec<u8> = (0..131_072).map(|i| (i % 251) as u8).collect();
        let first = directory.0.join("before-sharing.bin");
        std::fs::write(&first, &contents).unwrap();
        let mut file_sender = FileTestSender::new(&sender, address);
        file_sender.deliver(&receiver, first).await;
        assert_eq!(
            std::fs::read(receive.join("before-sharing.bin")).unwrap(),
            contents
        );

        publisher.set_capture_available(true, false);
        assert!(
            matches!(reply(&receiver).await, SessionCommand::Sources {sources,..} if sources.len()==1)
        );
        command(
            &sender,
            address,
            SessionCommand::Subscribe(receiver_subscription(256)),
        )
        .await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Subscribed {
                id: 256,
                audio_owner: None,
                ..
            }
        ));
        assert!(publisher.demand().active);
        publisher.set_capture_available(false, false);
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Unsubscribed { id: 256 }
        ));
        assert!(
            matches!(reply(&receiver).await, SessionCommand::Sources {sources,..} if sources.is_empty())
        );
        assert!(!publisher.demand().active);
        assert_eq!(publisher.address, address);
        assert!(!publisher.is_finished());

        let second = directory.0.join("after-sharing.bin");
        let reversed: Vec<u8> = contents.iter().rev().copied().collect();
        std::fs::write(&second, &reversed).unwrap();
        file_sender.deliver(&receiver, second).await;
        assert_eq!(
            std::fs::read(receive.join("after-sharing.bin")).unwrap(),
            reversed
        );
        // Reopening the SAME connection id succeeds, proving the authenticated
        // connection was not replaced during the media permission transition.
        command(
            &sender,
            address,
            SessionCommand::Open {
                connection_id: 17,
                version: SESSION_VERSION,
            },
        )
        .await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Opened {
                connection_id: 17,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn old_capture_frames_cannot_cross_authorization_epochs() {
        let directory = ReceiverTestDir::new();
        let key = b"receiver-epoch-test".to_vec();
        let publisher = MobilePublisher::start_with_permissions(
            "127.0.0.1:0".parse().unwrap(),
            directory.0.clone(),
            key.clone(),
            true,
            false,
        )
        .await
        .unwrap();
        let (sender, receiver) = receiver_client(&publisher, &key).await;
        let old_epoch = publisher.permission.borrow().epoch;
        command(
            &sender,
            publisher.address,
            SessionCommand::Subscribe(receiver_subscription(256)),
        )
        .await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Subscribed { .. }
        ));
        publisher.set_capture_available(false, false);
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Unsubscribed { .. }
        ));
        let _ = reply(&receiver).await;
        publisher.set_capture_available(true, false);
        let _ = reply(&receiver).await;
        command(
            &sender,
            publisher.address,
            SessionCommand::Subscribe(receiver_subscription(512)),
        )
        .await;
        assert!(matches!(
            reply(&receiver).await,
            SessionCommand::Subscribed { id: 512, .. }
        ));
        publisher
            .frames
            .send(Frame {
                epoch: old_epoch,
                bytes: vec![9, 9, 9],
                timestamp: 0,
                audio: false,
            })
            .await
            .unwrap();
        publisher.frame(vec![0, 0, 0, 1, 0x26, 1, 7], 0, false);
        let packet = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let envelope = match packet {
            MultiplexedPacket::Data(e, _) | MultiplexedPacket::DataWithTiming(e, _, _) => e,
            _ => panic!("expected media"),
        };
        assert_eq!(envelope.header.stream_id, 512);
        assert_eq!(envelope.payload, vec![0, 0, 0, 1, 0x26, 1, 7]);
    }
}
