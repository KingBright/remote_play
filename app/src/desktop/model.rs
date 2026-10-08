//! Shared desktop session state. Network and transfer implementations stay in remote_core.
use super::requests::{FileRequests, RequestKind};
use client::{ClientMediaRuntime, TransferCenterState};
use protocol::{
    InputEvent,
    session::{CaptureSource, CaptureSourceInfo, SessionCommand, SubscriptionRequest},
    shared_files::{SharedFileInfo, SharedFileRequest, SharedFileResponse},
};
use remote_core::{
    SharedHostStats, Statistics,
    file_transfer_runtime::{FileTransferCommand, FileTransferEvent},
    workspace_session::{WorkspaceConnection, WorkspaceEvents},
};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;

/// One protocol/lifecycle implementation for both UI renderers. The type parameters
/// own presentation handles only; network identity and input safety are not duplicated.
pub struct SessionState<Texture, Pan: Default> {
    pub key: String,
    pub name: String,
    pub route: String,
    pub conn: Arc<WorkspaceConnection>,
    pub events: WorkspaceEvents,
    pub media: Option<ClientMediaRuntime>,
    #[cfg(any(all(target_os = "windows", feature = "native-windows-video"), all(target_os = "linux", feature = "native-linux-video")))]
    pub prefer_native_video: bool,
    pub stats: Arc<Statistics>,
    pub host_stats: Arc<SharedHostStats>,
    pub sources: Vec<CaptureSourceInfo>,
    pub source: CaptureSource,
    pub video_id: u32,
    source_revision: u32,
    pub confirmed: bool,
    pub supports_input: bool,
    pub locked: bool,
    pub paused: bool,
    pub pending: Option<(u32, Instant)>,
    pub activity_revision: u64,
    pub status: String,
    pub error: String,
    pub video_error: Option<String>,
    pub connected: bool,
    pub files: Vec<SharedFileInfo>,
    pub next_file_page: Option<u64>,
    pub file_pending: bool,
    pub transfers: Arc<Mutex<TransferCenterState>>,
    file_notices: tokio::sync::mpsc::Receiver<FileTransferEvent>,
    file_task: Option<JoinHandle<()>>,
    pub volume: u8,
    pub muted: bool,
    pub zoom: f32,
    pub pan: Pan,
    pub fit: bool,
    pub texture: Option<Texture>,
    pub uploaded_at: Option<Instant>,
    pub keys: BTreeSet<u16>,
    pub mouse: BTreeSet<u8>,
    pub modifiers: u8,
    pub clipboard: client::ClipboardRuntimeControl,
    pub clipboard_enabled: bool,
    pub request: SubscriptionRequest,
    reader_task: Option<JoinHandle<()>>,
    audio_task: Option<JoinHandle<()>>,
    pub background_paused: bool,
    activity_pending: Option<u64>,
    pub first_frame_after: Instant,
    file_requests: FileRequests,
    pub file_page_after: u64,
    pub file_history: Vec<u64>,
    next_request: u32,
    decoder_recovery_pending: bool,
}
#[cfg(feature = "gpui-restoration")]
pub(crate) type OriginalGuiSession =
    SessionState<Arc<client::MacDecodedVideoFrame>, gpui::Point<f32>>;

