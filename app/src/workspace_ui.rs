use client::TransferCenterState;
use client::audio_player::{AudioPlayer, AudioPlayerEvent};
use client::{ClientMediaRuntime, ClipboardRuntimeControl};
use gpui::*;
use protocol::session::{CaptureSourceInfo, SessionCommand, SubscriptionRequest};
use remote_core::workspace_session::{WorkspaceConnection, WorkspaceEvents};
use remote_core::{SharedHostStats, Statistics};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
static CLIPBOARD_TARGET: Mutex<Option<std::sync::Weak<WorkspaceConnection>>> = Mutex::new(None);

#[derive(Default)]
struct Shared {
    input_sources: std::collections::HashSet<u32>,
    clipboard_enabled: Arc<std::sync::atomic::AtomicBool>,
    connection: Option<Arc<WorkspaceConnection>>,
    sources: Vec<CaptureSourceInfo>,
    bindings: HashMap<u32, Option<u32>>,
    status: String,
    audio_error: Option<String>,
    transfers: TransferCenterState,
}
struct Pane {
    focus: FocusHandle,
    request: SubscriptionRequest,
    title: String,
    media: ClientMediaRuntime,
    _stats: Arc<Statistics>,
    bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    activity: Option<(bool, bool)>,
    revision: u64,
    resize_at: Instant,
    requested_size: (u32, u32),
}
struct WorkspaceView {
    shared: Arc<Mutex<Shared>>,
    panes: Vec<Pane>,
    next_id: u32,
    selected: Option<u32>,
    tiled: bool,
    keep_audio: bool,
    audio_tx: mpsc::Sender<AudioPlayerEvent>,
    _audio_player: Option<AudioPlayer>,
    volumes: HashMap<u32, u8>,
    clipboard: Option<ClipboardRuntimeControl>,
    receive_dir: std::path::PathBuf,
    last_visible: bool,
    rate_editor: Option<u32>,
    fps_edit: String,
    bitrate_edit: String,
}
impl Drop for WorkspaceView {
    fn drop(&mut self) {
        if let Some(control) = &self.clipboard {
            control.stop();
        }
        self.shared.lock().unwrap().connection.take();
    }
}

pub fn open(target: SocketAddr, name: String, cx: &mut App) {
    let result = cx.open_window(
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(format!("RemotePlay · {name}").into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds(
                point(px(100.), px(80.)),
                size(px(1200.), px(780.)),
            ))),
            window_min_size: Some(size(px(720.), px(480.))),
            ..Default::default()
        },
        move |window, cx| {
            let view = cx.new(|cx| WorkspaceView::new(target, window, cx));
            let weak = view.downgrade();
            cx.spawn(async move |cx| {
                loop {
                    let Ok(delay) = weak.update(&mut *cx, |view, cx| {
                        cx.notify();
                        if view.last_visible && !view.panes.is_empty() {
                            16
                        } else {
                            250
                        }
                    }) else {
                        break;
                    };
                    Timer::after(Duration::from_millis(delay)).await;
                }
            })
            .detach();
            view
        },
    );
    if let Err(error) = result {
        eprintln!("Open workspace: {error}");
    }
}

