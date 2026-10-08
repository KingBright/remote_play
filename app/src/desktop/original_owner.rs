//! Adapter between the unchanged original product controls and current safe sessions.
//! This owns no discovery/host runtime of its own and does not use legacy StartStream.
use super::model::{OriginalGuiSession, candidate_routes};
use crate::{AppDevice, StreamStartOptions, UnifiedServiceOwner};
use client::{MacDecodedVideoFrame, TransferEntrySnapshot};
use protocol::{
    InputEvent,
    session::{CaptureSource, CaptureSourceInfo, SessionCommand},
};
use remote_core::{
    file_transfer_runtime::FileTransferCommand,
    role::{RolePeer, RoleSession, RoleState},
    shared_files::ShareScope,
    workspace_session::WorkspaceConnection,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Instant,
};

type Error = Box<dyn std::error::Error + Send + Sync>;

// A delayed result must never remove the replacement request for the same device.
fn retire_attempt(pending: &mut BTreeMap<String, u64>, key: &str, attempt: u64) -> bool {
    if pending.get(key) != Some(&attempt) {
        return false;
    }
    pending.remove(key);
    true
}

pub(crate) type FrameSlot = Arc<Mutex<Option<Arc<MacDecodedVideoFrame>>>>;

#[derive(Clone)]
pub(crate) struct FileViewBinding {
    connection: Weak<WorkspaceConnection>,
    scope: ShareScope,
}
fn view_binding_matches<T>(
    connection: &Weak<T>,
    active: &Arc<T>,
    scope: ShareScope,
    current: ShareScope,
) -> bool {
    scope == current
        && connection
            .upgrade()
            .is_some_and(|previous| Arc::ptr_eq(&previous, active))
}