impl<Texture, Pan: Default> SessionState<Texture, Pan> {
    pub fn new(
        key: String,
        name: String,
        route: String,
        conn: WorkspaceConnection,
        mut events: WorkspaceEvents,
    ) -> Self {
        let conn = Arc::new(conn);
        let clipboard = client::start_clipboard_runtime_control(conn.sender.clone());
        let transfers = Arc::new(Mutex::new(TransferCenterState::default()));
        let state = transfers.clone();
        let (_unused, empty) = tokio::sync::mpsc::unbounded_channel();
        let mut incoming = std::mem::replace(&mut events.files, empty);
        let (notice_tx, file_notices) = tokio::sync::mpsc::channel(64);
        let file_task = tokio::spawn(async move {
            while let Some(event) = incoming.recv().await {
                {
                    let mut state = state.lock().unwrap();
                    state.apply_event(&event);
                    state.compact_history(128);
                }
                if matches!(
                    event,
                    FileTransferEvent::SharedResponse { .. }
                        | FileTransferEvent::Error { .. }
                        | FileTransferEvent::IncomingCompleted { .. }
                        | FileTransferEvent::OutgoingCompleted { .. }
                ) {
                    let _ = notice_tx.try_send(event);
                }
            }
        });
        Self {
            key,
            name,
            route,
            conn,
            events,
            media: None,
            #[cfg(any(all(target_os = "windows", feature = "native-windows-video"), all(target_os = "linux", feature = "native-linux-video")))]
            prefer_native_video: false,
            stats: Statistics::new(),
            host_stats: Arc::new(SharedHostStats::default()),
            sources: Vec::new(),
            source: CaptureSource::MainDisplay,
            video_id: 1,
            source_revision: 0,
            confirmed: false,
            supports_input: false,
            locked: true,
            paused: false,
            pending: None,
            activity_revision: 0,
            status: "Authenticated device connection".into(),
            error: String::new(),
            video_error: None,
            connected: true,
            files: Vec::new(),
            next_file_page: None,
            file_pending: false,
            transfers,
            file_notices,
            file_task: Some(file_task),
            volume: 100,
            muted: false,
            zoom: 1.,
            pan: Pan::default(),
            fit: true,
            texture: None,
            uploaded_at: None,
            keys: BTreeSet::new(),
            mouse: BTreeSet::new(),
            modifiers: 0,
            clipboard,
            clipboard_enabled: false,
            request: SubscriptionRequest {
                id: 1,
                source: CaptureSource::MainDisplay,
                width: 1920,
                height: 1080,
                fps: 30,
                bitrate_kbps: 8000,
                audio: true,
            },
            reader_task: None,
            audio_task: None,
            background_paused: false,
            activity_pending: None,
            first_frame_after: Instant::now(),
            file_requests: FileRequests::default(),
            file_page_after: 0,
            file_history: Vec::new(),
            next_request: 1000,
            decoder_recovery_pending: false,
        }
    }
    pub fn control(&mut self, command: SessionCommand) -> bool {
        if self.conn.try_control(command) {
            true
        } else {
            self.error = "Control queue is busy; release input and retry".into();
            self.locked = true;
            false
        }
    }
    pub fn start_video(&mut self) {
        if self.pending.is_some() || !self.connected {
            return;
        }
        self.video_error = None;
        if self.media.is_none() {
            let create = || {
                if self.request.audio {
                    ClientMediaRuntime::start(self.stats.clone())
                } else {
                    ClientMediaRuntime::start_video_only(self.stats.clone())
                }
            };
            #[cfg(any(all(target_os = "windows", feature = "native-windows-video"), all(target_os = "linux", feature = "native-linux-video")))]
            let result = if self.prefer_native_video {
                ClientMediaRuntime::start_native(
                    self.stats.clone(),
                    self.request.audio,
                    self.request.fps,
                )
            } else {
                create()
            };
            #[cfg(not(any(all(target_os = "windows", feature = "native-windows-video"), all(target_os = "linux", feature = "native-linux-video"))))]
            let result = create();
            match result {
                Ok(media) => self.media = Some(media),
                Err(e) => {
                    self.fail_video(e.to_string());
                    return;
                }
            }
            let media = self.media.as_ref().unwrap();
            let player = media.audio_tx.clone();
            let (_unused, dummy) = tokio::sync::mpsc::channel(1);
            let mut audio = std::mem::replace(&mut self.events.audio, dummy);
            self.audio_task = Some(tokio::spawn(async move {
                while let Some(event) = audio.recv().await {
                    let event = match event {
                        remote_core::client_session::AudioIngressEvent::Packet(p) => {
                            client::audio_player::AudioPlayerEvent::Packet(p)
                        }
                        remote_core::client_session::AudioIngressEvent::StreamConfig(c) => {
                            client::audio_player::AudioPlayerEvent::StreamConfig(c)
                        }
                    };
                    if player.send(event).await.is_err() {
                        break;
                    }
                }
            }));
            if let client::ClientMediaRuntimeStatus::AudioUnavailable { reason } = &media.status {
                self.error = format!("Video ready; audio unavailable: {reason}");
            }
        }
        self.confirmed = false;
        self.locked = true;
        self.error.clear();
        self.status = "Requesting video; waiting for the first decoded frame".into();
        self.request.source = self.source;
        self.request.id = self.video_id;
        self.pending = Some((self.video_id, Instant::now()));
        let conn = self.conn.clone();
        let request = self.request.clone();
        let video = self.media.as_ref().unwrap().decode_tx.clone();
        let stats = self.stats.clone();
        let host = self.host_stats.clone();
        self.reader_task = Some(tokio::spawn(async move {
            if let Err(e) = conn.subscribe(request, video, stats, host).await {
                eprintln!("subscription request failed: {e}");
            }
        }));
        self.control(SessionCommand::ListSources {
            request_id: 900_001,
        });
    }
    pub fn switch_source(&mut self, source: CaptureSource) {
        if source == self.source || self.pending.is_some() {
            return;
        }
        self.release_input();
        self.next_request = self.next_request.wrapping_add(1).max(1000);
        let request = self.next_request;
        if self.control(SessionCommand::SwitchSource {
            id: self.video_id,
            request_id: request,
            source,
        }) {
            self.pending = Some((request, Instant::now()));
            self.confirmed = false;
            self.locked = true;
            self.status = "Switching source".into();
        }
    }
    pub fn set_paused(&mut self, paused: bool) {
        self.release_input();
        self.paused = paused;
        self.send_activity();
    }
    pub fn set_background(&mut self, hidden: bool) {
        if self.background_paused != hidden {
            self.release_input();
            self.background_paused = hidden;
            if self.confirmed {
                self.send_activity();
            }
        }
    }
    fn send_activity(&mut self) {
        self.activity_revision = self.activity_revision.wrapping_add(1);
        self.activity_pending = Some(self.activity_revision);
        let active = !self.paused && !self.background_paused;
        self.control(SessionCommand::SetActivity {
            id: self.video_id,
            revision: self.activity_revision,
            video: active,
            audio: active && !self.muted,
        });
    }
    pub fn media_activity_pending(&self) -> bool {
        self.activity_pending.is_some()
    }