impl WorkspaceView {
    fn new(target: SocketAddr, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_window_activation(window, |this, window, _| {
            if !window.is_window_active() {
                this.release_input();
            }
        })
        .detach();
        let shared = Arc::new(Mutex::new(Shared {
            status: "Connecting…".into(),
            ..Default::default()
        }));
        let receive_dir = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Downloads/RemotePlay");
        let (audio_tx, audio_rx) = mpsc::channel(64);
        let audio_player = match AudioPlayer::new(audio_rx) {
            Ok(player) => Some(player),
            Err(error) => {
                shared.lock().unwrap().audio_error =
                    Some(format!("Audio output unavailable: {error}"));
                None
            }
        };
        let weak = Arc::downgrade(&shared);
        let directory = receive_dir.clone();
        let audio_events = audio_tx.clone();
        tokio::spawn(async move {
            match WorkspaceConnection::connect(target, directory).await {
                Ok((connection, events)) => {
                    let connection = Arc::new(connection);
                    if let Some(shared) = weak.upgrade() {
                        let mut state = shared.lock().unwrap();
                        state.connection = Some(connection.clone());
                        state.status = "Connected · choose windows or transfer files".into();
                    } else {
                        return;
                    }
                    let _ = connection
                        .control(SessionCommand::ListSources { request_id: 1 })
                        .await;
                    drop(connection);
                    let WorkspaceEvents {
                        mut control,
                        mut audio,
                        mut files,
                    } = events;
                    let (forward_tx, forward_rx) = mpsc::unbounded_channel();
                    let (commands, enabled) = {
                        let Some(shared) = weak.upgrade() else {
                            return;
                        };
                        let state = shared.lock().unwrap();
                        (
                            state.connection.as_ref().unwrap().file_commands.clone(),
                            state.clipboard_enabled.clone(),
                        )
                    };
                    tokio::spawn(async move {
                        let (cancel, cancel_rx) = tokio::sync::broadcast::channel(1);
                        let _cancel = cancel;
                        let _ = remote_core::clipboard_file_runtime::run_clipboard_file_sync(
                            remote_platform::MacClipboardProvider::new(),
                            commands,
                            files,
                            Some(forward_tx),
                            cancel_rx,
                            remote_core::clipboard_file_runtime::ClipboardFileSyncConfig {
                                enabled: Some(enabled),
                                ..Default::default()
                            },
                        )
                        .await;
                    });
                    files = forward_rx;
                    loop {
                        tokio::select! {
                            message = control.recv() => {
                                let Some(message) = message else { break; };
                                let Some(shared) = weak.upgrade() else { break; };
                                let mut state = shared.lock().unwrap();
                                match message {
                                    SessionCommand::Sources { sources, .. } => state.sources = sources,
                                    SessionCommand::Subscribed { id, audio_owner, supports_input } => { state.bindings.insert(id, audio_owner); if supports_input { state.input_sources.insert(id); } }
                                    SessionCommand::Error { reason, .. } => state.status = reason,
                                    SessionCommand::Closed { reason, .. } => state.status = format!("Disconnected: {reason}"),
                                    _ => {}
                                }
                            }
                            event = audio.recv() => {
                                let Some(event) = event else { break; };
                                let event = match event { remote_core::AudioIngressEvent::StreamConfig(config) => AudioPlayerEvent::StreamConfig(config), remote_core::AudioIngressEvent::Packet(packet) => AudioPlayerEvent::Packet(packet) };
                                let _ = audio_events.try_send(event);
                            }
                            event = files.recv() => {
                                let Some(event) = event else { break; };
                                if let Some(shared) = weak.upgrade() { shared.lock().unwrap().transfers.apply_event(&event); } else { break; }
                            }
                        }
                    }
                }
                Err(error) => {
                    if let Some(shared) = weak.upgrade() {
                        shared.lock().unwrap().status = error;
                    }
                }
            }
        });
        Self {
            shared,
            panes: Vec::new(),
            next_id: 256,
            selected: None,
            tiled: true,
            keep_audio: false,
            audio_tx,
            _audio_player: audio_player,
            volumes: HashMap::new(),
            clipboard: None,
            receive_dir,
            last_visible: true,
            rate_editor: None,
            fps_edit: String::new(),
            bitrate_edit: String::new(),
        }
    }
    fn connection(&self) -> Option<Arc<WorkspaceConnection>> {
        self.shared.lock().unwrap().connection.clone()
    }
    fn apply_rates(&mut self) {
        let Some(id) = self.rate_editor else {
            return;
        };
        let (Ok(fps), Ok(bitrate_kbps)) = (
            self.fps_edit.trim().parse::<u32>(),
            self.bitrate_edit.trim().parse::<u32>(),
        ) else {
            self.shared.lock().unwrap().status =
                "Enter positive integer FPS and bitrate (kbps)".into();
            return;
        };
        let Some(connection) = self.connection() else {
            return;
        };
        let Some(pane) = self.panes.iter_mut().find(|p| p.request.id == id) else {
            return;
        };
        if let Err(reason) = protocol::validate_video_settings(
            pane.request.width,
            pane.request.height,
            fps,
            bitrate_kbps,
        ) {
            self.shared.lock().unwrap().status = reason.into();
            return;
        }
        pane.request.fps = fps;
        pane.request.bitrate_kbps = bitrate_kbps;
        let request = pane.request.clone();
        tokio::spawn(async move {
            let _ = connection.update_settings(&request).await;
        });
        self.rate_editor = None;
    }
    fn input(&self, id: u32, event: protocol::InputEvent) {
        if !self.shared.lock().unwrap().input_sources.contains(&id) {
            return;
        }
        if let Some(connection) = self.connection() {
            connection.try_control(SessionCommand::Input { id, event });
        }
    }
    fn release_input(&self) {
        if let Some(connection) = self.connection() {
            for id in self.shared.lock().unwrap().input_sources.iter().copied() {
                let connection = connection.clone();
                tokio::spawn(async move {
                    let _ = connection
                        .control(SessionCommand::ReleaseInput { id })
                        .await;
                });
            }
        }
    }
    fn release_input_id(&self, id: u32) {
        if let Some(connection) = self.connection() {
            tokio::spawn(async move {
                let _ = connection
                    .control(SessionCommand::ReleaseInput { id })
                    .await;
            });
        }
    }
    fn pointer(&self, id: u32, position: Point<Pixels>) {
        let Some(pane) = self.panes.iter().find(|p| p.request.id == id) else {
            return;
        };
        let Some(rect) = *pane.bounds.lock().unwrap() else {
            return;
        };
        let frames = pane.media.shared_frame();
        let frame = frames.lock().unwrap();
        let Some(frame) = frame.as_ref() else {
            return;
        };
        use remote_core::VideoFrame;
        if let Some(event) = crate::ui::absolute_pointer_event(
            (position.x.into(), position.y.into()),
            (
                rect.origin.x.into(),
                rect.origin.y.into(),
                rect.size.width.into(),
                rect.size.height.into(),
            ),
            (frame.width(), frame.height()),
            crate::ui::ViewportScaleMode::AspectFit,
        ) {
            self.input(id, event);
        }
    }
    fn add(&mut self, source: CaptureSourceInfo, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.len() >= protocol::session::MAX_SUBSCRIPTIONS {
            return;
        }
        let Some(connection) = self.connection() else {
            return;
        };
        if self.panes.len() >= connection.max_subscriptions {
            self.shared.lock().unwrap().status = format!(
                "This device supports {} concurrent source(s)",
                connection.max_subscriptions
            );
            return;
        }
        let stats = Statistics::new();
        let media = match ClientMediaRuntime::start_video_only(stats.clone()) {
            Ok(media) => media,
            Err(error) => {
                self.shared.lock().unwrap().status = error.to_string();
                return;
            }
        };
        let preferences = crate::preferences::UserPreferences::load_or_default();
        let request = SubscriptionRequest {
            id: self.next_id,
            source: source.source,
            width: 1920,
            height: 1080,
            fps: preferences.stream.fps,
            bitrate_kbps: preferences.stream.bitrate_kbps,
            audio: true,
        };
        self.next_id += 256;
        self.selected = Some(request.id);
        let send_request = request.clone();
        let decode = media.decode_tx.clone();
        let decode_stats = stats.clone();
        tokio::spawn(async move {
            let _ = connection
                .subscribe(
                    send_request,
                    decode,
                    decode_stats,
                    Arc::new(SharedHostStats::default()),
                )
                .await;
        });
        let focus = cx.focus_handle();
        let id = request.id;
        cx.on_focus_out(&focus, window, move |this, _, _, _| {
            this.release_input_id(id)
        })
        .detach();
        self.panes.push(Pane {
            focus,
            request,
            title: format!("{} · {}", source.application, source.title),
            media,
            _stats: stats,
            bounds: Arc::new(Mutex::new(None)),
            activity: None,
            revision: 0,
            resize_at: Instant::now(),
            requested_size: (1920, 1080),
        });
    }
    fn remove(&mut self, id: u32) {
        self.release_input();
        self.panes.retain(|pane| pane.request.id != id);
        if self.selected == Some(id) {
            self.selected = self.panes.first().map(|p| p.request.id);
        }
        if let Some(connection) = self.connection() {
            tokio::spawn(async move {
                let _ = connection.control(SessionCommand::Unsubscribe { id }).await;
            });
        }
        let mut state = self.shared.lock().unwrap();
        if let Some(Some(owner)) = state.bindings.remove(&id)
            && !state.bindings.values().any(|value| *value == Some(owner))
        {
            let _ = self.audio_tx.try_send(AudioPlayerEvent::RemoveStream(
                protocol::remote_system_audio_stream_id(owner),
            ));
        }
    }
    fn volume(&mut self, id: u32, change: i16) {
        let owner = self
            .shared
            .lock()
            .unwrap()
            .bindings
            .get(&id)
            .copied()
            .flatten();
        if let Some(owner) = owner {
            let volume = self.volumes.entry(owner).or_insert(100);
            *volume = (i16::from(*volume) + change).clamp(0, 200) as u8;
            let _ = self.audio_tx.try_send(AudioPlayerEvent::StreamVolume {
                stream_id: protocol::remote_system_audio_stream_id(owner),
                volume_percent: *volume,
                muted: *volume == 0,
            });
        }
    }
    fn toggle_clipboard(&mut self) {
        let Some(connection) = self.connection() else {
            return;
        };
        if !connection.clipboard_available {
            self.shared.lock().unwrap().status =
                "This device does not support clipboard sharing".into();
            return;
        }
        if let Some(control) = self.clipboard.take() {
            self.shared
                .lock()
                .unwrap()
                .clipboard_enabled
                .store(false, std::sync::atomic::Ordering::Relaxed);
            control.stop();
            tokio::spawn(async move {
                let _ = connection.clipboard(None).await;
                let _ = connection
                    .control(SessionCommand::SetClipboard { enabled: false })
                    .await;
            });
        } else {
            if let Some(previous) = CLIPBOARD_TARGET
                .lock()
                .unwrap()
                .replace(Arc::downgrade(&connection))
                .and_then(|weak| weak.upgrade())
            {
                tokio::spawn(async move {
                    let _ = previous
                        .control(SessionCommand::SetClipboard { enabled: false })
                        .await;
                    let _ = previous.clipboard(None).await;
                });
            }
            self.shared
                .lock()
                .unwrap()
                .clipboard_enabled
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let control = client::start_clipboard_runtime_control(connection.sender.clone());
            control.start_workspace(
                connection.target,
                self.shared.lock().unwrap().clipboard_enabled.clone(),
            );
            self.clipboard = Some(control.clone());
            tokio::spawn(async move {
                let _ = connection.clipboard(Some(Arc::new(control))).await;
                let _ = connection
                    .control(SessionCommand::SetClipboard { enabled: true })
                    .await;
            });
        }
    }