struct ViewSession {
    session: OriginalGuiSession,
    frame: FrameSlot,
    source_seen: CaptureSource,
    frame_observer: Option<tokio::task::JoinHandle<()>>,
    sources: Arc<Vec<CaptureSourceInfo>>,
}
impl Drop for ViewSession {
    fn drop(&mut self) {
        if let Some(task) = self.frame_observer.take() {
            task.abort();
        }
        *self.frame.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
struct Pool {
    sessions: Vec<ViewSession>,
    selected: usize,
    pending: BTreeMap<String, u64>,
    intent: Option<String>,
    next_attempt: u64,
    connecting: Option<RoleSession>,
    message: String,
    receive_dir: PathBuf,
    talkback: Option<(u32, client::TalkbackRuntimeControl)>,
    clipboard_preference: bool,
}
impl Pool {
    fn active(&self) -> Option<&ViewSession> {
        if self.connecting.is_some() {
            None
        } else {
            self.sessions.get(self.selected)
        }
    }
    fn active_mut(&mut self) -> Option<&mut ViewSession> {
        if self.connecting.is_some() {
            None
        } else {
            self.sessions.get_mut(self.selected)
        }
    }
}

pub(crate) struct OriginalOwner {
    backend: Arc<UnifiedServiceOwner>,
    pool: Mutex<Pool>,
    updates: tokio::sync::watch::Sender<u64>,
}
#[derive(Clone)]
pub(crate) struct ViewSnapshot {
    pub role: RoleState,
    pub devices: Vec<AppDevice>,
    pub sources: Arc<Vec<CaptureSourceInfo>>,
    pub active_source: CaptureSource,
    pub pending_source: Option<CaptureSource>,
    pub supports_input: bool,
    pub error: Option<String>,
    pub locked: bool,
    pub connection_id: Option<u32>,
    pub source_epoch: Option<Instant>,
    pub message: String,
    pub input_status: String,
}
impl OriginalOwner {
    pub fn new(backend: Arc<UnifiedServiceOwner>) -> Arc<Self> {
        let prefs = std::fs::read(super::prefs_path())
            .ok()
            .and_then(|b| serde_json::from_slice::<super::Preferences>(&b).ok())
            .unwrap_or_default();
        let (updates, _) = tokio::sync::watch::channel(0);
        Arc::new(Self {
            backend,
            updates,
            pool: Mutex::new(Pool {
                sessions: Vec::new(),
                selected: 0,
                pending: BTreeMap::new(),
                intent: None,
                next_attempt: 0,
                connecting: None,
                message: "Ready".into(),
                receive_dir: prefs.receive_dir,
                talkback: None,
                clipboard_preference: false,
            }),
        })
    }
    pub fn subscribe_updates(&self) -> tokio::sync::watch::Receiver<u64> {
        self.updates.subscribe()
    }
    pub fn poll(&self) {
        let Ok(mut pool) = self.pool.try_lock() else {
            return;
        };
        let selected = pool.selected;
        let mut retired: [Option<Arc<MacDecodedVideoFrame>>; 8] = std::array::from_fn(|_| None);
        let transitioning = pool.connecting.is_some();
        for (index, entry) in pool.sessions.iter_mut().enumerate() {
            entry
                .session
                .set_background(transitioning || index != selected);
            entry.session.poll();
            let s = &mut entry.session;
            if entry.sources.as_ref() != &s.sources {
                entry.sources = Arc::new(s.sources.clone());
            }
            if entry.frame_observer.is_none()
                && let Some(media) = &s.media
            {
                let signal = media.frame_updates();
                let updates = self.updates.clone();
                entry.frame_observer = Some(tokio::spawn(async move {
                    loop {
                        signal.notified().await;
                        updates.send_modify(|v| *v = v.wrapping_add(1));
                    }
                }));
            }
            if !s.confirmed
                || s.video_error.is_some()
                || !s.connected
                || s.source != entry.source_seen
            {
                *entry.frame.lock().unwrap_or_else(|e| e.into_inner()) = None;
                entry.source_seen = s.source;
            }
            // Observe the exact prior native frame's submission before replacing
            // it with a newer frame. Continuous video must not starve the input
            // gate, nor may stream-wide counters unlock an unpainted new source.
            #[cfg(any(all(target_os = "windows", feature = "native-windows-video"), all(target_os = "linux", feature = "native-linux-video")))]
            if let Some(frame) = s.texture.clone() {
                super::original_presenter::acknowledge_paint(s, &frame);
            }
            if s.confirmed
                && !s.paused
                && !s.background_paused
                && s.video_error.is_none()
                && let Some(media) = &s.media
            {
                let shared = media.shared_frame();
                let next = shared.try_lock().ok().and_then(|mut f| f.take());
                if let Some(frame) = next
                    && frame.decoded_at >= s.first_frame_after
                {
                    let frame = Arc::new(frame);
                    s.texture = Some(frame.clone());
                    let previous = entry
                        .frame
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .replace(frame);
                    retired[index] = previous;
                }
            }
        }
        let selected = pool.selected;
        if pool.sessions.get(selected).is_some_and(|s| {
            !s.session.connected || s.session.paused || s.session.background_paused
        }) && let Some((_, control)) = pool.talkback.take()
        {
            control.stop();
        }
        drop(pool);
        drop(retired);
    }
    pub fn snapshot(&self) -> ViewSnapshot {
        let devices = self.backend.runtime().lock().unwrap().devices();
        let p = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(connecting) = &p.connecting {
            return ViewSnapshot {
                role: RoleState::Connecting(connecting.clone()),
                devices,
                sources: Arc::new(Vec::new()),
                active_source: CaptureSource::MainDisplay,
                pending_source: None,
                supports_input: false,
                error: None,
                locked: true,
                connection_id: None,
                source_epoch: None,
                message: p.message.clone(),
                input_status: "Connecting to selected device".into(),
            };
        }
        if let Some(e) = p.active() {
            let s = &e.session;
            let role_session =
                RoleSession::new(RolePeer::new(&s.key, &s.name, s.conn.target), s.conn.id, 0);
            let role = if s.confirmed && s.media.is_some() {
                RoleState::Viewing(role_session)
            } else {
                RoleState::Connecting(role_session)
            };
            ViewSnapshot {
                role,
                devices,
                sources: e.sources.clone(),
                active_source: s.source,
                pending_source: s.pending.map(|_| s.request.source),
                supports_input: s.supports_input && s.connected,
                error: active_video_error(s.video_error.as_deref(), s.media.as_ref().and_then(|m|m.decode_status.try_lock().ok().map(|v|v.clone())).as_deref()),
                locked: s.locked,
                connection_id: Some(s.conn.id),
                source_epoch: Some(s.first_frame_after),
                message: s.status.clone(),
                input_status: if s.error.is_empty() {
                    s.input_status().into()
                } else {
                    s.error.clone()
                },
            }
        } else {
            let role = self.backend.runtime().lock().unwrap().role_state().clone();
            ViewSnapshot {
                role,
                devices,
                sources: Arc::new(Vec::new()),
                active_source: CaptureSource::MainDisplay,
                pending_source: None,
                supports_input: false,
                error: None,
                locked: true,
                connection_id: None,
                source_epoch: None,
                message: p.message.clone(),
                input_status: "Choose a device".into(),
            }
        }
    }
    pub fn can_change_network(&self) -> bool {
        let p = self.pool.lock().unwrap();
        p.sessions.is_empty() && p.pending.is_empty()
    }
    pub fn sessions(&self) -> Vec<(u32, String, bool)> {
        let p = self.pool.lock().unwrap();
        p.sessions
            .iter()
            .enumerate()
            .map(|(i, e)| (e.session.conn.id, e.session.name.clone(), i == p.selected))
            .collect()
    }
    pub fn select(&self, id: u32) {
        let mut p = self.pool.lock().unwrap();
        if let Some(index) = p.sessions.iter().position(|e| e.session.conn.id == id) {
            if let Some(e) = p.active_mut() {
                e.session.release_input();
            }
            if let Some((_, c)) = p.talkback.take() {
                c.stop();
            }
            p.selected = index;
            p.intent = Some(p.sessions[index].session.key.clone());
            p.connecting = None;
        }
    }
    pub fn active_connection(&self) -> Option<Arc<WorkspaceConnection>> {
        let p = self.pool.lock().unwrap();
        p.active().map(|e| e.session.conn.clone())
    }
    pub fn frame_binding(&self) -> Option<(u32, FrameSlot)> {
        let p = self.pool.lock().unwrap();
        p.active().map(|e| (e.session.conn.id, e.frame.clone()))
    }
    pub fn stats(&self) -> Option<Arc<remote_core::SharedHostStats>> {
        let p = self.pool.lock().unwrap();
        p.active().map(|e| e.session.host_stats.clone())
    }
    pub fn acknowledge_paint(&self, connection: u32, frame: &Arc<MacDecodedVideoFrame>) {
        let mut p = self.pool.lock().unwrap();
        let Some(e) = p
            .sessions
            .iter_mut()
            .find(|e| e.session.conn.id == connection)
        else {
            return;
        };
        super::original_presenter::acknowledge_paint(&mut e.session, frame);
    }
    pub async fn connect_device(
        &self,
        key: &str,
        options: StreamStartOptions,
        _now: u64,
    ) -> Result<(), Error> {
        self.connect_mode(key, options, true, true).await
    }
    pub async fn connect_files(&self, key: &str) -> Result<(), Error> {
        self.connect_mode(key, StreamStartOptions::default(), false, true)
            .await
    }

    pub async fn connect_silent_files(&self, key: &str) -> Result<(), Error> {
        self.connect_mode(key, StreamStartOptions::default(), false, false)
            .await
    }
    async fn connect_mode(
        &self,
        key: &str,
        options: StreamStartOptions,
        video: bool,
        audio: bool,
    ) -> Result<(), Error> {
        protocol::validate_video_settings(
            options.width,
            options.height,
            options.fps,
            options.bitrate_kbps,
        )?;
        let device = self
            .backend
            .runtime()
            .lock()
            .unwrap()
            .devices()
            .into_iter()
            .find(|d| d.device_id == key && d.online)
            .ok_or("Device is not currently online")?;
        let routes = self
            .backend
            .discovery_snapshot_rx()
            .map(|rx| candidate_routes(&rx.borrow(), key))
            .unwrap_or_default();
        if routes.is_empty() {
            return Err("No current route to the selected device".into());
        }
        let (attempt, receive_dir) = {
            let mut p = self.pool.lock().unwrap();
            p.intent = Some(key.to_owned());
            if let Some(index) = p.sessions.iter().position(|e| e.session.key == key) {
                let s = &p.sessions[index].session;
                if super::reusable_connection(
                    s.connected,
                    s.conn.peer_is_responsive(),
                    s.video_error.is_some(),
                ) {
                    if let Some(e) = p.active_mut() {
                        e.session.release_input();
                    }
                    p.selected = index;
                    p.connecting = None;
                    if let Some((_, c)) = p.talkback.take() {
                        c.stop();
                    }
                    if video && p.sessions[index].session.media.is_none() {
                        let session = &mut p.sessions[index].session;
                        session.request.width = options.width;
                        session.request.height = options.height;
                        session.request.fps = options.fps;
                        session.request.bitrate_kbps = options.bitrate_kbps;
                        session.request.audio = audio;
                        session.start_video();
                    }
                    if !video {
                        p.sessions[index].session.list_files(0);
                    }
                    return Ok(());
                }
                p.sessions.remove(index);
                p.selected = super::selection_after_close(p.selected, index, p.sessions.len());
            }
            if p.pending.contains_key(key) {
                return Ok(());
            }
            if p.sessions.len() + p.pending.len() >= 8 {
                return Err("Close an unused connection before opening another (maximum 8)".into());
            }
            p.next_attempt = p
                .next_attempt
                .checked_add(1)
                .ok_or("Connection generation exhausted")?;
            let attempt = p.next_attempt;
            p.pending.insert(key.to_owned(), attempt);
            let previous = p.selected;
            if let Some(e) = p.sessions.get_mut(previous) {
                e.session.release_input();
                e.session.set_background(true);
            }
            p.connecting = Some(RoleSession::new(device.role_peer(), attempt as u32, 0));
            p.message = format!("Connecting to {}", device.display_name);
            (attempt, p.receive_dir.clone())
        };
        let mut failures = Vec::new();
        let mut connected = None;
        for (route, endpoint) in routes {
            match WorkspaceConnection::connect(endpoint, receive_dir.clone()).await {
                Ok(c) => {
                    connected = Some((route, c));
                    break;
                }
                Err(e) => failures.push(format!("{route}: {e}")),
            }
        }
        let mut p = self.pool.lock().unwrap();
        if !retire_attempt(&mut p.pending, key, attempt) {
            drop(connected);
            return Err("Connection request was cancelled or superseded".into());
        }
        let selected = p.intent.as_deref() == Some(key);
        if selected {
            p.connecting = None;
        }
        let Some((route, (conn, events))) = connected else {
            if selected {
                // Keep earlier sessions available as tabs, but never show one as
                // the newly requested device after its connection failed.
                p.selected = p.sessions.len();
            }
            p.message = format!(
                "Could not connect to {}: {}",
                device.display_name,
                failures.join("; ")
            );
            return Err(p.message.clone().into());
        };
        let mut session =
            OriginalGuiSession::new(key.into(), device.display_name, route, conn, events);
        #[cfg(any(all(target_os = "windows", feature = "native-windows-video"), all(target_os = "linux", feature = "native-linux-video")))]
        {
            session.prefer_native_video = true;
        }
        session.request.audio = audio;
        session.request.width = options.width;
        session.request.height = options.height;
        session.request.fps = options.fps;
        session.request.bitrate_kbps = options.bitrate_kbps;
        session.set_clipboard(p.clipboard_preference);
        if video {
            session.start_video();
        } else {
            session.list_files(0);
        }
        let previous = p.selected;
        if selected && let Some(e) = p.sessions.get_mut(previous) {
            e.session.release_input();
        }
        p.sessions.push(ViewSession {
            session,
            frame: Arc::new(Mutex::new(None)),
            source_seen: CaptureSource::MainDisplay,
            frame_observer: None,
            sources: Arc::new(Vec::new()),
        });
        if selected || p.sessions.len() == 1 {
            p.selected = p.sessions.len() - 1;
        }
        Ok(())
    }
    pub async fn disconnect_active(&self) -> Result<(), Error> {
        let mut p = self.pool.lock().unwrap();
        let index = p.selected;
        let cancelling_connection = p.connecting.take().is_some();
        if let Some(key) = p.intent.take() {
            p.pending.remove(&key);
        }
        // Cancelling a pending B connection must not destroy the existing A tab.
        if !cancelling_connection && index < p.sessions.len() {
            p.sessions.remove(index);
            p.selected = super::selection_after_close(index, index, p.sessions.len());
        }
        if let Some((_, c)) = p.talkback.take() {
            c.stop();
        }
        Ok(())
    }
    pub async fn reconnect_active(&self) -> Result<(), Error> {
        let (key, opts) = {
            let p = self.pool.lock().unwrap();
            let s = &p
                .sessions
                .get(p.selected)
                .ok_or("No selected device")?
                .session;
            (
                s.key.clone(),
                StreamStartOptions {
                    width: s.request.width,
                    height: s.request.height,
                    fps: s.request.fps,
                    bitrate_kbps: s.request.bitrate_kbps,
                },
            )
        };
        self.disconnect_active().await?;
        self.connect_device(&key, opts, 0).await
    }
    pub async fn request_capture_sources(&self) -> Result<(), Error> {
        let mut p = self.pool.lock().unwrap();

        let s = &mut p.active_mut().ok_or("No active connection")?.session;
        if !s.control(SessionCommand::ListSources {
            request_id: 900_001,
        }) {
            return Err("Control queue is busy".into());
        }
        Ok(())
    }
    pub async fn switch_capture_source(
        &self,
        source: CaptureSource,
        w: u32,
        h: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<(), Error> {
        protocol::validate_video_settings(w, h, fps, bitrate)?;
        let mut p = self.pool.lock().unwrap();
        let e = p.active_mut().ok_or("No active connection")?;
        if !matches!(source, CaptureSource::MainDisplay)
            && !e.session.sources.iter().any(|s| s.source == source)
        {
            return Err("Selected source is no longer available".into());
        }
        e.session.request.width = w;
        e.session.request.height = h;
        e.session.request.fps = fps;
        e.session.request.bitrate_kbps = bitrate;
        if e.session.media.is_none() {
            // A files-only connection has no video subscription to switch yet.
            // Subscribe directly to the requested window; never flash the desktop.
            e.session.source = source;
            e.session.request.source = source;
            e.session.start_video();
        } else {
            e.session.switch_source(source);
        }
        *e.frame.lock().unwrap() = None;
        Ok(())
    }
    pub async fn update_stream_settings(
        &self,
        w: u32,
        h: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<(), Error> {
        let source = self.snapshot().active_source;
        self.switch_capture_source(source, w, h, fps, bitrate).await
    }
    pub fn queue_viewing_input(&self, event: InputEvent) -> bool {
        let mut p = self.pool.lock().unwrap();
        let Some(e) = p.active_mut() else {
            return false;
        };
        let s = &mut e.session;
        if !s.can_input() {
            return false;
        }
        match &event {
            InputEvent::MouseUp(id) if !s.mouse.contains(id) => return false,
            InputEvent::Key {
                key_code,
                pressed: false,
                ..
            } if !s.keys.contains(key_code) => return false,
            _ => {}
        }
        let command = s.input_command(event.clone());
        if !s.control(command) {
            s.release_input();
            return false;
        }
        match event {
            InputEvent::MouseDown(b) => {
                s.mouse.insert(b);
            }
            InputEvent::MouseUp(b) => {
                s.mouse.remove(&b);
            }
            InputEvent::Key {
                key_code, pressed, ..
            } => {
                if pressed {
                    s.keys.insert(key_code);
                } else {
                    s.keys.remove(&key_code);
                }
            }
            InputEvent::ModifiersChanged(m) => s.modifiers = m,
            _ => {}
        }
        true
    }
    pub fn set_input_locked(&self, locked: bool) {
        let mut p = self.pool.lock().unwrap();

        if let Some(e) = p.active_mut() {
            if locked {
                e.session.release_input();
            }
            e.session.locked = locked;
        }
    }
    pub fn release_input(&self) {
        let mut p = self.pool.lock().unwrap();

        if let Some(e) = p.active_mut() {
            e.session.release_input();
        }
    }
    pub fn set_media_paused(&self, paused: bool) {
        let mut p = self.pool.lock().unwrap();

        if let Some(e) = p.active_mut()
            && e.session.paused != paused
        {
            e.session.set_paused(paused);
        }
    }
    pub fn supports_clipboard_sync(&self) -> bool {
        let p = self.pool.lock().unwrap();
        p.active()
            .is_some_and(|e| e.session.conn.clipboard_available)
    }
    pub fn clipboard_sync_enabled(&self) -> bool {
        let p = self.pool.lock().unwrap();
        p.active().is_some_and(|e| e.session.clipboard_enabled)
    }
    pub fn set_clipboard_sync_enabled(&self, enabled: bool) -> bool {
        let mut p = self.pool.lock().unwrap();
        p.clipboard_preference = enabled;
        if let Some(e) = p.active_mut() {
            e.session.set_clipboard(enabled);
            return e.session.clipboard_enabled == enabled;
        }
        false
    }
    pub fn supports_file_transfer(&self) -> bool {
        let p = self.pool.lock().unwrap();
        p.active().is_some_and(|e| e.session.conn.files_available)
    }
    pub fn file_transfer_enabled(&self) -> bool {
        self.backend.file_transfer_enabled()
    }
    pub fn set_file_transfer_enabled(&self, enabled: bool) -> bool {
        self.backend.set_file_transfer_enabled(enabled)
    }
    pub fn supports_talkback(&self) -> bool {
        cfg!(target_os = "macos") && self.active_connection().is_some()
    }
    pub fn talkback_enabled(&self) -> bool {
        self.pool.lock().unwrap().talkback.is_some()
    }
    pub fn set_talkback_enabled(&self, enabled: bool) -> bool {
        let mut p = self.pool.lock().unwrap();
        if let Some((_, c)) = p.talkback.take() {
            c.stop();
        }
        if !enabled {
            return true;
        }
        if !cfg!(target_os = "macos") {
            return false;
        }
        let Some(e) = p.active() else {
            return false;
        };
        if !e.session.confirmed || e.session.paused {
            return false;
        }
        let c = client::start_talkback_runtime_control(e.session.conn.sender.clone());
        c.start(e.session.conn.target, e.session.video_id);
        p.talkback = Some((e.session.conn.id, c));
        true
    }
    pub fn set_audio(&self, volume: u8, muted: bool) {
        let mut p = self.pool.lock().unwrap();

        if let Some(e) = p.active_mut() {
            e.session.volume = volume.min(100);
            e.session.muted = muted;
            e.session.audio_settings();
        }
    }
    pub fn media_state(&self) -> (bool, bool) {
        let p = self.pool.lock().unwrap();
        p.active()
            .map(|e| {
                (
                    e.session.paused || e.session.background_paused,
                    e.session.media_activity_pending(),
                )
            })
            .unwrap_or((false, false))
    }
    pub fn audio(&self) -> (u8, bool) {
        let p = self.pool.lock().unwrap();
        p.active()
            .map(|e| (e.session.volume, e.session.muted))
            .unwrap_or((100, false))
    }
    pub fn file_transfer_snapshot(&self) -> Vec<TransferEntrySnapshot> {
        let p = self.pool.lock().unwrap();
        p.active()
            .map(|e| e.session.transfers.lock().unwrap().snapshots())
            .unwrap_or_default()
    }
    pub fn receive_dir(&self) -> PathBuf {
        self.pool.lock().unwrap().receive_dir.clone()
    }
    pub fn set_file_receive_dir(&self, path: PathBuf) {
        let mut p = self.pool.lock().unwrap();
        p.receive_dir = path.clone();
        for e in &mut p.sessions {
            e.session
                .files_command(FileTransferCommand::SetReceiveConfig {
                    receive_dir: path.clone(),
                    receive_policy: remote_core::file_transfer::FileReceivePolicy::default(),
                });
        }
    }
    pub fn file_snapshot(
        &self,
    ) -> (
        Vec<protocol::shared_files::SharedFileInfo>,
        Option<u64>,
        Option<u64>,
        bool,
    ) {
        let p = self.pool.lock().unwrap();
        p.active()
            .map(|e| {
                (
                    e.session.files.clone(),
                    e.session.file_history.last().copied(),
                    e.session.next_file_page,
                    e.session.file_pending,
                )
            })
            .unwrap_or_default()
    }
    pub(crate) fn file_view_binding(&self) -> Option<FileViewBinding> {
        self.active_connection().map(|connection| FileViewBinding {
            connection: Arc::downgrade(&connection),
            scope: remote_core::shared_files::current_share_scope(),
        })
    }
    pub(crate) fn list_bound_files(&self, binding: &Option<FileViewBinding>, after: u64) {
        self.with_bound_file_session(binding, |s| {
            s.release_input();
            s.list_files(after);
        });
    }
    pub(crate) fn fetch_bound_file(&self, binding: &Option<FileViewBinding>, id: u64) {
        self.with_bound_file_session(binding, |s| s.fetch_file(id));
    }
    pub(crate) fn cancel_bound_transfer(
        &self,
        binding: &Option<FileViewBinding>,
        target: client::TransferCancelTarget,
    ) {
        self.with_bound_file_session(binding, |s| {
            let command = match target {
                client::TransferCancelTarget::Transfer(transfer_id) => {
                    FileTransferCommand::CancelTransfer { transfer_id }
                }
                client::TransferCancelTarget::Group(group_id) => {
                    FileTransferCommand::CancelGroup { group_id }
                }
            };
            s.files_command(command);
        });
    }
    fn with_bound_file_session(
        &self,
        binding: &Option<FileViewBinding>,
        action: impl FnOnce(&mut OriginalGuiSession),
    ) {
        let Some(binding) = binding else {
            return;
        };
        let mut pool = self.pool.lock().unwrap();
        if let Some(entry) = pool.active_mut()
            && entry.session.connected
            && view_binding_matches(
                &binding.connection,
                &entry.session.conn,
                binding.scope,
                remote_core::shared_files::current_share_scope(),
            )
        {
            action(&mut entry.session);
        }
    }

    pub fn send_files_to(
        &self,
        target: &Weak<WorkspaceConnection>,
        scope: ShareScope,
        paths: Vec<PathBuf>,
    ) {
        if scope != remote_core::shared_files::current_share_scope() {
            return;
        }
        let mut p = self.pool.lock().unwrap();
        if let Some(e) = p
            .sessions
            .iter_mut()
            .find(|e| super::same_connection(target, &e.session.conn) && e.session.connected)
        {
            if paths.len() == 1 {
                e.session.files_command(FileTransferCommand::SendFile {
                    path: paths.into_iter().next().unwrap(),
                    mime_type: None,
                });
            } else if !paths.is_empty() {
                e.session.files_command(FileTransferCommand::SendFileGroup {
                    files: paths
                        .into_iter()
                        .map(
                            |path| remote_core::file_transfer_runtime::FileTransferGroupFile {
                                path,
                                mime_type: None,
                            },
                        )
                        .collect(),
                });
            }
        }
    }
    pub fn diagnostic_snapshot(&self) -> serde_json::Value {
        let p = self.pool.lock().unwrap();
        serde_json::json!({"pending":p.pending.len(),"sessions":p.sessions.iter().map(|e|{
            let s=&e.session;
            let native=serde_json::Value::Null;
            #[cfg(all(target_os="windows",feature="native-windows-video"))]
            let native=s.texture.as_ref().and_then(|frame|frame.native.as_ref().map(|n|serde_json::json!({
                "generation":n.generation,"coded":n.ready.frame.geometry().coded,"visible":n.ready.frame.geometry().visible,
                "cpu_pixel_bytes":frame.rgba.len(),"exact_frame_submitted":n.ready.frame.was_submitted(),
                "surface_counters":n.ready.frame.stats().snapshot(),"surface_error":n.ready.frame.stats().last_error.lock().unwrap().clone()
            }))).unwrap_or(native);
            #[cfg(all(target_os="linux",feature="native-linux-video"))]
            let native=s.texture.as_ref().and_then(|frame|frame.native_linux.as_ref().map(|n|serde_json::json!({
                "generation":n.generation,"coded":n.frame.geometry().coded,"visible":n.frame.geometry().visible,
                "cpu_pixel_bytes":frame.rgba.len(),"exact_frame_submitted":n.frame.was_submitted(),
                "surface_counters":n.frame.stats().snapshot(),"surface_error":n.frame.stats().last_error.lock().unwrap().clone()
            }))).unwrap_or(native);
            serde_json::json!({"connected":s.connected,"peer_responsive":s.conn.peer_is_responsive(),
                "decoder_status":s.media.as_ref().and_then(|m|m.decode_status.try_lock().ok().map(|v|v.clone())),
                "decoder_needs_keyframe":s.stats.video_decoder_needs_keyframe.load(std::sync::atomic::Ordering::Acquire),
                "native_frame":native,"device_id":s.key,"name":s.name,"connection_id":s.conn.id,"target":s.conn.target.to_string(),"route":s.route,
                "source":format!("{:?}",s.source),"decoded_frames":s.stats.video_frames_decoded.load(std::sync::atomic::Ordering::Relaxed),
                "decode_errors":s.stats.video_decode_errors.load(std::sync::atomic::Ordering::Relaxed),"confirmed":s.confirmed,
                "scene_submitted_frame":s.uploaded_at.is_some(),"input_locked":s.locked,"error":s.error,"video_error":s.video_error,
                "files_listed":s.files.len(),"transfers":s.transfers.lock().unwrap().snapshots().len()})
        }).collect::<Vec<_>>()})
    }
    #[cfg(all(test, any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    pub(crate) fn attach_recorder_session(&self, mut session: OriginalGuiSession) {
        // Test-only presentation precondition: real GUI input routing with a local
        // packet recorder. No native decoder image or OS injector is exercised.
        session.confirmed = true;
        session.supports_input = true;
        session.locked = false;
        session.first_frame_after = Instant::now() - std::time::Duration::from_secs(1);
        session.uploaded_at = Some(Instant::now());
        let mut p = self.pool.lock().unwrap();
        p.intent = Some(session.key.clone());
        p.selected = 0;
        p.sessions.push(ViewSession {
            source_seen: session.source,
            session,
            frame: Arc::new(Mutex::new(None)),
            frame_observer: None,
            sources: Arc::new(Vec::new()),
        });
    }

    pub fn close_all(&self) {
        let mut p = self.pool.lock().unwrap();
        p.pending.clear();
        p.connecting = None;
        p.intent = None;
        p.sessions.clear();
        if let Some((_, c)) = p.talkback.take() {
            c.stop();
        }
    }
}
impl Drop for OriginalOwner {
    fn drop(&mut self) {
        self.close_all();
    }
}

#[cfg(test)]
mod restoration_regression_tests {
    use super::*;
    #[test]
    fn stale_file_view_rejects_new_connection_and_network_even_with_same_labels() {
        let first = Arc::new("same device");
        let second = Arc::new("same device");
        let binding = Arc::downgrade(&first);
        let scope = Some([7; 32]);
        assert!(view_binding_matches(&binding, &first, scope, scope));
        assert!(!view_binding_matches(&binding, &second, scope, scope));
        assert!(!view_binding_matches(
            &binding,
            &first,
            scope,
            Some([8; 32])
        ));
        drop(first);
        assert!(!view_binding_matches(&binding, &second, scope, scope));
    }
    #[test]
    fn late_connection_cannot_cancel_newer_attempt_for_same_device() {
        let mut pending = BTreeMap::from([("macbook".to_owned(), 2), ("studio".to_owned(), 1)]);
        assert!(!retire_attempt(&mut pending, "macbook", 1));
        assert_eq!(pending.get("macbook"), Some(&2));
        assert!(retire_attempt(&mut pending, "macbook", 2));
        assert!(!retire_attempt(&mut pending, "macbook", 2));
        assert_eq!(pending.get("studio"), Some(&1));
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "original_owner_live_tests.rs"]
mod original_owner_live_tests;

// Surface errors must not disappear behind a generic connected/waiting state.
// Readiness still comes from the current frame gate, never from a status label.
fn active_video_error(host:Option<&str>,decoder:Option<&str>)->Option<String> {
    host.filter(|s|!s.trim().is_empty()).map(str::to_owned)
        .or_else(||decoder.filter(|s|!s.trim().is_empty()).map(|s|format!("Video decoder: {s}")))
}
#[cfg(test)] mod decoder_diagnostic_tests {
    use super::active_video_error;
    #[test] fn host_error_wins_without_hiding_decoder_only_failure() {
        assert_eq!(active_video_error(Some("source closed"),Some("codec failed")),Some("source closed".into()));
        assert_eq!(active_video_error(None,Some("unsupported source matrix")),Some("Video decoder: unsupported source matrix".into()));
    }
    #[test] fn recovered_decoder_does_not_leave_an_old_error() {
        assert!(active_video_error(None,Some("")).is_none());assert!(active_video_error(None,None).is_none());
        assert!(active_video_error(Some("  "),Some(" 
 ")).is_none());
    }
}