    pub fn audio_settings(&mut self) {
        if let Some(media) = &self.media {
            media
                .audio_playback
                .set_settings(client::audio_player::AudioPlayerSettings {
                    volume_percent: self.volume,
                    remote_system_muted: self.muted,
                    remote_microphone_muted: true,
                });
        }
    }
    pub fn set_clipboard(&mut self, enabled: bool) {
        if !self.conn.clipboard_available {
            return;
        }
        self.clipboard_enabled = enabled;
        let conn = self.conn.clone();
        let clip = self.clipboard.clone();
        let gate = Arc::new(AtomicBool::new(true));
        tokio::spawn(async move {
            if enabled {
                clip.start_workspace(conn.target, gate);
                let _ = conn.clipboard(Some(Arc::new(clip))).await;
            } else {
                clip.stop();
                let _ = conn.clipboard(None).await;
            }
            let _ = conn.control(SessionCommand::SetClipboard { enabled }).await;
        });
    }
    pub fn release_input(&mut self) {
        if (!self.keys.is_empty() || !self.mouse.is_empty() || self.modifiers != 0)
            && !self
                .conn
                .try_control(SessionCommand::ReleaseInput { id: self.video_id })
        {
            let conn = self.conn.clone();
            let id = self.video_id;
            tokio::spawn(async move {
                let _ = conn.control(SessionCommand::ReleaseInput { id }).await;
            });
        }
        self.keys.clear();
        self.mouse.clear();
        self.modifiers = 0;
    }
    fn fail_video(&mut self, reason: String) {
        self.release_input();
        self.pending = None;
        self.confirmed = false;
        self.locked = true;
        self.texture = None;
        self.uploaded_at = None;
        self.video_error = Some(reason.clone());
        self.error = reason;
        self.status = "Video request failed; file connection remains available".into();
    }
    pub fn input_status(&self) -> &'static str {
        if self.video_error.is_some() || !self.connected {
            "Video/connection unavailable: control disabled"
        } else if !self.confirmed || self.pending.is_some() {
            "Waiting for the host to confirm this source"
        } else if !self.supports_input {
            "This source is view-only on this host; its window input is not available"
        } else if self.paused || self.background_paused || self.activity_pending.is_some() {
            "Control is disabled while video is paused or resuming"
        } else if self
            .stats
            .video_decoder_needs_keyframe
            .load(std::sync::atomic::Ordering::Acquire)
        {
            "Recovering video references: control waits for a newly displayed frame"
        } else if !self.conn.peer_is_responsive() {
            "Host heartbeat is stale: control disabled"
        } else if !current_input_frame(self.uploaded_at, self.first_frame_after, true) {
            "Waiting for a frame from the current source"
        } else if self.locked {
            "View only: click Enable control, then click the remote picture"
        } else {
            "Mouse and keyboard enabled; Ctrl+Alt+Esc locks control"
        }
    }
    pub fn can_input(&self) -> bool {
        self.video_error.is_none()
            && !self
                .stats
                .video_decoder_needs_keyframe
                .load(std::sync::atomic::Ordering::Acquire)
            && self.connected
            && self.confirmed
            && self.pending.is_none()
            && self.supports_input
            && !self.locked
            && !self.paused
            && !self.background_paused
            && self.activity_pending.is_none()
            && current_input_frame(
                self.uploaded_at,
                self.first_frame_after,
                self.conn.peer_is_responsive(),
            )
    }
    pub fn input_command(&self, event: InputEvent) -> SessionCommand {
        if matches!(self.source, CaptureSource::Window(_)) {
            SessionCommand::SourceInput {
                id: self.video_id,
                source_revision: self.source_revision,
                event,
            }
        } else {
            SessionCommand::Input {
                id: self.video_id,
                event,
            }
        }
    }
    pub fn input(&mut self, event: InputEvent) {
        if self.can_input() {
            self.control(self.input_command(event));
        }
    }
    pub fn files_command(&mut self, command: FileTransferCommand) -> bool {
        if !self.conn.files_available {
            self.error = "This peer does not support files".into();
            return false;
        }
        if self.conn.file_commands.try_send(command).is_err() {
            self.error = "File queue is busy".into();
            return false;
        }
        true
    }
    fn shared_request(&mut self, kind: RequestKind, request: SharedFileRequest) {
        if !self.connected || !self.conn.files_available {
            self.error = "This connection cannot transfer files".into();
            return;
        }
        let id = match self.file_requests.begin(kind, Instant::now()) {
            Ok(id) => id,
            Err(reason) => {
                self.error = reason.into();
                return;
            }
        };
        if !self.files_command(FileTransferCommand::SharedRequest {
            request_id: id,
            request,
        }) {
            self.file_requests.cancel(id);
        }
        self.file_pending = self.file_requests.list_pending();
    }
    pub fn list_files(&mut self, after: u64) {
        self.shared_request(
            RequestKind::List { after },
            SharedFileRequest::List { after_id: after },
        );
    }
    pub fn fetch_file(&mut self, file: u64) {
        if !self.files.iter().any(|f| f.id == file) {
            self.error = "Refresh the shared list before pulling this file".into();
            return;
        }
        self.shared_request(
            RequestKind::Fetch { file },
            SharedFileRequest::Fetch { file_id: file },
        );
    }
    pub fn poll(&mut self) {
        let decoder_waiting = self
            .stats
            .video_decoder_needs_keyframe
            .load(std::sync::atomic::Ordering::Acquire);
        if decoder_waiting && !self.decoder_recovery_pending {
            self.release_input();
            self.first_frame_after = Instant::now();
            self.uploaded_at = None;
        }
        self.decoder_recovery_pending = decoder_waiting;
        if !self.can_input()
            && (!self.keys.is_empty() || !self.mouse.is_empty() || self.modifiers != 0)
        {
            self.release_input();
        }
        if self.file_requests.expire(Instant::now()) != 0 {
            self.file_pending = self.file_requests.list_pending();
            self.error = "A shared-file request timed out; retry or update the peer".into();
        }
        for _ in 0..128 {
            let Ok(event) = self.events.control.try_recv() else {
                break;
            };
            match event {
                SessionCommand::Sources { sources, .. } => self.sources = sources,
                SessionCommand::Subscribed {
                    id, supports_input, ..
                } if id == self.video_id
                    && self.pending.is_some_and(|p| p.0 == self.video_id)
                    && self.video_error.is_none() =>
                {
                    self.confirmed = true;
                    self.supports_input = supports_input;
                    self.pending = None;
                    self.first_frame_after = Instant::now();
                    self.status = "Video accepted; waiting for its first decoded frame".into();
                    if self.paused || self.background_paused {
                        self.send_activity();
                    }
                }
                SessionCommand::SourceSwitched {
                    id,
                    request_id,
                    source,
                    supports_input,
                } if id == self.video_id && self.pending.is_some_and(|p| p.0 == request_id) => {
                    self.source = source;
                    self.source_revision = request_id;
                    self.request.source = source;
                    self.supports_input = supports_input;
                    self.pending = None;
                    self.confirmed = true;
                    self.uploaded_at = None;
                    self.texture = None;
                    self.first_frame_after = Instant::now();
                    if let Some(media) = &self.media {
                        media.reset_video();
                    }
                    let conn = self.conn.clone();
                    let id = self.video_id;
                    tokio::spawn(async move {
                        let _ = conn
                            .sender
                            .send_control(
                                &protocol::ControlMessage::RequestKeyframe { session_id: id },
                                conn.target,
                            )
                            .await;
                    });
                    self.status = "Source accepted; waiting for its first frame".into();
                }
                SessionCommand::Activity {
                    id,
                    revision,
                    video,
                    ..
                } if id == self.video_id
                    && revision >= self.activity_revision
                    && self.activity_pending.is_some_and(|r| revision >= r) =>
                {
                    self.activity_pending = None;
                    if video {
                        self.first_frame_after = Instant::now();
                    } else if !self.paused && !self.background_paused {
                        self.locked = true;
                        self.error =
                            "The host did not resume video; retry before enabling input".into();
                    }
                }
                SessionCommand::Closed { reason, .. } => {
                    self.release_input();
                    self.connected = false;
                    self.confirmed = false;
                    self.error = reason;
                }
                SessionCommand::Error { request_id, reason }
                    if request_id == self.video_id && reason.starts_with("INPUT_UNAVAILABLE:") =>
                {
                    self.release_input();
                    self.locked = true;
                    self.error = reason
                        .trim_start_matches("INPUT_UNAVAILABLE:")
                        .trim()
                        .to_owned();
                    self.status="Application control paused; video remains available. Correct the host condition and enable control again.".into();
                }
                SessionCommand::Error { request_id, reason } => {
                    let source_permission_failed = request_id == 900_001
                        && self.media.is_some()
                        && self.uploaded_at.is_none()
                        && capture_permission_failure(&reason);
                    if request_id == self.video_id
                        || self.pending.is_some_and(|p| p.0 == request_id)
                        || source_permission_failed
                    {
                        self.fail_video(reason);
                    } else {
                        self.error = reason;
                    }
                }
                _ => {}
            }
        }
        if self
            .pending
            .is_some_and(|p| p.1.elapsed() > Duration::from_secs(10))
        {
            self.fail_video(
                "Host did not confirm the video request; reconnect or update the peer".into(),
            );
        }
        for _ in 0..256 {
            let Ok(event) = self.events.audio.try_recv() else {
                break;
            };
            if let Some(media) = &self.media {
                let e = match event {
                    remote_core::client_session::AudioIngressEvent::Packet(p) => {
                        client::audio_player::AudioPlayerEvent::Packet(p)
                    }
                    remote_core::client_session::AudioIngressEvent::StreamConfig(c) => {
                        client::audio_player::AudioPlayerEvent::StreamConfig(c)
                    }
                };
                let _ = media.audio_tx.try_send(e);
            }
        }
        for _ in 0..256 {
            let Ok(event) = self.file_notices.try_recv() else {
                break;
            };
            match event {
                FileTransferEvent::SharedResponse {
                    request_id,
                    response,
                } => {
                    let Some(kind) = self.file_requests.resolve(request_id, &response) else {
                        continue;
                    };
                    self.file_pending = self.file_requests.list_pending();
                    match response {
                        SharedFileResponse::Page {
                            entries,
                            next_after_id,
                        } => {
                            if let RequestKind::List { after } = kind {
                                if after == 0 {
                                    self.file_history.clear();
                                } else if self.file_history.last() == Some(&after) {
                                    self.file_history.pop();
                                } else if after != self.file_page_after {
                                    self.file_history.push(self.file_page_after);
                                }
                                self.file_page_after = after;
                            }
                            self.files = entries;
                            self.next_file_page = next_after_id;
                            self.error.clear();
                        }
                        SharedFileResponse::Queued { .. } => {
                            self.status = "Pull accepted; awaiting verified file delivery".into()
                        }
                        SharedFileResponse::Rejected { message } => self.error = message,
                    }
                }
                FileTransferEvent::IncomingCompleted { size_bytes, .. } => {
                    self.status = format!("File received, verified and saved ({size_bytes} bytes)");
                }
                FileTransferEvent::OutgoingCompleted { .. } => {
                    self.status = "File delivered; receiver confirmed completion".into();
                }
                FileTransferEvent::Error { message, .. } => {
                    self.error = message;
                }
                _ => {}
            }
        }
    }
}
impl<Texture, Pan: Default> Drop for SessionState<Texture, Pan> {
    fn drop(&mut self) {
        self.release_input();
        self.clipboard.stop();
        if let Some(task) = self.file_task.take() {
            task.abort();
        }
        if let Some(task) = self.audio_task.take() {
            task.abort();
        }
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
        self.conn
            .try_control(SessionCommand::Unsubscribe { id: self.video_id });
    }
}