    fn update_activity(&mut self, visible: bool, scale: f32) {
        let Some(connection) = self.connection() else {
            return;
        };
        self.last_visible = visible;
        let visible_count = if self.tiled {
            self.panes.len().max(1)
        } else {
            1
        };
        for pane in &mut self.panes {
            let video = visible && (self.tiled || self.selected == Some(pane.request.id));
            let audio = video || self.keep_audio;
            if pane.activity != Some((video, audio)) {
                pane.activity = Some((video, audio));
                pane.revision += 1;
                let command = SessionCommand::SetActivity {
                    id: pane.request.id,
                    revision: pane.revision,
                    video,
                    audio,
                };
                let connection = connection.clone();
                tokio::spawn(async move {
                    let _ = connection.control(command).await;
                });
            }
            if video && let Some(bounds) = *pane.bounds.lock().unwrap() {
                let width = (f32::from(bounds.size.width) * scale).clamp(64., 3840.) as u32 & !1;
                let height = (f32::from(bounds.size.height) * scale).clamp(64., 2160.) as u32 & !1;
                let size = (width / 32 * 32, height / 32 * 32);
                if pane.requested_size != size {
                    pane.requested_size = size;
                    pane.resize_at = Instant::now();
                }
                if (pane.request.width, pane.request.height) != size
                    && pane.resize_at.elapsed() > Duration::from_millis(250)
                {
                    pane.request.width = size.0;
                    pane.request.height = size.1;
                    let ceiling = 120_000_000_u64
                        / (u64::from(size.0) * u64::from(size.1) * visible_count as u64).max(1);
                    let mut request = pane.request.clone();
                    request.fps = request.fps.min(ceiling.max(1) as u32);
                    let connection = connection.clone();
                    tokio::spawn(async move {
                        let _ = connection.update_settings(&request).await;
                    });
                }
            }
        }
    }
}