pub fn candidate_routes(
    snapshot: &remote_core::discovery::DiscoveryPeerSnapshot,
    device_id: &str,
) -> Vec<(String, std::net::SocketAddr)> {
    use remote_core::discovery::DiscoveryScope;
    let mut routes: Vec<_> = snapshot
        .peers()
        .iter()
        .filter(|p| p.announcement.device_id == device_id && p.announcement.control_port != 0)
        .map(|p| {
            (
                match p.scope {
                    DiscoveryScope::Lan => 0,
                    DiscoveryScope::P2p => 1,
                    DiscoveryScope::Relay => 2,
                    _ => 3,
                },
                format!("{:?}", p.scope),
                p.endpoint,
            )
        })
        .collect();
    routes.sort_by_key(|p| p.0);
    routes.dedup_by_key(|p| p.2);
    routes.into_iter().map(|p| (p.1, p.2)).collect()
}

/// Classify the host error without claiming the user forgot to grant permission.
fn capture_permission_failure(reason: &str) -> bool {
    let reason = reason.to_lowercase();
    reason.contains("tcc")
        || reason.contains("code requirement")
        || (reason.contains("capture") && reason.contains("permission"))
}

pub fn video_failure_guidance(reason: &str) -> &'static str {
    let lower = reason.to_lowercase();
    if lower.contains("code requirement") || lower.contains("signing identity") {
        "The running host does not match its authorized signing identity. Install the correctly signed RemotePlay release on that host and restart it. Do not reset unrelated privacy permissions."
    } else if capture_permission_failure(reason) {
        "The host could not use its screen-recording authorization. Permission may already be enabled: check the actual running app and its signing identity, then restart that app. Files remain available."
    } else {
        "The host did not provide a usable video stream. Check the host error below and reconnect after resolving it. Files remain available."
    }
}