fn action(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id.into())
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(0x25313a))
        .cursor_pointer()
        .child(label.into())
}
impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self
            .shared
            .lock()
            .unwrap()
            .clipboard_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
            && let Some(control) = self.clipboard.take()
        {
            control.stop();
        }
        self.update_activity(window_visible(window), window.scale_factor());
        let state = self.shared.lock().unwrap();
        let sources = state.sources.clone();
        let status = state.status.clone();
        let audio_error = state.audio_error.clone();
        let transfers = state.transfers.snapshots();
        let bindings = state.bindings.clone();
        drop(state);
        let toolbar = div().flex().gap_2().items_center().child(action("grid", if self.tiled { "Grid ✓" } else { "Grid" }).on_click(cx.listener(|this, _, _, cx| { this.tiled = true; cx.notify(); })))
            .child(action("clipboard", if self.clipboard.is_some() { "Clipboard ✓" } else { "Clipboard" })
                .on_click(cx.listener(|this, _, _, cx| { this.toggle_clipboard(); cx.notify(); })))
            .child(action("tabs", if !self.tiled { "Tabs ✓" } else { "Tabs" }).on_click(cx.listener(|this, _, _, cx| { this.tiled = false; cx.notify(); })))
            .child(action("audio-background", if self.keep_audio { "Background audio ✓" } else { "Background audio" }).on_click(cx.listener(|this, _, _, cx| { this.keep_audio = !this.keep_audio; cx.notify(); })))
            .child(action("send", "Send files").on_click(cx.listener(|this, _, _, _| {
                if let Some(connection) = this.connection() { std::thread::spawn(move || {
                    if let Some(paths) = rfd::FileDialog::new().pick_files() { let _ = connection.file_commands.blocking_send(remote_core::file_transfer_runtime::FileTransferCommand::SendFileGroup { files: paths.into_iter().map(|path| remote_core::file_transfer_runtime::FileTransferGroupFile { path, mime_type: None }).collect() }); }
                }); }
            })))
            .child(action("received", "Received files").on_click(cx.listener(|this, _, _, cx| cx.reveal_path(&this.receive_dir))))
            .child(action("refresh", "Refresh sources").on_click(cx.listener(|this, _, _, _| { if let Some(connection) = this.connection() { tokio::spawn(async move { let _ = connection.control(SessionCommand::ListSources { request_id: 1 }).await; }); } })));
        let mut sidebar = div()
            .id("sources")
            .w(px(240.))
            .flex_none()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .child("Displays and application windows");
        for (index, source) in sources.into_iter().enumerate() {
            sidebar = sidebar.child(
                action(
                    format!("source-{index}"),
                    format!("{} — {}", source.application, source.title),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.add(source.clone(), window, cx);
                    cx.notify();
                })),
            );
        }
        let mut tabs = div().flex().flex_wrap().gap_2();
        for pane in &self.panes {
            let id = pane.request.id;
            tabs = tabs.child(action(format!("tab-{id}"), pane.title.clone()).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.selected = Some(id);
                    cx.notify();
                }),
            ));
        }
        let count = if self.tiled {
            self.panes.len()
        } else {
            usize::from(!self.panes.is_empty())
        };
        let columns = if count > 1 { 2 } else { 1 };
        let mut grid = div()
            .flex_1()
            .min_h_0()
            .grid()
            .grid_cols(columns)
            .grid_rows((count as u16).div_ceil(columns).max(1))
            .gap_2();
        for pane in &self.panes {
            let id = pane.request.id;
            if !self.tiled && self.selected != Some(id) {
                continue;
            }
            let volume = bindings
                .get(&id)
                .copied()
                .flatten()
                .and_then(|owner| self.volumes.get(&owner).copied())
                .unwrap_or(100);
            let heading = div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().flex_1().overflow_hidden().child(pane.title.clone()))
                .child(action(format!("rates-{id}"), "Rates").on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(pane) = this.panes.iter().find(|p| p.request.id == id) {
                            this.rate_editor = Some(id);
                            this.fps_edit = pane.request.fps.to_string();
                            this.bitrate_edit = pane.request.bitrate_kbps.to_string();
                            cx.notify();
                        }
                    },
                )))
                .child(action(format!("less-{id}"), "−").on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.volume(id, -10);
                        cx.notify();
                    },
                )))
                .child(format!("{volume}%"))
                .child(action(format!("more-{id}"), "+").on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.volume(id, 10);
                        cx.notify();
                    },
                )))
                .child(action(format!("close-{id}"), "×").on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.remove(id);
                        cx.notify();
                    },
                )));
            let shared_frame = pane.media.shared_frame();
            let frame = shared_frame.lock().unwrap();
            let image = if let Some(frame) = frame.as_ref() {
                client::decoded_video_frame_surface_with_fit(frame, ObjectFit::Contain)
            } else {
                div().child("Waiting for source…").into_any_element()
            };
            let bounds = pane.bounds.clone();
            grid = grid.child(
                div()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .bg(rgb(0x101820))
                    .border_1()
                    .border_color(rgb(0x2a3844))
                    .rounded_md()
                    .p_2()
                    .child(heading)
                    .child(
                        div()
                            .id(format!("workspace-input-{id}"))
                            .track_focus(&pane.focus)
                            .on_mouse_move(cx.listener(
                                move |this, event: &MouseMoveEvent, _, _| {
                                    this.pointer(id, event.position)
                                },
                            ))
                            .on_any_mouse_down(cx.listener(
                                move |this, event: &MouseDownEvent, window, _| {
                                    if let Some(pane) =
                                        this.panes.iter().find(|p| p.request.id == id)
                                    {
                                        window.focus(&pane.focus);
                                    }
                                    this.pointer(id, event.position);
                                    if let Some(button) =
                                        crate::ui::protocol_mouse_button(event.button)
                                    {
                                        this.input(id, protocol::InputEvent::MouseDown(button));
                                    }
                                },
                            ))
                            .capture_any_mouse_up(cx.listener(
                                move |this, event: &MouseUpEvent, _, _| {
                                    if let Some(button) =
                                        crate::ui::protocol_mouse_button(event.button)
                                    {
                                        this.input(id, protocol::InputEvent::MouseUp(button));
                                    }
                                },
                            ))
                            .capture_key_down(cx.listener(
                                move |this, event: &KeyDownEvent, _, _| {
                                    if let Some(event) =
                                        crate::ui::protocol_key_event(&event.keystroke, true)
                                    {
                                        this.input(id, event);
                                    }
                                },
                            ))
                            .capture_key_up(cx.listener(move |this, event: &KeyUpEvent, _, _| {
                                if let Some(event) =
                                    crate::ui::protocol_key_event(&event.keystroke, false)
                                {
                                    this.input(id, event);
                                }
                            }))
                            .on_scroll_wheel(cx.listener(
                                move |this, event: &ScrollWheelEvent, _, _| {
                                    let delta = event.delta.pixel_delta(px(40.));
                                    this.input(
                                        id,
                                        protocol::InputEvent::MouseScroll {
                                            delta_x: f32::from(delta.x).round() as i32,
                                            delta_y: f32::from(delta.y).round() as i32,
                                        },
                                    );
                                },
                            ))
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .child(image)
                            .child(
                                canvas(
                                    move |rect, _, _| {
                                        *bounds.lock().unwrap() = Some(rect);
                                    },
                                    |_, (), _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            ),
                    ),
            );
        }
        let mut transfer_line = div().flex().flex_wrap().gap_2();
        for transfer in transfers.iter().rev().take(4) {
            transfer_line = transfer_line.child(format!(
                "{} · {:?} · {:.0}%",
                transfer.label,
                transfer.status,
                transfer.progress() * 100.
            ));
        }
        let mut rates = div().flex().gap_2();
        if self.rate_editor.is_some() {
            rates = rates
                .child("FPS / kbps")
                .child(
                    yororen_ui::component::text_input("pane-fps")
                        .content(self.fps_edit.clone())
                        .on_change({
                            let view = cx.weak_entity();
                            move |text, _, cx| {
                                let _ = view.update(cx, |this, _| this.fps_edit = text.to_string());
                            }
                        }),
                )
                .child(
                    yororen_ui::component::text_input("pane-bitrate")
                        .content(self.bitrate_edit.clone())
                        .on_change({
                            let view = cx.weak_entity();
                            move |text, _, cx| {
                                let _ =
                                    view.update(cx, |this, _| this.bitrate_edit = text.to_string());
                            }
                        }),
                )
                .child(action("apply-pane-rates", "Apply").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.apply_rates();
                        cx.notify();
                    },
                )));
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(rgb(0x0a1017))
            .text_color(rgb(0xe0e9ef))
            .text_size(px(12.))
            .child(toolbar)
            .child(status)
            .children(audio_error)
            .child(rates)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(sidebar)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(tabs)
                            .child(grid),
                    ),
            )
            .child(transfer_line)
    }
}

/// Keyboard focus is not visibility: unfocused tiled windows keep streaming.
pub(crate) fn window_visible(window: &Window) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return true;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return true;
    };
    unsafe {
        let view = handle.ns_view.as_ptr() as *mut objc2::runtime::AnyObject;
        let native: *mut objc2::runtime::AnyObject = objc2::msg_send![view, window];
        if native.is_null() {
            return true;
        }
        let minimized: bool = objc2::msg_send![native, isMiniaturized];
        let visible: bool = objc2::msg_send![native, isVisible];
        !minimized && visible
    }
}