#[cfg(test)]
mod capture_error_tests {
    use super::*;
    #[test]
    fn tcc_does_not_blame_missing_user_authorization() {
        let error = "No shareable content available: 用户拒绝了应用程序、窗口、显示器捕捉的TCC";
        assert!(capture_permission_failure(error));
        let hint = video_failure_guidance(error);
        assert!(hint.contains("may already be enabled"));
        assert!(hint.contains("signing identity"));
    }
    #[test]
    fn signing_mismatch_has_specific_remedy() {
        assert!(
            video_failure_guidance("Failed to match existing code requirement")
                .contains("correctly signed")
        );
    }
    #[test]
    fn network_failure_is_not_labeled_permission_failure() {
        assert!(!capture_permission_failure("Connection timed out"));
        assert!(!video_failure_guidance("Connection timed out").contains("authorization"));
    }
}

fn current_input_frame(frame: Option<Instant>, source_started: Instant, responsive: bool) -> bool {
    responsive && frame.is_some_and(|decoded| decoded >= source_started)
}
#[cfg(test)]
mod static_input_tests {
    use super::*;
    #[test]
    fn unchanged_screen_does_not_disable_input() {
        let now = Instant::now();
        let start = now - Duration::from_secs(1200);
        let decoded = now - Duration::from_secs(600);
        assert!(current_input_frame(Some(decoded), start, true));
    }
    #[test]
    fn stale_connection_or_old_source_cannot_receive_input() {
        let now = Instant::now();
        assert!(!current_input_frame(Some(now), now, false));
        assert!(!current_input_frame(
            Some(now - Duration::from_secs(1)),
            now,
            true
        ));
        assert!(!current_input_frame(None, now, true));
    }
}
