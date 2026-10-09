#[path="restored_acceptance.rs"]
mod sequence_acceptance;
#[cfg(target_os = "macos")]
use client::decoded_video_frame_surface_with_fit;
// Original product visual tree rebound to the current authenticated workspace sessions.
// This preview is feature gated until preservation and platform performance acceptance.
use crate::design_system::{
    color_accent_amber, color_accent_cyan, color_accent_emerald, color_accent_purple,
    color_border_fine, color_glass_card, remote_play_themes,
};
use crate::desktop::instance::Instance;
use crate::desktop::device_list::{
    DeviceListAction, DeviceListEffect, DeviceListState,
};
use crate::desktop::original_owner::{FrameSlot, OriginalOwner, SourceViewBinding};
use crate::original_design::{full_idle_canvas_stage, stream_status_capsule_card};
use crate::{
    AppDevice, HostStats, MacDecodedVideoFrame, MeshPairingControl, MeshPairingMessageKind,
    MeshPairingSnapshot, RoleState, SharedHostStats, StreamStartOptions, UnifiedRuntimeConfig,
    UnifiedRuntimeHandle, UnifiedViewerMediaStatus, start_unified_runtime,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use remote_core::{
    VideoFrame,
    net::DEFAULT_CONTROL_PORT,
    pairing_qr::{encode_pairing_qr, qr_matrix_from_payload},
    role::RoleKind,
    session_tabs::{SessionCommandState, SessionTabsAction, SessionTabsEffect},
    stream_settings::{StreamSettingsAction, StreamSettingsState, StreamSettingsValues},
};
use std::collections::BTreeSet;
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use crate::product_components::{
    assets::UiAsset,
    component::{self, Button, IconName, button, icon, tooltip},
    theme::{ActionVariantKind, ActiveTheme, Theme},
};

pub async fn run_restored_gui(
    mut config: UnifiedRuntimeConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    start_owned_gui_trace();
    let Some(mut instance) = Instance::acquire_for_renderer(&config.mesh_dir,crate::gui_backend::ORIGINAL_RENDERER)? else {
        return Ok(());
    };
    let activation = Arc::new(AtomicBool::new(false));
    let signal = activation.clone();
    instance.attach_callback(move || {
        signal.store(true, Ordering::Release);
    })?;
    config.enable_client_receiver = false;
    config.enable_viewer_media = false;
    config.enable_workspace_viewer = true;
    #[cfg(target_os = "windows")]
    if std::env::var("REMOTE_PLAY_ENABLE_HOST").as_deref() != Ok("1") {
        config.enable_passive_host = false;
    }
    if std::env::var("REMOTE_PLAY_ENABLE_HOST").as_deref() == Ok("0") {
        config.enable_passive_host = false;
    }
    let runtime = Arc::new(start_unified_runtime(config).await?);
    let startup_error = Arc::new(Mutex::new(None));
    let init_error = startup_error.clone();
    crate::desktop::foreground_runtime::run(move || {
        Application::new()
            .with_assets(UiAsset)
            .run(move |cx: &mut App| {
                apply_product_window_appearance();
                if let Err(error) = component::init(cx) {
                    *init_error.lock().expect("GUI startup error lock") = Some(error.to_string());
                    cx.quit();
                    return;
                }
                crate::product_components::theme::install(
                    product_window_appearance(),
                    remote_play_themes(), cx);
                open_restored_window(runtime, Some(instance), activation, None, cx);
            })
    })?;
    if let Some(error) = startup_error.lock().map_err(|_| "GUI startup error lock poisoned")?.take() {
        return Err(format!("Ely initialization failed: {error}").into());
    }
    Ok(())
}
fn open_restored_window(
    runtime: Arc<UnifiedRuntimeHandle>,
    instance: Option<Instance>,
    activation: Arc<AtomicBool>,
    device: Option<String>,
    cx: &mut App,
) {
    let primary = instance.is_some();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds(
            point(px(80.), px(60.)),
            size(px(1280.), px(800.)),
        ))),
        window_min_size: Some(size(px(800.), px(560.))),
        titlebar: Some(TitlebarOptions {
            title: Some(
                if std::env::var_os("REMOTE_PLAY_RESTORED_TEST_OUTPUT").is_some() {
                    "RemotePlay restored GUI review"
                } else {
                    "RemotePlay"
                }
                .into(),
            ),
            ..Default::default()
        }),
        ..Default::default()
    };
    cx.open_window(options, move |window, cx| {
        let view = cx.new(|cx| RestoredDashboard::new(runtime, instance, activation, window, cx));
        let weak = view.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            if primary {
                let _ = weak.update(cx, |this, _| {
                    this.owner.release_input();
                    this.window_active = false;
                });
                window.minimize_window();
                false
            } else {
                true
            }
        });
        if let Some(device) = device {
            view.update(cx, |this, cx| {
                let ticket = this.session_commands.begin();
                let owner = this.owner.clone();
                cx.spawn(async move |view, cx| {
                    if !view.update(cx, |this, _| this.session_commands.is_current(ticket)).unwrap_or(false) { return; }
                    let result = owner
                        .connect_device(&device, StreamStartOptions::default(), 0)
                        .await;
                    let _ = view.update(cx, |this, cx| {
                        if !this.session_commands.is_current(ticket) { return; }
                        if let Err(e) = result {
                            this.status = e.to_string();
                        }
                        cx.notify();
                    });
                })
                .detach();
            });
        }
        let weak = view.downgrade();
        let mut frame_updates = view.read(cx).owner.subscribe_updates();
        cx.spawn(async move |cx| {
            loop {
                let Ok(interval) = weak.update(cx, |view, cx| {
                    if view.activation.swap(false, Ordering::AcqRel) {
                        let handle = view.window_handle;
                        let _ = handle.update(cx, |_, window, _| window.activate_window());
                    }
                    owned_gui_stage(1);
                    view.owner.poll();
                    owned_gui_stage(2);
                    if view.acceptance_tick(cx) {
                        return Duration::from_secs(1);
                    }
                    owned_gui_stage(3);
                    view.sync_media_pause();
                    view.check_toolbar_auto_hide(cx);
                    cx.notify();
                    view.refresh_interval()
                }) else {
                    break;
                };
                owned_gui_stage(4);
                tokio::select! {
                    changed=frame_updates.changed()=>{if changed.is_err(){break;}},
                    _=Timer::after(interval)=>{}
                }
            }
        })
        .detach();
        view
    })
    .expect("failed to open restored RemotePlay window");
}

fn product_window_appearance() -> WindowAppearance {
    // The video canvas and translucent control surfaces are intentionally dark.
    WindowAppearance::Dark
}

fn apply_product_window_appearance() {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSAppearance, NSAppearanceNameDarkAqua, NSApplication};

        let mtm = MainThreadMarker::new().expect("GPUI application must run on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        if let Some(appearance) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua })
        {
            app.setAppearance(Some(&appearance));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawerTab {
    Devices,
    DeviceGroup,
    Security,
    Network,
    Files,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportScaleMode {
    AspectFit,
    Fill,
}

#[derive(Debug, Clone, Copy)]
struct FeatureToggleState {
    available: bool,
    enabled: bool,
}

#[derive(Debug, Clone, Copy)]
struct SideServiceUiState {
    talkback: FeatureToggleState,
    clipboard_sync: FeatureToggleState,
    file_transfer: FeatureToggleState,
}

pub const TOOLBAR_TRIGGER_ZONE_HEIGHT_PX: f32 = 16.0;
pub const TOOLBAR_AUTO_HIDE_DELAY: Duration = Duration::from_millis(1800);

pub fn should_reveal_toolbar(cursor_y: f32) -> bool {
    (0.0..=TOOLBAR_TRIGGER_ZONE_HEIGHT_PX).contains(&cursor_y)
}

pub fn should_auto_hide_toolbar(
    toolbar_hovered: bool,
    menu_open: bool,
    last_activity: Instant,
    now: Instant,
    hide_delay: Duration,
) -> bool {
    !toolbar_hovered && !menu_open && now.saturating_duration_since(last_activity) >= hide_delay
}

struct RestoredDashboard {
    runtime: Arc<UnifiedRuntimeHandle>,
    owner: Arc<OriginalOwner>,
    _instance: Option<Instance>,
    activation: Arc<AtomicBool>,
    bound_source: Option<(u32, Instant)>,
    quitting: bool,
    window_handle: AnyWindowHandle,
    started: Instant,
    render_calls: u64,
    test_connect_started: bool,
    test_source_started: bool,
    test_sequence:Option<Result<sequence_acceptance::Sequence,String>>,
    current_frame: Option<Arc<MacDecodedVideoFrame>>,
    presentation_frame: FrameSlot,
    host_stats: Option<Arc<SharedHostStats>>,
    viewer_media_status: UnifiedViewerMediaStatus,
    mesh_pairing: Option<MeshPairingControl>,
    mesh_pairing_snapshot: Option<MeshPairingSnapshot>,
    pointer_input: PointerInputTracker,
    input_focus: FocusHandle,
    root_focus: FocusHandle,
    video_surface_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    input_session_id: Option<u32>,
    status: String,

    drawer_open: bool,
    active_tab: DrawerTab,
    device_list_state: DeviceListState,
    session_commands: SessionCommandState,
    source_commands: SessionCommandState,
    discovery_commands: SessionCommandState,
    scale_mode: ViewportScaleMode,
    telemetry_hud_collapsed: bool,
    input_locked: bool,
    capture_supports_input: bool,
    stream_settings: StreamSettingsState,
    manual_media_paused: bool,
    pause_when_inactive: bool,
    window_active: bool,
    telemetry_engine: remote_core::PipelineTelemetryEngine,

    show_apps_menu: bool,
    toolbar_revealed: bool,
    toolbar_hovered: bool,
    last_toolbar_activity: Instant,
    capture_error_seen: Option<String>,
}

impl RestoredDashboard {
    fn new(
        runtime: Arc<UnifiedRuntimeHandle>,
        instance: Option<Instance>,
        activation: Arc<AtomicBool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            if !this.window_active {
                this.owner.release_input();
                this.pointer_input = PointerInputTracker::default();
            }
            this.sync_media_pause();
            cx.notify();
        })
        .detach();
        let owner = OriginalOwner::new(runtime.owner.clone());
        let mesh_pairing = runtime.mesh_pairing.clone();
        let mesh_pairing_snapshot = mesh_pairing.as_ref().map(MeshPairingControl::snapshot);
        let prefs = crate::preferences::UserPreferences::load_or_default();
        let viewer_media_status = runtime.viewer_media_status.clone();
        let scale_mode = if prefs.ui.scale_mode == "fill" {
            ViewportScaleMode::Fill
        } else {
            ViewportScaleMode::AspectFit
        };
        let root_focus = cx.focus_handle();
        window.focus(&root_focus);
        Self {
            runtime,
            owner,
            _instance: instance,
            activation,
            bound_source: None,
            quitting: false,
            window_handle: window.window_handle(),
            started: Instant::now(),
            render_calls: 0,
            test_connect_started: false,
            test_source_started: false,
            test_sequence:sequence_acceptance::Sequence::from_env(),
            current_frame: None,
            presentation_frame: Arc::new(Mutex::new(None)),
            host_stats: None,
            viewer_media_status,
            mesh_pairing,
            mesh_pairing_snapshot,
            pointer_input: PointerInputTracker::default(),
            input_focus: cx.focus_handle(),
            root_focus,
            video_surface_bounds: Arc::new(Mutex::new(None)),
            input_session_id: None,
            status: "Ready".into(),
            drawer_open: true,
            active_tab: DrawerTab::Devices,
            device_list_state: DeviceListState::default(),
            session_commands: SessionCommandState::default(),
            source_commands: SessionCommandState::default(),
            discovery_commands: SessionCommandState::default(),
            scale_mode,
            telemetry_hud_collapsed: prefs.ui.telemetry_hud_collapsed,
            input_locked: true,
            capture_supports_input: false,
            stream_settings: StreamSettingsState::new(StreamSettingsValues {
                width: prefs.stream.width, height: prefs.stream.height,
                fps: prefs.stream.fps, bitrate_kbps: prefs.stream.bitrate_kbps,
            }),
            manual_media_paused: false,
            pause_when_inactive: prefs.ui.pause_when_inactive,
            window_active: window.is_window_active(),
            telemetry_engine: remote_core::PipelineTelemetryEngine::new(0, 300),
            show_apps_menu: false,
            toolbar_revealed: true,
            toolbar_hovered: false,
            last_toolbar_activity: Instant::now(),
            capture_error_seen: None,
        }
    }

    fn check_toolbar_auto_hide(&mut self, cx: &mut Context<Self>) {
        if self.toolbar_revealed
            && should_auto_hide_toolbar(
                self.toolbar_hovered,
                self.show_apps_menu,
                self.last_toolbar_activity,
                Instant::now(),
                TOOLBAR_AUTO_HIDE_DELAY,
            )
        {
            self.toolbar_revealed = false;
            self.show_apps_menu = false;
            cx.notify();
        }
    }

    fn persist_preferences(&self) {
        if let Err(err) = crate::preferences::UserPreferences::update(|prefs| {
            prefs.stream.width = self.stream_settings.values().resolution().0;
            prefs.stream.height = self.stream_settings.values().resolution().1;
            prefs.stream.fps = self.stream_settings.values().fps;
            prefs.stream.bitrate_kbps = self.stream_settings.values().bitrate_kbps;
            prefs.ui.scale_mode = match self.scale_mode {
                ViewportScaleMode::AspectFit => "aspect_fit".to_string(),
                ViewportScaleMode::Fill => "fill".to_string(),
            };
            prefs.ui.telemetry_hud_collapsed = self.telemetry_hud_collapsed;
            prefs.ui.pause_when_inactive = self.pause_when_inactive;
        }) {
            eprintln!("Failed to persist user preferences: {err}");
        }
    }

    fn snapshot(&self) -> DashboardSnapshot {
        let snapshot = self.owner.snapshot();
        DashboardSnapshot {
            role: snapshot.role,
            devices: snapshot.devices,
            capture_sources: snapshot.sources,
            source_binding: snapshot.source_binding,
            active_capture_source: snapshot.active_source,
            pending_capture_source: snapshot.pending_source,
            active_capture_supports_input: snapshot.supports_input,
            capture_source_error: snapshot.error,
        }
    }

    fn drain_latest_frame(&mut self) {
        self.owner.poll();
        let state = self.owner.snapshot();
        let identity = state.connection_id.zip(state.source_epoch);
        if identity != self.bound_source {
            self.owner.release_input();
            self.pointer_input = PointerInputTracker::default();
            self.current_frame = None;
            self.bound_source = identity;
            self.input_locked = true;
            self.presentation_frame = identity
                .and_then(|(expected, _)| {
                    self.owner.frame_binding().filter(|(id, _)| *id == expected)
                })
                .map(|(_, slot)| slot)
                .unwrap_or_else(|| Arc::new(Mutex::new(None)));
        }
        self.input_locked = state.locked;
        self.status = if let Some(error) = state.error {
            error
        } else if matches!(state.role, RoleState::Viewing(_)) {
            state.input_status
        } else {
            state.message
        };
        self.host_stats = self.owner.stats();
        let previous = self.current_frame.as_ref().map(|f| f.decoded_at);
        self.current_frame = self
            .presentation_frame
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(frame) = &self.current_frame
            && previous != Some(frame.decoded_at)
        {
            let mut timing = frame.timing;
            timing.render_submit_ts_us = 0;
            timing.render_done_ts_us = 0;
            self.telemetry_engine.record_frame(&timing, 0);
            if let Some(stats) = &self.host_stats {
                stats.set_decode_latency(frame.decode_cost_ms);
                stats.set_latest_timing(timing);
                stats.set_pipeline_report(self.telemetry_engine.generate_report());
            }
        }
        // Submission, GPU completion, and actual display are distinct. Do not manufacture
        // render_done from decoder timestamps; the paint hook below records eligibility only.
    }

    fn host_stats_snapshot(&self, role: &RoleState) -> Option<HostStats> {
        if !matches!(role, RoleState::Viewing(_)) {
            return None;
        }
        self.host_stats
            .as_ref()
            .map(|stats| stats.snapshot())
            .filter(host_stats_available)
    }

    fn reset_host_stats(&self) {
        if let Some(stats) = &self.host_stats {
            stats.reset();
        }
    }

    fn refresh_interval(&self) -> Duration {
        if self.manual_media_paused || !self.window_active {
            return Duration::from_millis(150);
        }
        Duration::from_millis(100)
    }

    fn copy_mesh_invite_code(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = &mut self.mesh_pairing_snapshot else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(snapshot.invite_code.clone()));
        snapshot.message = "Device-group code copied to clipboard.".to_string();
        snapshot.message_kind = MeshPairingMessageKind::Success;
        snapshot.restart_required = false;
    }

    fn create_mesh_group(&mut self) {
        if !self.owner.can_change_network() {
            self.status = "Disconnect current sessions before changing the device network.".into();
            return;
        }
        self.invalidate_source_commands();
        if let Some(control) = &self.mesh_pairing {
            self.session_commands.begin();
            self.owner.clear_failed_connection();
            self.mesh_pairing_snapshot = Some(control.create_new_group());
        }
    }

    fn join_mesh_group_from_clipboard(&mut self, cx: &mut Context<Self>) {
        if !self.owner.can_change_network() {
            self.status = "Disconnect current sessions before changing the device network.".into();
            return;
        }
        self.invalidate_source_commands();
        let Some(control) = &self.mesh_pairing else {
            return;
        };
        let invite_code = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        self.session_commands.begin();
        self.owner.clear_failed_connection();
        self.mesh_pairing_snapshot = Some(control.join_from_invite_code(&invite_code));
    }

    fn open_popout_pip_window(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.snapshot();
        let presentation_frame = self.presentation_frame.clone();
        let host_stats = self.host_stats.clone();

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds(
                point(px(200.0), px(120.0)),
                size(px(854.0), px(480.0)),
            ))),
            window_min_size: Some(size(px(480.0), px(270.0))),
            titlebar: Some(TitlebarOptions {
                title: Some(
                    format!(
                        "RemotePlay PiP - {}",
                        snapshot
                            .role
                            .session()
                            .map(|s| s.peer.display_name.as_str())
                            .unwrap_or("Viewer")
                    )
                    .into(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        };

        let scale_mode = self.scale_mode;
        let result = cx.open_window(window_options, move |_window, cx| {
            let view = cx.new(|_cx| PopoutStreamView {
                presentation_frame,
                host_stats,
                scale_mode,
                telemetry_hud_collapsed: true,
            });
            cx.spawn({
                let view = view.clone();
                async move |cx| {
                    loop {
                        let Ok(interval) = view.update(&mut *cx, |view, cx| {
                            cx.notify();
                            if view
                                .host_stats
                                .as_ref()
                                .is_some_and(|stats| stats.media_pause.is_paused())
                            {
                                Duration::from_millis(200)
                            } else {
                                Duration::from_millis(16)
                            }
                        }) else {
                            break;
                        };
                        Timer::after(interval).await;
                    }
                }
            })
            .detach();
            view
        });
        self.status = match result {
            Ok(_) => "PiP window opened".to_string(),
            Err(err) => format!("PiP failed: {err}"),
        };
    }

    fn request_capture_sources(&mut self, cx: &mut Context<Self>) {
        let Some(binding) = self.owner.source_view_binding() else { return; };
        self.request_bound_capture_sources(binding, cx);
    }
    fn invalidate_source_commands(&mut self) {
        self.source_commands.begin(); self.discovery_commands.begin();
    }
    fn request_bound_capture_sources(&mut self, binding: SourceViewBinding, cx: &mut Context<Self>) {
        if !self.owner.source_binding_is_current(&binding) { return; }
        let ticket = self.discovery_commands.begin();
        let owner = self.owner.clone();
        cx.spawn(async move |view, cx| {
            if !view.update(cx, |this, _| this.discovery_commands.is_current(ticket)
                && this.owner.source_binding_is_current(&binding)).unwrap_or(false) { return; }
            if let Err(err) = owner.request_bound_capture_sources(&binding).await {
                let _ = view.update(cx, |this, cx| {
                    if !this.discovery_commands.is_current(ticket) || !owner.source_binding_is_current(&binding) { return; }
                    this.status = format!("Source discovery failed: {err}");
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn start_capture_source_switch(
        &mut self,
        binding: Option<SourceViewBinding>,
        source: protocol::session::CaptureSource,
        label: String,
        cx: &mut Context<Self>,
    ) {
        let Some(binding) = binding.filter(|binding| self.owner.source_binding_is_current(binding)) else { return; };
        let ticket = self.source_commands.begin();
        for event in self.pointer_input.release_events() {
            self.owner.queue_viewing_input(event);
        }
        self.show_apps_menu = false;
        self.toolbar_revealed = true;
        self.toolbar_hovered = false;
        self.last_toolbar_activity = Instant::now();
        let owner = self.owner.clone();
        cx.spawn(async move |view, cx| {
            let Some(values) = view.update(cx, |this, _| {
                (this.source_commands.is_current(ticket) && this.owner.source_binding_is_current(&binding))
                    .then(|| this.stream_settings.values())
            }).unwrap_or(None) else { return; };
            if let Err(err) = owner
                .switch_bound_capture_source(&binding, source, values.width, values.height, values.fps, values.bitrate_kbps)
                .await
            {
                let _ = view.update(cx, |this, cx| {
                    if !this.source_commands.is_current(ticket) || !owner.source_binding_is_current(&binding) { return; }
                    this.status = format!("Switch to {label} failed: {err}");
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn sync_input_session(&mut self, role: &RoleState, cx: &mut Context<Self>) {
        let input_session_id = match role {
            RoleState::Viewing(session) => Some(session.session_id),
            RoleState::Idle | RoleState::Connecting(_) | RoleState::Serving(_) => None,
        };
        if self.input_session_id != input_session_id {
            if self.input_session_id.is_none() && input_session_id.is_some() {
                self.drawer_open = false;
                self.status = "Connected".to_string();
                self.request_capture_sources(cx);
            } else if self.input_session_id.is_some() && input_session_id.is_none() {
                self.drawer_open = true;
                self.status = "Ready".to_string();
                self.reset_host_stats();
            }
            self.pointer_input = PointerInputTracker::default();
            self.input_session_id = input_session_id;
        }
        if !matches!(role, RoleState::Connecting(_) | RoleState::Viewing(_)) {
            self.current_frame = None;
            *self
                .presentation_frame
                .lock()
                .expect("presentation frame lock") = None;
        }
    }

    fn queue_pointer_move(&mut self, position: Point<Pixels>) {
        let cursor_y = f32::from(position.y);
        if should_reveal_toolbar(cursor_y) {
            self.last_toolbar_activity = Instant::now();
            if !self.toolbar_revealed {
                self.toolbar_revealed = true;
            }
        }
        if self.input_locked
            || !self.capture_supports_input
            || self.drawer_open
            || self.show_apps_menu
        {
            return;
        }
        let Some(frame) = self.current_frame.as_ref() else {
            return;
        };
        let Some(bounds) = *self
            .video_surface_bounds
            .lock()
            .expect("video surface bounds lock")
        else {
            return;
        };
        #[cfg(feature="gpui-native-video")]
        if let Some(geometry) = native_frame_geometry(frame) {
            use gpui::native_video::{VideoFit, VideoRect};
            let viewport = VideoRect {
                x: f32::from(bounds.origin.x),
                y: f32::from(bounds.origin.y),
                width: f32::from(bounds.size.width),
                height: f32::from(bounds.size.height),
            };
            let fit = match self.scale_mode {
                ViewportScaleMode::AspectFit => VideoFit::Contain,
                ViewportScaleMode::Fill => VideoFit::Cover,
            };
            if let Some(event)=native_pointer_event(geometry,viewport,fit,[f32::from(position.x),f32::from(position.y)]) {
                self.owner.queue_viewing_input(event);
            }
            return;
        }
        if let Some(event) = absolute_pointer_event(
            (f32::from(position.x), f32::from(position.y)),
            (
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            ),
            (frame.width(), frame.height()),
            self.scale_mode,
        ) {
            self.owner.queue_viewing_input(event);
        }
    }

    fn queue_pointer_button(&mut self, button: MouseButton, pressed: bool) {
        if self.input_locked
            || !self.capture_supports_input
            || self.drawer_open
            || self.show_apps_menu
        {
            return;
        }
        let Some(button) = protocol_mouse_button(button) else {
            return;
        };
        if !pressed && !self.pointer_input.pressed_buttons.contains(&button) {
            return;
        }
        let event = if pressed {
            protocol::InputEvent::MouseDown(button)
        } else {
            protocol::InputEvent::MouseUp(button)
        };
        if self.owner.queue_viewing_input(event) {
            self.pointer_input.button_changed(button, pressed);
        }
    }

    fn queue_pointer_scroll(&mut self, delta: ScrollDelta) {
        if self.input_locked
            || !self.capture_supports_input
            || self.drawer_open
            || self.show_apps_menu
        {
            return;
        }
        let delta = delta.pixel_delta(px(40.0));
        if let Some(event) = self
            .pointer_input
            .scrolled((f32::from(delta.x), f32::from(delta.y)))
        {
            self.owner.queue_viewing_input(event);
        }
    }

    fn queue_key(&mut self, keystroke: &Keystroke, pressed: bool) {
        if self.input_locked
            || !self.capture_supports_input
            || self.drawer_open
            || self.show_apps_menu
        {
            return;
        }
        let Some(event) = protocol_key_event(keystroke, pressed) else {
            return;
        };
        let protocol::InputEvent::Key { key_code, .. } = &event else {
            return;
        };
        let key_code = *key_code;
        if self.owner.queue_viewing_input(event) {
            self.pointer_input.key_changed(key_code, pressed);
        }
    }

    fn queue_modifiers(&mut self, event: &ModifiersChangedEvent) {
        if self.input_locked
            || !self.capture_supports_input
            || self.drawer_open
            || self.show_apps_menu
        {
            return;
        }
        let mut modifiers = protocol_modifiers(event.modifiers);
        if event.capslock.on {
            modifiers |= protocol::input_modifiers::CAPS_LOCK;
        }
        if self
            .owner
            .queue_viewing_input(protocol::InputEvent::ModifiersChanged(modifiers))
        {
            self.pointer_input.modifiers = modifiers;
        }
    }

    fn sync_media_pause(&mut self) {
        let paused = self.manual_media_paused || (self.pause_when_inactive && !self.window_active);
        if paused || self.drawer_open {
            for event in self.pointer_input.release_events() {
                self.owner.queue_viewing_input(event);
            }
        }
        self.owner.set_media_paused(paused);
    }

    fn media_controls(&self, cx: &mut Context<Self>) -> Div {
        let form = self.stream_settings.project();
        let (paused, pending) = self.owner.media_state();
        let (volume, muted) = self.owner.audio();
        let mut controls = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(11.0))
                    .child("Custom frame rate (FPS) and video bitrate (kbps)"),
            )
            .child(
                component::text_input("custom_fps")
                    .content(form.fps_text.to_owned())
                    .on_change({
                        let view = cx.weak_entity();
                        move |text, _, cx| {
                            let _ = view.update(cx, |this, cx| this.dispatch_stream_settings_action(StreamSettingsAction::EditFps(text.to_string()), cx));
                        }
                    }),
            )
            .child(
                component::text_input("custom_bitrate")
                    .content(form.bitrate_text.to_owned())
                    .on_change({
                        let view = cx.weak_entity();
                        move |text, _, cx| {
                            let _ =
                                view.update(cx, |this, cx| this.dispatch_stream_settings_action(StreamSettingsAction::EditBitrate(text.to_string()), cx));
                        }
                    }),
            )
            .child(
                command_button("apply_custom_stream", ActionVariantKind::Primary, cx)
                    .child("Apply custom values")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dispatch_stream_settings_action(StreamSettingsAction::ApplyCustom, cx);
                    })),
            )
            .child(
                command_button("toggle_media_pause", ActionVariantKind::Neutral, cx)
                    .child(if self.manual_media_paused {
                        "Resume media"
                    } else {
                        "Pause media · keep connected"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.manual_media_paused = !this.manual_media_paused;
                        this.sync_media_pause();
                        cx.notify();
                    })),
            )
            .child(
                command_button("pause_in_background", ActionVariantKind::Neutral, cx)
                    .child(if self.pause_when_inactive {
                        "Background pause: on"
                    } else {
                        "Background pause: off"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pause_when_inactive = !this.pause_when_inactive;
                        this.persist_preferences();
                        this.sync_media_pause();
                        cx.notify();
                    })),
            )
            .child(div().text_size(px(11.0)).child(if pending {
                "Waiting for host confirmation…"
            } else if paused {
                "Media paused · connection retained"
            } else {
                "Media enabled"
            }));
        let quieter = self.owner.clone();
        let louder = self.owner.clone();
        let mute = self.owner.clone();
        controls = controls.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    command_button("volume_down", ActionVariantKind::Neutral, cx)
                        .child("−")
                        .on_click(move |_, _, _| {
                            quieter.set_audio(volume.saturating_sub(5), muted)
                        }),
                )
                .child(div().text_size(px(11.)).child(format!("Volume {volume}%")))
                .child(
                    command_button("volume_up", ActionVariantKind::Neutral, cx)
                        .child("+")
                        .on_click(move |_, _, _| {
                            louder.set_audio(volume.saturating_add(5).min(100), muted)
                        }),
                )
                .child(
                    command_button(
                        "remote_audio_mute",
                        if muted {
                            ActionVariantKind::Primary
                        } else {
                            ActionVariantKind::Neutral
                        },
                        cx,
                    )
                    .child(if muted { "Unmute" } else { "Mute" })
                    .on_click(move |_, _, _| mute.set_audio(volume, !muted)),
                ),
        );
        if let Some(error) = form.error {
            controls = controls.child(div().text_size(px(11.0)).child(error.to_owned()));
        }
        controls
    }

    fn acceptance_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(seconds) = std::env::var("REMOTE_PLAY_RESTORED_TEST_SECONDS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|s| (5..=180).contains(s))
        else {
            return false;
        };
        let Ok(output) = std::env::var("REMOTE_PLAY_RESTORED_TEST_OUTPUT") else {
            return false;
        };
        if self.quitting {
            return true;
        }
        if self.test_sequence.is_some(){return self.sequence_acceptance_tick(&output,seconds,cx);}
        if !self.test_connect_started
            && let Ok(key) = std::env::var("REMOTE_PLAY_RESTORED_TEST_DEVICE")
            && self
                .runtime
                .owner
                .runtime()
                .lock()
                .unwrap()
                .devices()
                .iter()
                .any(|d| d.device_id == key && d.online)
        {
            self.test_connect_started = true;
            self.pause_when_inactive = false;
            let owner = self.owner.clone();
            cx.spawn(async move |view, cx| {
                let result = owner.connect_silent_files(&key).await;
                if result.is_ok() {
                    let _ = owner.request_capture_sources().await;
                }
                let _ = view.update(cx, |this, cx| {
                    if let Err(e) = result {
                        this.status = e.to_string();
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        if self.test_connect_started
            && !self.test_source_started
            && let (Ok(title), Some(id), Some(pid)) = (
                std::env::var("REMOTE_PLAY_RESTORED_TEST_TITLE"),
                std::env::var("REMOTE_PLAY_RESTORED_TEST_WINDOW")
                    .ok()
                    .and_then(|v| v.parse::<u32>().ok()),
                std::env::var("REMOTE_PLAY_RESTORED_TEST_PID")
                    .ok()
                    .and_then(|v| v.parse::<i32>().ok()),
            )
        {
            let state = self.owner.snapshot();
            if state.sources.iter().any(|s| {
                s.source == protocol::session::CaptureSource::Window(id)
                    && s.process_id == Some(pid)
                    && s.title == title
            }) {
                self.test_source_started = true;
                let owner = self.owner.clone();
                cx.spawn(async move |view, cx| {
                    let r = owner
                        .switch_capture_source(
                            protocol::session::CaptureSource::Window(id),
                            1280,
                            720,
                            30,
                            6000,
                        )
                        .await;
                    let _ = view.update(cx, |this, cx| {
                        if let Err(e) = r {
                            this.status = e.to_string();
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        // Explicit bounded visual-review state only; never active in normal usage.
        if self.test_source_started
            && let Ok(panel) = std::env::var("REMOTE_PLAY_RESTORED_TEST_PANEL")
        {
            match panel.as_str() {
                "devices" => {
                    self.drawer_open = true;
                    self.active_tab = DrawerTab::Devices;
                }
                "files" => {
                    self.drawer_open = true;
                    self.active_tab = DrawerTab::Files;
                }
                _ => {}
            }
            self.toolbar_revealed = true;
        }
        if self.started.elapsed() >= Duration::from_secs(seconds) {
            owned_gui_stage(12);
            self.quitting = true;
            let result = serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"renderer":"restored-original-gpui",
                "render_calls":self.render_calls,"elapsed_ms":self.started.elapsed().as_millis(),"state":self.owner.diagnostic_snapshot(),
                "frame_boundary":"gpui_scene_submission","actual_display_completion_measured":false,
                "screen_capture_started_locally":false,"input_injected":false,"full_acceptance":false});
            if let Err(e) = std::fs::write(&output, serde_json::to_vec_pretty(&result).unwrap()) {
                eprintln!("RESTORED_TEST_RECEIPT_ERROR {e}");
            }
            println!("RESTORED_NATIVE_GUI_TEST {result}");
            owned_gui_stage(13);
            self.owner.close_all();
            owned_gui_stage(14);
            cx.quit();
            return true;
        }
        false
    }

    fn clear_acceptance_session(&mut self) {
        self.owner.release_input();self.owner.close_all();self.current_frame=None;
        self.presentation_frame.lock().unwrap().take();self.bound_source=None;
        self.pointer_input=PointerInputTracker::default();self.input_locked=true;
        self.test_connect_started=false;self.test_source_started=false;
    }
    fn sequence_acceptance_tick(&mut self,output:&str,seconds:u64,cx:&mut Context<Self>)->bool {
        let pending=self.test_sequence.take().expect("sequence state present");
        let mut sequence=match pending {
            Ok(s)=>s,
            Err(error)=>{
                let record=serde_json::json!({"passed":false,"error":error,"input_injected":false,"full_acceptance":false});
                let _=std::fs::write(output,serde_json::to_vec_pretty(&record).unwrap());
                self.clear_acceptance_session();self.quitting=true;cx.quit();return true;
            }
        };
        if sequence.closing.is_none() {
            if self.started.elapsed()>=Duration::from_secs(seconds)||sequence.phase_expired(){
                sequence.error=Some(format!("bounded source step {} timed out",sequence.index));
                sequence.receipts.push(self.owner.diagnostic_snapshot());
            }
            let source=sequence.step().clone();
            if sequence.error.is_none()&&!self.test_connect_started&&self.runtime.owner.runtime().lock().unwrap().devices().iter().any(|d|d.device_id==source.device&&d.online){
                self.test_connect_started=true;self.pause_when_inactive=false;
                let owner=self.owner.clone();let key=source.device.clone();
                cx.spawn(async move|view,cx|{
                    let result=owner.connect_silent_files(&key).await;
                    if result.is_ok(){let _=owner.request_capture_sources().await;}
                    let _=view.update(cx,|this,cx|{if let Err(e)=result{this.status=e.to_string();}cx.notify();});
                }).detach();
            }
            if sequence.error.is_none()&&self.test_connect_started&&!self.test_source_started {
                let state=self.owner.snapshot();
                if state.sources.iter().any(|s|s.source==protocol::session::CaptureSource::Window(source.window)&&s.process_id==Some(source.pid)&&s.title==source.title){
                    self.test_source_started=true;let owner=self.owner.clone();let id=source.window;
                    cx.spawn(async move|view,cx|{
                        let result=owner.switch_capture_source(protocol::session::CaptureSource::Window(id),1280,720,30,6000).await;
                        let _=view.update(cx,|this,cx|{if let Err(e)=result{this.status=e.to_string();}cx.notify();});
                    }).detach();
                }
            }
            let snapshot=self.owner.diagnostic_snapshot();
            let sessions=snapshot["sessions"].as_array().cloned().unwrap_or_default();
            let current=sessions.first();
            let ready=current.is_some_and(|s|s["device_id"]==source.device&&s["source"]==format!("Window({})",source.window)&&s["route"]==sequence.plan.route&&s["connected"]==true&&s["peer_responsive"]==true&&s["confirmed"]==true&&s["scene_submitted_frame"]==true&&s["input_locked"]==true&&s["decoded_frames"].as_u64().unwrap_or(0)>=sequence.plan.minimum_frames&&s["decode_errors"]==0&&s["video_error"].is_null()&&s["error"]=="");
            if sessions.len()>1||current.is_some_and(|s|s["device_id"]!=source.device){sequence.error=Some("old/wrong device survived explicit close and reconnect".into());}
            if ready {
                let since=sequence.healthy_since.get_or_insert_with(Instant::now);
                if since.elapsed()>=Duration::from_secs(sequence.plan.hold_seconds){
                    sequence.receipts.push(serde_json::json!({"step":sequence.index,"expected_device":source.device,"expected_window":source.window,"state":snapshot,"passed":true}));
                    self.clear_acceptance_session();sequence.index+=1;sequence.healthy_since=None;sequence.phase_started=Instant::now();
                    if sequence.index==sequence.plan.sources.len(){sequence.closing=Some(Instant::now());}
                }
            }else{sequence.healthy_since=None;}
            if sequence.error.is_some(){self.clear_acceptance_session();sequence.closing=Some(Instant::now());}
        }
        if let Some(closing)=sequence.closing {
            let mut native_released=true;
            #[cfg(all(target_os="linux",feature="native-linux-video"))]
            {native_released=gpui::native_video::linux_video_imports_live()==0;}
            if native_released&&closing.elapsed()>Duration::from_millis(300)||closing.elapsed()>Duration::from_secs(3){
                let passed=sequence.error.is_none()&&sequence.index==sequence.plan.sources.len()&&native_released;
                let record=serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"renderer":"restored-original-gpui","passed":passed,"sequence_steps":sequence.receipts,"error":sequence.error,"native_allocations_released_before_exit":native_released,"input_injected":false,"screen_capture_started_locally":false,"actual_display_completion_measured":false,"full_acceptance":false});
                let _=std::fs::write(output,serde_json::to_vec_pretty(&record).unwrap());println!("RESTORED_SEQUENCE_RESULT {record}");
                self.quitting=true;cx.quit();return true;
            }
        }
        self.test_sequence=Some(Ok(sequence));cx.notify();false
    }

    fn open_device_window(&mut self, device_id: &str, cx: &mut Context<Self>) {
        let device = self
            .runtime
            .owner
            .runtime()
            .lock()
            .unwrap()
            .devices()
            .into_iter()
            .find(|d| d.device_id == device_id && d.online);
        if let Some(device) = device {
            open_restored_window(
                self.runtime.clone(),
                None,
                Arc::new(AtomicBool::new(false)),
                Some(device.device_id),
                cx,
            );
        }
    }
    fn paint_acknowledgement(&self) -> AnyElement {
        // One capability predicate owns this path. Keeping separate platform
        // cfgs for four captured values let Linux draw frames but never acknowledge
        // one, leaving its otherwise-working input permanently locked.
        if !native_presentation_available() {
            return div().into_any_element();
        }
        let frame=self.current_frame.clone();
        let owner=self.owner.clone();
        let connection=self.bound_source.map(|(id,_)|id);
        canvas(|_,_,_|(),move |_,(),_,_| {
            if let (Some(id),Some(frame))=(connection,frame.as_ref()) {
                owned_gui_stage(10);
                owner.acknowledge_paint(id,frame);
                owned_gui_stage(11);
            }
        }).absolute().top_0().left_0().w_full().h_full().into_any_element()
    }

    fn session_switcher(&self, cx: &mut Context<Self>) -> Div {
        let view = cx.weak_entity();
        let mut row = div()
            .absolute()
            .bottom(px(14.))
            .left(px(16.))
            .flex()
            .items_center()
            .gap_2();
        let model = self.owner.session_tabs();
        let can_reconnect = model.can_reconnect && model.failed.is_none();
        for tab in model.tabs {
            let id = tab.connection_id;
            let view = view.clone();
            row = row.child(
                command_button(
                    format!("session_{id}"),
                    if tab.selected {
                        ActionVariantKind::Primary
                    } else {
                        ActionVariantKind::Neutral
                    },
                    cx,
                )
                .rounded_full()
                .h(px(28.))
                .text_size(px(10.))
                .child(tab.label)
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.dispatch_session_action(SessionTabsAction::Select(id), cx));
                }),
            );
        }
        if can_reconnect {
            let view = view.clone();
            row = row.child(
                command_button("selected_reconnect", ActionVariantKind::Neutral, cx)
                    .rounded_full()
                    .h(px(28.))
                    .text_size(px(10.))
                    .child("Reconnect")
                    .on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| this.dispatch_session_action(SessionTabsAction::Reconnect, cx));
                    }),
            );
        }
        row.child(
            command_button("quit_restored_app", ActionVariantKind::Neutral, cx)
                .rounded_full()
                .h(px(28.))
                .text_size(px(10.))
                .child("Quit")
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.owner.close_all();
                        this.quitting = true;
                        cx.quit();
                    });
                }),
        )
    }

    fn set_input_locked(&mut self, locked: bool) {
        if locked && !self.input_locked {
            for event in self.pointer_input.release_events() {
                self.owner.queue_viewing_input(event);
            }
        }
        self.owner.set_input_locked(locked);
        self.input_locked = locked;
    }
}

#[cfg(test)]
fn dashboard_refresh_interval(role: RoleKind) -> Duration {
    if role == RoleKind::Viewing {
        Duration::from_millis(16)
    } else {
        Duration::from_millis(100)
    }
}

fn stream_video_canvas_surface(
    frame: Option<&MacDecodedVideoFrame>,
    fallback_title: &'static str,
    fallback_subtitle: &'static str,
    scale_mode: ViewportScaleMode,
    theme: &Theme,
) -> AnyElement {
    if let Some(frame) = frame {
        decoded_video_frame_surface_with_fit(
            frame,
            match scale_mode {
                ViewportScaleMode::AspectFit => ObjectFit::Contain,
                ViewportScaleMode::Fill => ObjectFit::Cover,
            },
        )
    } else {
        div()
            .w_full()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.surface.canvas)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .p_6()
                    .rounded(px(8.0))
                    .bg(color_glass_card())
                    .border_1()
                    .border_color(color_border_fine())
                    .child(
                        div()
                            .size(px(44.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(theme.surface.sunken)
                            .child(
                                icon(IconName::Maximize(false))
                                    .size(px(20.0))
                                    .color(color_accent_cyan()),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.content.primary)
                            .child(fallback_title),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.content.tertiary)
                            .child(fallback_subtitle),
                    ),
            )
            .into_any_element()
    }
}

struct PopoutStreamView {
    presentation_frame: FrameSlot,
    host_stats: Option<Arc<SharedHostStats>>,
    scale_mode: ViewportScaleMode,
    telemetry_hud_collapsed: bool,
}

impl Render for PopoutStreamView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let view = cx.weak_entity();
        let current_frame = self
            .presentation_frame
            .lock()
            .expect("presentation frame lock")
            .clone();
        let stats = self
            .host_stats
            .as_ref()
            .map(|stats| stats.snapshot())
            .filter(host_stats_available);

        let content = stream_video_canvas_surface(
            current_frame.as_deref(),
            "RemotePlay PiP",
            "Waiting for decoded video stream...",
            self.scale_mode,
            &theme,
        );

        let scale_mode = self.scale_mode;
        let scale_btn = command_button("pip_toggle_scale", ActionVariantKind::Neutral, cx)
            .h(px(24.0))
            .px_2()
            .text_size(px(10.0))
            .tooltip(
                tooltip(match scale_mode {
                    ViewportScaleMode::AspectFit => "Scale mode: Aspect Fit (click for Fill)",
                    ViewportScaleMode::Fill => "Scale mode: Fill (click for Fit)",
                })
                .build(),
            )
            .on_click({
                let view = view.clone();
                move |_event, _window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.scale_mode = match this.scale_mode {
                            ViewportScaleMode::AspectFit => ViewportScaleMode::Fill,
                            ViewportScaleMode::Fill => ViewportScaleMode::AspectFit,
                        };
                        if let Err(err) = crate::preferences::UserPreferences::update(|prefs| {
                            prefs.ui.scale_mode = match this.scale_mode {
                                ViewportScaleMode::AspectFit => "aspect_fit".to_string(),
                                ViewportScaleMode::Fill => "fill".to_string(),
                            };
                        }) {
                            eprintln!("Failed to save scale preference: {err}");
                        }
                        cx.notify();
                    });
                }
            })
            .child(match scale_mode {
                ViewportScaleMode::AspectFit => icon(IconName::Maximize(false)).size(px(12.0)),
                ViewportScaleMode::Fill => icon(IconName::Maximize(true)).size(px(12.0)),
            })
            .into_any_element();

        div()
            .relative()
            .w_full()
            .h_full()
            .bg(theme.surface.canvas)
            .text_color(theme.content.primary)
            .font_family(crate::design_system::product_ui_font())
            .child(content)
            .child(
                div()
                    .absolute()
                    .top(px(10.0))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .on_any_mouse_down(cx.listener(|_this, _event, _window, cx| {
                        cx.stop_propagation();
                    }))
                    .child(stream_status_capsule_card(
                        "RemotePlay PiP".to_string(),
                        current_frame.is_some(),
                        compact_telemetry_label(stats.as_ref()),
                        true,
                        &theme,
                        Some(scale_btn),
                    )),
            )
            .child(if self.telemetry_hud_collapsed {
                div()
                    .absolute()
                    .bottom(px(12.0))
                    .right(px(16.0))
                    .on_any_mouse_down(cx.listener(|_this, _event, _window, cx| {
                        cx.stop_propagation();
                    }))
                    .child(
                        command_button("expand_pip_hud_btn", ActionVariantKind::Neutral, cx)
                            .size(px(26.0))
                            .rounded_full()
                            .tooltip(tooltip("Show live telemetry").build())
                            .on_click({
                                let view = view.clone();
                                move |_event, _window, cx| {
                                    let _ = view.update(cx, |this, cx| {
                                        this.telemetry_hud_collapsed = false;
                                        if let Err(err) =
                                            crate::preferences::UserPreferences::update(|prefs| {
                                                prefs.ui.telemetry_hud_collapsed = false;
                                            })
                                        {
                                            eprintln!("Failed to save telemetry preference: {err}");
                                        }
                                        cx.notify();
                                    });
                                }
                            })
                            .child(icon(IconName::PingIndicator(3)).size(px(12.0))),
                    )
            } else {
                div()
                    .absolute()
                    .bottom(px(12.0))
                    .right(px(16.0))
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .p_3()
                    .bg(color_glass_card())
                    .border_1()
                    .border_color(color_border_fine())
                    .rounded(px(8.0))
                    .shadow_lg()
                    .on_any_mouse_down(cx.listener(|_this, _event, _window, cx| {
                        cx.stop_propagation();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(color_accent_cyan())
                                    .child("LIVE TELEMETRY"),
                            )
                            .child(
                                command_button(
                                    "collapse_pip_hud_btn",
                                    ActionVariantKind::Neutral,
                                    cx,
                                )
                                .size(px(18.0))
                                .rounded_full()
                                .text_size(px(8.0))
                                .tooltip(tooltip("Collapse live telemetry").build())
                                .on_click({
                                    let view = view.clone();
                                    move |_event, _window, cx| {
                                        let _ = view.update(cx, |this, cx| {
                                            this.telemetry_hud_collapsed = true;
                                            if let Err(err) =
                                                crate::preferences::UserPreferences::update(
                                                    |prefs| {
                                                        prefs.ui.telemetry_hud_collapsed = true;
                                                    },
                                                )
                                            {
                                                eprintln!(
                                                    "Failed to save telemetry preference: {err}"
                                                );
                                            }
                                            cx.notify();
                                        });
                                    }
                                })
                                .child(icon(IconName::Minimize).size(px(10.0))),
                            ),
                    )
                    .child(telemetry_hud_metrics_list(stats.as_ref(), &theme))
            })
    }
}

impl Render for RestoredDashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        owned_gui_stage(5);
        self.render_calls = self.render_calls.saturating_add(1);
        self.drain_latest_frame();
        owned_gui_stage(6);
        let snapshot = self.snapshot();
        let role = snapshot.role.clone();
        self.capture_supports_input = snapshot.active_capture_supports_input;
        if self.capture_error_seen != snapshot.capture_source_error {
            self.capture_error_seen = snapshot.capture_source_error.clone();
            if self.capture_error_seen.is_some() {
                self.toolbar_revealed = true;
                self.show_apps_menu = true;
                self.last_toolbar_activity = Instant::now();
            }
        }
        if !matches!(role, RoleState::Viewing(_) | RoleState::Connecting(_)) {
            self.manual_media_paused = false;
        }
        self.window_active = window.is_window_active();
        self.sync_media_pause();
        owned_gui_stage(7);
        let media_controls = self.media_controls(cx);
        owned_gui_stage(8);
        self.sync_input_session(&role, cx);
        let active_session = role.session().cloned();
        let theme = cx.theme().clone();
        let show_drawer = self.drawer_open;
        let is_fullscreen = window.is_fullscreen();
        let compact = f32::from(window.viewport_size().width) < 1200.0;
        let host_stats = self.host_stats_snapshot(&role);
        let side_services = SideServiceUiState {
            talkback: FeatureToggleState {
                available: self.owner.supports_talkback(),
                enabled: self.owner.talkback_enabled(),
            },
            clipboard_sync: FeatureToggleState {
                available: self.owner.supports_clipboard_sync(),
                enabled: self.owner.clipboard_sync_enabled(),
            },
            file_transfer: FeatureToggleState {
                available: self.owner.supports_file_transfer(),
                enabled: self.owner.file_transfer_enabled(),
            },
        };

        div()
            .relative()
            .w_full()
            .h_full()
            .min_w_0()
            .min_h_0()
            .bg(theme.surface.canvas)
            .text_color(theme.content.primary)
            .font_family(crate::design_system::product_ui_font())
            .overflow_hidden()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "f11" {
                    window.toggle_fullscreen();
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if event.keystroke.key == "escape"
                    && !event.keystroke.modifiers.control
                    && (this.drawer_open || this.show_apps_menu)
                {
                    this.owner.release_input();
                    this.drawer_open = false;
                    this.show_apps_menu = false;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if event.keystroke.key == "escape"
                    && event.keystroke.modifiers.control
                    && event.keystroke.modifiers.alt
                {
                    this.set_input_locked(true);
                    this.toolbar_revealed = true;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if this.input_session_id.is_some() && this.input_focus.is_focused(window) {
                    this.queue_key(&event.keystroke, true);
                    cx.stop_propagation();
                }
            }))
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                if this.input_session_id.is_some() && this.input_focus.is_focused(window) {
                    this.queue_key(&event.keystroke, false);
                    cx.stop_propagation();
                }
            }))
            .on_modifiers_changed(cx.listener(
                |this, event: &ModifiersChangedEvent, window, _cx| {
                    if this.input_session_id.is_some() && this.input_focus.is_focused(window) {
                        this.queue_modifiers(event);
                    }
                },
            ))
            .child(div().absolute().top_0().left_0().w_full().h_full().child(
                full_canvas_stream_viewport(
                    &role,
                    self.current_frame.as_deref(),
                    self.input_focus.clone(),
                    self.video_surface_bounds.clone(),
                    self.scale_mode,
                    cx,
                ),
            ))
            .when(
                !matches!(role, RoleState::Viewing(_) | RoleState::Connecting(_))
                    || self.toolbar_revealed
                    || self.toolbar_hovered,
                |this| {
                    this.child(floating_control_island(
                        &role,
                        active_session.as_ref(),
                        &self.viewer_media_status,
                        &self.status,
                        self.input_locked,
                        snapshot.active_capture_supports_input,
                        side_services,
                        self.scale_mode,
                        is_fullscreen,
                        compact,
                        host_stats.as_ref(),
                        self.owner.clone(),
                        &snapshot.capture_sources,
                        snapshot.source_binding.as_ref(),
                        snapshot.active_capture_source,
                        snapshot.pending_capture_source,
                        snapshot.capture_source_error.as_deref(),
                        self.show_apps_menu,
                        cx,
                    ))
                },
            )
            .child(self.session_switcher(cx))
            .child(self.paint_acknowledgement())
            .child(drawer_trigger_capsule(
                &snapshot.devices,
                show_drawer,
                compact,
                cx,
            ))
            .when(show_drawer, |this| {
                this.child(slide_over_management_drawer(
                    &snapshot.devices,
                    &role,
                    self.mesh_pairing_snapshot.clone(),
                    self.active_tab,
                    &self.device_list_state,
                    self.input_locked,
                    side_services,
                    self.stream_settings.values().resolution(),
                    self.stream_settings.values().fps,
                    self.stream_settings.values().bitrate_kbps,
                    media_controls,
                    host_stats.as_ref(),
                    self.owner.clone(),
                    cx,
                ))
            })
            .when(
                matches!(role, RoleState::Connecting(_) | RoleState::Viewing(_)),
                |this| {
                    this.child(telemetry_waterfall_overlay(
                        self.telemetry_hud_collapsed,
                        host_stats.as_ref(),
                        cx,
                    ))
                },
            )
            .map(|tree| ely_gpui_component::primitives::FocusScope::new(&self.root_focus).root().size_full().child(tree))
    }
}

fn full_canvas_stream_viewport(
    role: &RoleState,
    frame: Option<&MacDecodedVideoFrame>,
    input_focus: FocusHandle,
    video_surface_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    scale_mode: ViewportScaleMode,
    cx: &mut Context<RestoredDashboard>,
) -> AnyElement {
    match role {
        RoleState::Viewing(_) | RoleState::Connecting(_) => full_video_surface(
            frame,
            if matches!(role, RoleState::Connecting(_)) {
                "Connecting to remote host..."
            } else {
                "Waiting for high-resolution video stream..."
            },
            input_focus,
            video_surface_bounds,
            scale_mode,
            cx,
        ),
        RoleState::Serving(session) => {
            full_serving_stage(&session.peer.display_name, cx).into_any_element()
        }
        RoleState::Idle => full_idle_canvas_stage(cx).into_any_element(),
    }
}

fn full_video_surface(
    frame: Option<&MacDecodedVideoFrame>,
    fallback: &'static str,
    input_focus: FocusHandle,
    video_surface_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    scale_mode: ViewportScaleMode,
    cx: &mut Context<RestoredDashboard>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let content = stream_video_canvas_surface(
        frame,
        fallback,
        "Waiting for the first decoded video frame",
        scale_mode,
        &theme,
    );

    let input_bounds_probe = canvas(
        move |bounds, _window, _cx| {
            *video_surface_bounds
                .lock()
                .expect("video surface bounds lock") = Some(bounds);
        },
        |_bounds, (), _window, _cx| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .w_full()
    .h_full();

    div()
        .id("full_canvas_input_surface")
        .track_focus(&input_focus)
        .relative()
        .w_full()
        .h_full()
        .bg(rgb(0x050706))
        .on_hover(cx.listener(|this, hovered: &bool, _window, _cx| {
            if !hovered {
                this.pointer_input.reset_pointer();
            }
        }))
        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
            let was_revealed = this.toolbar_revealed;
            this.queue_pointer_move(event.position);
            if !was_revealed && this.toolbar_revealed {
                cx.notify();
            }
        }))
        .on_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
            if this.show_apps_menu {
                this.show_apps_menu = false;
                this.owner.release_input();
                this.pointer_input = PointerInputTracker::default();
                this.toolbar_hovered = false;
                this.last_toolbar_activity = Instant::now();
                cx.stop_propagation();
                cx.notify();
                return; // dismissing a local menu is not a remote click
            }
            window.focus(&this.input_focus);
            this.queue_pointer_move(event.position);
            this.queue_pointer_button(event.button, true);
            cx.stop_propagation();
        }))
        .capture_any_mouse_up(cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
            this.queue_pointer_button(event.button, false);
        }))
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _event: &MouseUpEvent, _window, _cx| {
                this.queue_pointer_button(MouseButton::Left, false);
                this.pointer_input.reset_pointer();
            }),
        )
        .on_mouse_up_out(
            MouseButton::Right,
            cx.listener(|this, _event: &MouseUpEvent, _window, _cx| {
                this.queue_pointer_button(MouseButton::Right, false);
                this.pointer_input.reset_pointer();
            }),
        )
        .on_mouse_up_out(
            MouseButton::Middle,
            cx.listener(|this, _event: &MouseUpEvent, _window, _cx| {
                this.queue_pointer_button(MouseButton::Middle, false);
                this.pointer_input.reset_pointer();
            }),
        )
        .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
            this.queue_pointer_scroll(event.delta);
            cx.stop_propagation();
        }))
        .child(content)
        .child(input_bounds_probe)
        .into_any_element()
}

fn full_serving_stage(peer_name: &str, cx: &mut Context<RestoredDashboard>) -> Div {
    let theme = cx.theme().clone();
    div()
        .w_full()
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(0x08090b))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .p_8()
                .rounded(px(8.0))
                .bg(color_glass_card())
                .border_1()
                .border_color(color_border_fine())
                .child(
                    div()
                        .size(px(56.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(theme.status.info.bg)
                        .child(
                            icon(IconName::User)
                                .size(px(26.0))
                                .color(theme.status.info.fg),
                        ),
                )
                .child(
                    div()
                        .text_size(px(17.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.content.primary)
                        .child("Sharing Desktop Stream"),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.content.tertiary)
                        .child(format!("Controlled by remote client {peer_name}")),
                ),
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ControlIslandVisibility {
    show_viewer_controls: bool,
    show_viewer_telemetry: bool,
    show_disconnect: bool,
}

fn control_island_visibility(role: RoleKind) -> ControlIslandVisibility {
    match role {
        RoleKind::Idle => ControlIslandVisibility {
            show_viewer_controls: false,
            show_viewer_telemetry: false,
            show_disconnect: false,
        },
        RoleKind::Connecting => ControlIslandVisibility {
            show_viewer_controls: false,
            show_viewer_telemetry: true,
            show_disconnect: true,
        },
        RoleKind::Viewing => ControlIslandVisibility {
            show_viewer_controls: true,
            show_viewer_telemetry: true,
            show_disconnect: true,
        },
        RoleKind::Serving => ControlIslandVisibility {
            show_viewer_controls: false,
            show_viewer_telemetry: false,
            show_disconnect: true,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn floating_control_island(
    role: &RoleState,
    session: Option<&crate::RoleSession>,
    _viewer_media_status: &UnifiedViewerMediaStatus,
    status: &str,
    input_locked: bool,
    capture_supports_input: bool,
    side_services: SideServiceUiState,
    scale_mode: ViewportScaleMode,
    is_fullscreen: bool,
    compact: bool,
    _host_stats: Option<&HostStats>,
    owner: Arc<OriginalOwner>,
    capture_sources: &[protocol::session::CaptureSourceInfo],
    source_binding: Option<&SourceViewBinding>,
    active_capture_source: protocol::session::CaptureSource,
    pending_capture_source: Option<protocol::session::CaptureSource>,
    capture_source_error: Option<&str>,
    show_apps_menu: bool,
    cx: &mut Context<RestoredDashboard>,
) -> Stateful<Div> {
    let theme = cx.theme().clone();
    let view = cx.weak_entity();
    let visibility = control_island_visibility(role.kind());
    let is_connected = visibility.show_disconnect;
    let talkback_enabled = side_services.talkback.enabled;
    let clipboard_sync_enabled = side_services.clipboard_sync.enabled;
    let talkback_available = side_services.talkback.available;
    let clipboard_sync_available = side_services.clipboard_sync.available;
    let title = session
        .map(|s| s.peer.display_name.clone())
        .unwrap_or_else(|| {
            if status == "Ready" {
                "RemotePlay".to_string()
            } else {
                status.to_string()
            }
        });

    div()
        .id("floating_auto_hide_toolbar_container")
        .absolute()
        .top(px(12.0))
        .left_0()
        .right_0()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(
            div()
                .id("floating_toolbar_pill")
                .occlude()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .bg(color_glass_card())
                .border_1()
                .border_color(color_border_fine())
                .rounded_full()
                .shadow_lg()
                .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                    this.toolbar_hovered = *hovered;
                    this.last_toolbar_activity = Instant::now();
                    cx.notify();
                }))
                .on_mouse_move(cx.listener(|this, _event: &MouseMoveEvent, _window, _cx| {
                    this.last_toolbar_activity = Instant::now();
                }))
                .on_any_mouse_down(cx.listener(|_this, _event, _window, cx| {
                    cx.stop_propagation();
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_1()
                        .child(
                            div()
                                .size(px(8.0))
                                .flex_none()
                                .rounded_full()
                                .bg(if is_connected {
                                    Hsla::from(color_accent_emerald())
                                } else {
                                    theme.content.disabled
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.content.primary)
                                .whitespace_nowrap()
                                .max_w(if compact { px(120.0) } else { px(220.0) })
                                .truncate()
                                .child(title),
                        ),
                )
                .when(visibility.show_viewer_controls, |this| {
                    this.child(
                        div()
                            .w(px(1.0))
                            .h(px(16.0))
                            .bg(theme.border.divider),
                    )
                    .child(display_switcher_control(
                        capture_sources,
                        source_binding,
                        active_capture_source,
                        pending_capture_source,
                        show_apps_menu,
                        cx,
                    ))
                    .child(
                        div()
                            .w(px(1.0))
                            .h(px(16.0))
                            .bg(theme.border.divider),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                        .when(!compact && talkback_available, |this| this.child({
                            let view = view.clone();
                            let owner = owner.clone();
                            command_button(
                                "island_toggle_talkback",
                                if talkback_enabled {
                                    ActionVariantKind::Primary
                                } else {
                                    ActionVariantKind::Neutral
                                },
                                cx,
                            )
                            .disabled(!talkback_available)
                            .h(px(26.0))
                            .px_2()
                            .text_size(px(10.0))
                            .on_click(move |_event, _window, cx| {
                                let _ = view.update(cx, |_this, cx| {
                                    owner.set_talkback_enabled(!talkback_enabled);
                                    cx.notify();
                                });
                            })
                            .child(if !talkback_available {
                                "Mic N/A"
                            } else if talkback_enabled {
                                "Mic ON"
                            } else {
                                "Mic Mute"
                            })
                        }))
                        .when(!compact && clipboard_sync_available, |this| this.child({
                            let view = view.clone();
                            let owner = owner.clone();
                            command_button(
                                "island_toggle_clipboard",
                                if clipboard_sync_enabled {
                                    ActionVariantKind::Primary
                                } else {
                                    ActionVariantKind::Neutral
                                },
                                cx,
                            )
                            .disabled(!clipboard_sync_available)
                            .h(px(26.0))
                            .px_2()
                            .text_size(px(10.0))
                            .on_click(move |_event, _window, cx| {
                                let _ = view.update(cx, |_this, cx| {
                                    owner.set_clipboard_sync_enabled(!clipboard_sync_enabled);
                                    cx.notify();
                                });
                            })
                            .child(if !clipboard_sync_available {
                                "Clip N/A"
                            } else if clipboard_sync_enabled {
                                "Clip ON"
                            } else {
                                "Clip Off"
                            })
                        }))
                        .child({
                            let view = view.clone();
                            command_button(
                                "island_toggle_input_lock",
                                if !input_locked {
                                    ActionVariantKind::Primary
                                } else {
                                    ActionVariantKind::Neutral
                                },
                                cx,
                            )
                            .disabled(!capture_supports_input)
                            .h(px(26.0))
                            .px_2()
                            .text_size(px(10.0))
                            .on_click(move |_event, _window, cx| {
                                if !capture_supports_input {
                                    return;
                                }
                                let _ = view.update(cx, |this, cx| {
                                    this.set_input_locked(!input_locked);
                                    cx.notify();
                                });
                            })
                            .child(if !capture_supports_input {
                                if compact { "View" } else { "View Only" }
                            } else if compact {
                                if input_locked { "Locked" } else { "Input" }
                            } else if input_locked {
                                "Input Locked"
                            } else {
                                "Input Active"
                            })
                        })
                        .child(scale_mode_control(scale_mode, cx)),
                    )
                })
                .child(
                    div()
                        .w(px(1.0))
                        .h(px(16.0))
                        .bg(theme.border.divider),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .when(visibility.show_viewer_controls, |this| this.child({
                            let view = view.clone();
                            command_button("island_popout_pip", ActionVariantKind::Neutral, cx)
                                .h(px(26.0))
                                .px_2()
                                .text_size(px(10.0))
                                .on_click(move |_event, _window, cx| {
                                    let _ = view.update(cx, |this, cx| {
                                        this.open_popout_pip_window(cx);
                                        cx.notify();
                                    });
                                })
                                .child(if compact { "PiP" } else { "Pop-out PiP" })
                        }))
                        .child({
                            command_button("island_toggle_fullscreen", ActionVariantKind::Neutral, cx)
                                .h(px(26.0))
                                .px_2()
                                .text_size(px(10.0))
                                .on_click(move |_event, window, cx| {
                                    window.toggle_fullscreen();
                                    cx.refresh_windows();
                                })
                                .child(if compact {
                                    if is_fullscreen { "Exit Full" } else { "Full" }
                                } else if is_fullscreen {
                                    "Windowed"
                                } else {
                                    "Fullscreen"
                                })
                        })
                        .when(visibility.show_disconnect, |this| {
                            let view = view.clone();
                            this.child(
                                command_button("island_disconnect", ActionVariantKind::Danger, cx)
                                    .h(px(26.0))
                                    .px(if compact { px(8.0) } else { px(12.0) })
                                    .text_size(px(10.0))
                                    .on_click(move |_event, _window, cx| {
                                        let _ = view.update(cx, |this, cx| {
                                            this.dispatch_session_action(SessionTabsAction::Disconnect, cx);
                                        });
                                    })
                                    .child(if compact { "End" } else { "Disconnect" }),
                            )
                        }),
                ),
        )
        .when(show_apps_menu, |this: Stateful<Div>| {
            this.child(capture_source_menu(
                capture_sources,
                source_binding,
                active_capture_source,
                pending_capture_source,
                capture_source_error,
                session,
                cx,
            ))
        })
}

fn display_capture_sources(
    sources: &[protocol::session::CaptureSourceInfo],
) -> Vec<protocol::session::CaptureSourceInfo> {
    let mut displays: Vec<_> = sources
        .iter()
        .filter(|source| {
            matches!(
                source.source,
                protocol::session::CaptureSource::MainDisplay
                    | protocol::session::CaptureSource::Display(_)
            )
        })
        .cloned()
        .collect();
    displays.sort_by_key(|source| {
        let primary_rank = if matches!(source.source, protocol::session::CaptureSource::MainDisplay)
            || source.supports_input
        {
            0
        } else {
            1
        };
        let id = match source.source {
            protocol::session::CaptureSource::MainDisplay => 0,
            protocol::session::CaptureSource::Display(id) => id,
            protocol::session::CaptureSource::Window(_) => u32::MAX,
        };
        (primary_rank, id)
    });
    if displays.is_empty() {
        displays.push(protocol::session::CaptureSourceInfo {
            source: protocol::session::CaptureSource::MainDisplay,
            title: "Main display".into(),
            application: String::new(),
            process_id: None,
            width: 0,
            height: 0,
            supports_input: true,
        });
    }
    displays
}

fn capture_source_is_selected(
    selected: protocol::session::CaptureSource,
    source: &protocol::session::CaptureSourceInfo,
    display_index: usize,
) -> bool {
    selected == source.source
        || (selected == protocol::session::CaptureSource::MainDisplay
            && (matches!(source.source, protocol::session::CaptureSource::MainDisplay)
                || source.supports_input
                || display_index == 0))
}

fn display_switcher_control(
    sources: &[protocol::session::CaptureSourceInfo],
    source_binding: Option<&SourceViewBinding>,
    active_source: protocol::session::CaptureSource,
    pending_source: Option<protocol::session::CaptureSource>,
    show_apps_menu: bool,
    cx: &Context<RestoredDashboard>,
) -> Div {
    let view = cx.weak_entity();
    let selected = active_source;
    let displays = display_capture_sources(sources);
    let mut control = div()
        .flex()
        .items_center()
        .gap_0p5()
        .p(px(2.0))
        .rounded(px(6.0))
        .bg(cx.theme().surface.sunken);

    for (index, source) in displays.into_iter().enumerate() {
        let binding = source_binding.cloned();
        let view = view.clone();
        let label = if source.supports_input {
            format!("Display {}", index + 1)
        } else {
            format!("Display {} · view", index + 1)
        };
        let action_label = label.clone();
        let source_id = source.source;
        let is_selected = capture_source_is_selected(selected, &source, index);
        control = control.child(
            command_button(
                format!("island_display_{}", index + 1),
                if is_selected {
                    ActionVariantKind::Primary
                } else {
                    ActionVariantKind::Neutral
                },
                cx,
            )
            .disabled(pending_source.is_some())
            .h(px(22.0))
            .px_2()
            .text_size(px(9.0))
            .on_click(move |_event, _window, cx| {
                let label = action_label.clone();
                let _ = view.update(cx, |this, cx| {
                    this.start_capture_source_switch(binding.clone(), source_id, label, cx);
                    cx.notify();
                });
            })
            .child(label),
        );
    }

    let view = view.clone();
    let binding = source_binding.cloned();
    control.child(
        command_button(
            "island_display_apps",
            if show_apps_menu || matches!(selected, protocol::session::CaptureSource::Window(_)) {
                ActionVariantKind::Primary
            } else {
                ActionVariantKind::Neutral
            },
            cx,
        )
        .h(px(22.0))
        .px_2()
        .text_size(px(9.0))
        .tooltip(tooltip("Switch displays or application windows").build())
        .on_click(move |_event, _window, cx| {
            let _ = view.update(cx, |this, cx| {
                let Some(binding) = binding.clone().filter(|binding| this.owner.source_binding_is_current(binding)) else { return; };
                this.owner.release_input();
                this.pointer_input = PointerInputTracker::default();
                this.show_apps_menu = !this.show_apps_menu;
                this.toolbar_revealed = true;
                this.last_toolbar_activity = Instant::now();
                if this.show_apps_menu {
                    this.request_bound_capture_sources(binding, cx);
                }
                cx.notify();
            });
        })
        .child("Apps"),
    )
}

fn capture_source_menu(
    sources: &[protocol::session::CaptureSourceInfo],
    source_binding: Option<&SourceViewBinding>,
    active_source: protocol::session::CaptureSource,
    pending_source: Option<protocol::session::CaptureSource>,
    error: Option<&str>,
    session: Option<&crate::RoleSession>,
    cx: &mut Context<RestoredDashboard>,
) -> Stateful<Div> {
    let theme = cx.theme().clone();
    let view = cx.weak_entity();
    let selected = active_source;
    let displays = display_capture_sources(sources);
    let windows: Vec<_> = sources
        .iter()
        .filter(|source| matches!(source.source, protocol::session::CaptureSource::Window(_)))
        // The host already bounds its catalog. The scrollable menu must not
        // silently hide every application after the first sixteen entries.
        .cloned()
        .collect();

    let mut menu = div()
        .id("floating_toolbar_apps_menu")
        .occlude()
        .max_h(px(440.0))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .min_w(px(320.0))
        .max_w(px(440.0))
        .bg(color_glass_card())
        .border_1()
        .border_color(color_border_fine())
        .rounded_xl()
        .shadow_2xl()
        .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
            this.toolbar_hovered = *hovered;
            this.last_toolbar_activity = Instant::now();
            cx.notify();
        }))
        .on_mouse_move(cx.listener(|this, _event: &MouseMoveEvent, _window, _cx| {
            this.last_toolbar_activity = Instant::now();
        }))
        .on_any_mouse_down(cx.listener(|_this, _event, _window, cx| {
            cx.stop_propagation();
        }))
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight::BOLD)
                .text_color(theme.content.primary)
                .child("Displays & Apps"),
        )
        .child(div().w_full().h(px(1.0)).bg(theme.border.divider));

    if let Some(error) = error {
        menu = menu.child(
            div()
                .text_size(px(10.0))
                .text_color(theme.status.error.bg)
                .child(format!("Last switch failed: {error}")),
        );
    }

    for (index, source) in displays.into_iter().enumerate() {
        let binding = source_binding.cloned();
        let view = view.clone();
        let source_id = source.source;
        let label = if source.supports_input {
            format!("Display {}", index + 1)
        } else {
            format!("Display {} · view only", index + 1)
        };
        let detail = source.title.clone();
        let action_label = label.clone();
        let selected = capture_source_is_selected(selected, &source, index);
        menu = menu.child(
            command_button(
                format!("island_menu_display_{}", index + 1),
                if selected {
                    ActionVariantKind::Primary
                } else {
                    ActionVariantKind::Neutral
                },
                cx,
            )
            .disabled(pending_source.is_some())
            .h(px(28.0))
            .px_2()
            .text_size(px(10.0))
            .on_click(move |_event, _window, cx| {
                let label = action_label.clone();
                let _ = view.update(cx, |this, cx| {
                    this.start_capture_source_switch(binding.clone(), source_id, label, cx);
                    cx.notify();
                });
            })
            .child(format!("{label} · {detail}")),
        );
    }

    if !windows.is_empty() {
        menu = menu.child(
            div()
                .pt_1()
                .text_size(px(9.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.content.secondary)
                .child("APPLICATION WINDOWS"),
        );
        for (index, source) in windows.into_iter().enumerate() {
            let binding = source_binding.cloned();
            let view = view.clone();
            let source_id = source.source;
            let title = source.title.clone();
            let app = source.application.clone();
            let action_label = if app.is_empty() {
                title.clone()
            } else {
                format!("{app} — {title}")
            };
            let button_label = action_label.clone();
            let is_selected = selected == source_id;
            menu = menu.child(
                command_button(
                    format!("island_window_{index}"),
                    if is_selected {
                        ActionVariantKind::Primary
                    } else {
                        ActionVariantKind::Neutral
                    },
                    cx,
                )
                .disabled(pending_source.is_some())
                .h(px(28.0))
                .px_2()
                .text_size(px(10.0))
                .on_click(move |_event, _window, cx| {
                    let label = action_label.clone();
                    let _ = view.update(cx, |this, cx| {
                        this.start_capture_source_switch(binding.clone(), source_id, label, cx);
                        cx.notify();
                    });
                })
                .child(button_label),
            );
        }
    } else {
        menu = menu.child(
            div()
                .text_size(px(10.0))
                .text_color(theme.content.secondary)
                .child("No application-window capture sources are available on this host."),
        );
    }

    if let Some((target, _name)) =
        session.map(|s| (s.peer.device_id.clone(), s.peer.display_name.clone()))
    {
        let view = view.clone();
        menu = menu
            .child(div().w_full().h(px(1.0)).bg(theme.border.divider))
            .child(
                command_button("island_open_workspace", ActionVariantKind::Neutral, cx)
                    .h(px(28.0))
                    .px_2()
                    .text_size(px(10.0))
                    .on_click(move |_event, _window, cx| {
                        let _ = view.update(cx, |this, cx| this.open_device_window(&target, cx));
                        let _ = view.update(cx, |this, cx| {
                            this.show_apps_menu = false;
                            cx.notify();
                        });
                    })
                    .child("Open multi-window workspace"),
            );
    }

    menu
}

fn scale_mode_control(current: ViewportScaleMode, cx: &Context<RestoredDashboard>) -> Div {
    let fit_view = cx.weak_entity();
    let fill_view = cx.weak_entity();
    div()
        .flex()
        .items_center()
        .gap_0p5()
        .p(px(2.0))
        .rounded(px(6.0))
        .bg(cx.theme().surface.sunken)
        .child(
            command_button(
                "island_scale_fit",
                if current == ViewportScaleMode::AspectFit {
                    ActionVariantKind::Primary
                } else {
                    ActionVariantKind::Neutral
                },
                cx,
            )
            .h(px(22.0))
            .px_2()
            .text_size(px(9.0))
            .tooltip(tooltip("Fit the entire remote display").build())
            .on_click(move |_event, _window, cx| {
                let _ = fit_view.update(cx, |this, cx| {
                    this.scale_mode = ViewportScaleMode::AspectFit;
                    this.persist_preferences();
                    cx.notify();
                });
            })
            .child("Fit"),
        )
        .child(
            command_button(
                "island_scale_fill",
                if current == ViewportScaleMode::Fill {
                    ActionVariantKind::Primary
                } else {
                    ActionVariantKind::Neutral
                },
                cx,
            )
            .h(px(22.0))
            .px_2()
            .text_size(px(9.0))
            .tooltip(tooltip("Fill the viewport and crop the edges").build())
            .on_click(move |_event, _window, cx| {
                let _ = fill_view.update(cx, |this, cx| {
                    this.scale_mode = ViewportScaleMode::Fill;
                    this.persist_preferences();
                    cx.notify();
                });
            })
            .child("Fill"),
        )
}

fn drawer_trigger_capsule(
    devices: &[AppDevice],
    drawer_open: bool,
    compact: bool,
    cx: &mut Context<RestoredDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    let view = cx.weak_entity();
    let online_count = devices.iter().filter(|d| d.online).count();

    div().absolute().top(px(12.0)).left(px(16.0)).child(
        command_button(
            "toggle_drawer_btn",
            if drawer_open {
                ActionVariantKind::Primary
            } else {
                ActionVariantKind::Neutral
            },
            cx,
        )
        .h(px(34.0))
        .px_3()
        .rounded_full()
        .on_click(move |_event, _window, cx| {
            let _ = view.update(cx, |this, cx| {
                this.drawer_open = !this.drawer_open;
                cx.notify();
            });
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .child(product_mark(cx))
                .when(!compact, |this| {
                    this.child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .whitespace_nowrap()
                            .child("Devices"),
                    )
                })
                .child(
                    div()
                        .px_1p5()
                        .py_0p5()
                        .rounded_full()
                        .bg(theme.surface.sunken)
                        .text_size(px(9.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(color_accent_cyan())
                        .whitespace_nowrap()
                        .child(format!("{online_count}")),
                ),
        ),
    )
}

#[allow(clippy::too_many_arguments)]
fn slide_over_management_drawer(
    devices: &[AppDevice],
    role: &RoleState,
    mesh_pairing_snapshot: Option<MeshPairingSnapshot>,
    active_tab: DrawerTab,
    device_list_state: &DeviceListState,
    input_locked: bool,
    side_services: SideServiceUiState,
    selected_resolution: (u32, u32),
    selected_fps: u32,
    selected_bitrate_kbps: u32,
    media_controls: Div,
    host_stats: Option<&HostStats>,
    owner: Arc<OriginalOwner>,
    cx: &mut Context<RestoredDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    let view = cx.weak_entity();

    div()
        .absolute()
        .top_0()
        .left_0()
        .w(px(390.0))
        .h_full()
        .flex()
        .flex_col()
        .bg(color_glass_card())
        .border_r_1()
        .border_color(color_border_fine())
        .shadow_xl()
        .overflow_hidden()
        .on_any_mouse_down(cx.listener(|_this, _event, _window, cx| {
            cx.stop_propagation();
        }))
        .child(
            div()
                .h(px(56.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .px_4()
                .border_b_1()
                .border_color(theme.border.divider)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(product_mark(cx))
                        .child(
                            div()
                                .text_size(px(14.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.content.primary)
                                .whitespace_nowrap()
                                .truncate()
                                .child("Device & Network Control"),
                        ),
                )
                .child(
                    command_button("close_drawer_btn", ActionVariantKind::Neutral, cx)
                        .size(px(28.0))
                        .flex_none()
                        .rounded_full()
                        .tooltip(tooltip("Close device panel").build())
                        .on_click(move |_event, _window, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.drawer_open = false;
                                cx.notify();
                            });
                        })
                        .text_size(px(11.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("X"),
                ),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .p_2()
                .bg(theme.surface.sunken)
                .child(drawer_tab_button(
                    "Devices",
                    DrawerTab::Devices,
                    active_tab,
                    cx,
                ))
                .child(drawer_tab_button(
                    "Pairing",
                    DrawerTab::DeviceGroup,
                    active_tab,
                    cx,
                ))
                .child(drawer_tab_button("Files", DrawerTab::Files, active_tab, cx))
                .child(drawer_tab_button(
                    "Security",
                    DrawerTab::Security,
                    active_tab,
                    cx,
                ))
                .child(drawer_tab_button(
                    "Settings",
                    DrawerTab::Network,
                    active_tab,
                    cx,
                )),
        )
        .child(
            div()
                .id("drawer_scrollable_container")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_4()
                .child(match active_tab {
                    DrawerTab::Devices => drawer_devices_tab(
                        devices,
                        role,
                        device_list_state,
                        owner.clone(),
                        cx,
                    )
                    .into_any_element(),
                    DrawerTab::DeviceGroup => {
                        drawer_device_group_tab(mesh_pairing_snapshot, cx).into_any_element()
                    }
                    DrawerTab::Files => file_transfer_panel(owner.clone(), cx).into_any_element(),
                    DrawerTab::Security => {
                        drawer_security_tab(input_locked, side_services, owner, cx)
                            .into_any_element()
                    }
                    DrawerTab::Network => drawer_network_telemetry_tab(
                        selected_resolution,
                        selected_fps,
                        selected_bitrate_kbps,
                        media_controls,
                        host_stats,
                        owner,
                        cx,
                    )
                    .into_any_element(),
                }),
        )
}

fn drawer_tab_button(
    label: &'static str,
    tab: DrawerTab,
    current_tab: DrawerTab,
    cx: &Context<RestoredDashboard>,
) -> Button {
    let view = cx.weak_entity();
    let is_active = tab == current_tab;
    command_button(
        format!("tab_{label}"),
        if is_active {
            ActionVariantKind::Primary
        } else {
            ActionVariantKind::Neutral
        },
        cx,
    )
    .h(px(26.0))
    .flex_1()
    .text_size(px(10.0))
    .font_weight(FontWeight::MEDIUM)
    .on_click(move |_event, _window, cx| {
        let _ = view.update(cx, |this, cx| {
            this.active_tab = tab;
            cx.notify();
        });
    })
    .child(label)
}

fn drawer_devices_tab(
    devices: &[AppDevice], role: &RoleState, state: &DeviceListState,
    owner: Arc<OriginalOwner>, cx: &mut Context<RestoredDashboard>,
) -> Div {
    let view = cx.weak_entity();
    let recovery_view = view.clone();
    let model = crate::desktop::device_list::DeviceListModel { devices, role };
    div().flex().flex_col().gap_3()
    .child(crate::desktop::session_recovery::ConnectionRecoveryControls::new(
        owner.session_tabs(), move |action, cx| {
            let _ = recovery_view.update(cx, |this, cx| this.dispatch_session_action(action, cx));
        },
    ))
    .child(crate::desktop::device_drawer::DeviceDrawer::new(
        model.project(state.filter), state.filter,
        move |action, cx| {
            if let Err(error) = view.update(cx, |this, cx| this.dispatch_device_action(action.clone(), cx)) {
                eprintln!("Device drawer action target closed: {error}");
            }
        },
    ))
}

impl RestoredDashboard {
    fn dispatch_stream_settings_action(
        &mut self,
        action: StreamSettingsAction,
        cx: &mut Context<Self>,
    ) {
        let effect = self.stream_settings.reduce(action);
        if let Some(effect) = effect {
            self.persist_preferences();
            let should_update = effect.should_update(false)
                || matches!(
                    self.snapshot().role,
                    RoleState::Viewing(_) | RoleState::Connecting(_)
                );
            if should_update {
                let owner = self.owner.clone();
                // Commit to a pending handshake synchronously; capturing None and
                // yielding would lose settings or apply them to a newer device intent.
                let connection = match owner.prepare_stream_settings_update(effect.values) {
                    Ok(Some(connection)) => connection,
                    Ok(None) => { cx.notify(); return; }
                    Err(error) => {
                        if effect.report_error {
                            self.stream_settings.reduce(StreamSettingsAction::UpdateFailed(
                                effect.receipt, error.to_string(),
                            ));
                        }
                        cx.notify();
                        return;
                    }
                };
                cx.spawn(async move |view, cx| {
                    let current = view
                        .update(cx, |this, _| {
                            this.stream_settings.effect_is_current(effect.receipt)
                                && this.owner.stream_settings_binding_is_current(&connection)
                        })
                        .unwrap_or(false);
                    if !current {
                        return;
                    }
                    let values = effect.values;
                    let result = owner
                        .update_stream_settings(
                            &connection,
                            values.width,
                            values.height,
                            values.fps,
                            values.bitrate_kbps,
                        )
                        .await;
                    if effect.report_error
                        && let Err(error) = result
                    {
                        let _ = view.update(cx, |this, cx| {
                            if this.owner.stream_settings_binding_is_current(&connection) {
                                this.stream_settings
                                    .reduce(StreamSettingsAction::UpdateFailed(
                                        effect.receipt,
                                        error.to_string(),
                                    ));
                                cx.notify();
                            }
                        });
                    }
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn dispatch_session_action(&mut self, action: SessionTabsAction, cx: &mut Context<Self>) {
        let Some(effect) = self.owner.session_tabs().effect(action) else { return; };
        self.invalidate_source_commands();
        let ticket = self.session_commands.begin();
        match effect {
            SessionTabsEffect::Select(id) => self.owner.select(id),
            SessionTabsEffect::Close(id) => self.owner.close_session(id),
            SessionTabsEffect::Disconnect | SessionTabsEffect::Reconnect => {
                let disconnect = effect == SessionTabsEffect::Disconnect;
                self.status = if disconnect { "Disconnecting" } else { "Retrying connection" }.into();
                let owner = self.owner.clone();
                cx.spawn(async move |view, cx| {
                    let Some(options) = view.update(cx, |this, _| {
                        if !this.session_commands.is_current(ticket) { return None; }
                        let values = this.stream_settings.values();
                        Some(StreamStartOptions { width: values.width, height: values.height,
                            fps: values.fps, bitrate_kbps: values.bitrate_kbps })
                    }).unwrap_or(None) else { return; };
                    let result = if disconnect { owner.disconnect_active().await } else { owner.reconnect_active(options).await };
                    let _ = view.update(cx, |this, cx| {
                        if !this.session_commands.is_current(ticket) { return; }
                        if disconnect {
                            match result {
                                Ok(_) => { this.status = "Ready".into(); this.drawer_open = true; }
                                Err(error) => this.status = format!("Disconnect failed: {error}"),
                            }
                        } else if let Err(error) = result {
                            if !owner.connection_error_is_current(&error) { return; }
                            this.status = error.to_string(); this.drawer_open = true;
                            this.active_tab = DrawerTab::Devices;
                        }
                        cx.notify();
                    });
                }).detach();
            }
        }
        cx.notify();
    }

    fn dispatch_device_action(&mut self, action: DeviceListAction, cx: &mut Context<Self>) {
        let Some(effect) = self.device_list_state.reduce(action) else { cx.notify(); return; };
        match effect {
            DeviceListEffect::ConnectStream(device_id) => {
                self.invalidate_source_commands();
                let ticket = self.session_commands.begin();
                self.status = format!("Connecting to {device_id}");
                self.reset_host_stats();
                let owner = self.owner.clone();
                cx.spawn(async move |view, cx| {
                    let Some(options) = view.update(cx, |this, _| {
                        if !this.session_commands.is_current(ticket) { return None; }
                        let values = this.stream_settings.values();
                        Some(StreamStartOptions {
                            width: values.width, height: values.height,
                            fps: values.fps, bitrate_kbps: values.bitrate_kbps,
                        })
                    }).unwrap_or(None) else { return; };
                    let result = owner.connect_device(&device_id, options, crate::unix_now_ms()).await;
                    let _ = view.update(cx, |this, cx| {
                        if !this.session_commands.is_current(ticket) { return; }
                        match result {
                            Ok(_) => this.status = "Waiting for video".into(),
                            Err(error) => {
                                if !owner.connection_error_is_current(&error) { return; }
                                this.status = format!("Connection failed: {error}"); this.drawer_open=true;
                                this.active_tab = DrawerTab::Devices;
                            }
                        }
                        cx.notify();
                    });
                }).detach();
            }
            DeviceListEffect::ConnectFiles(device_id) => {
                self.invalidate_source_commands();
                let ticket = self.session_commands.begin();
                self.owner.release_input(); self.drawer_open=true; self.active_tab=DrawerTab::Files;
                self.status="Connecting files without starting video".into();
                let owner=self.owner.clone();
                cx.spawn(async move |view, cx| {
                    if !view.update(cx, |this, _| this.session_commands.is_current(ticket)).unwrap_or(false) { return; }
                    let result=owner.connect_files(&device_id).await;
                    let _=view.update(cx,|this,cx|{
                        if !this.session_commands.is_current(ticket) { return; }
                        if let Err(error)=result {
                            if !owner.connection_error_is_current(&error) { return; }
                            this.status=format!("Files unavailable: {error}");
                            this.active_tab=DrawerTab::Devices;
                        } cx.notify();
                    });
                }).detach();
            }
            DeviceListEffect::OpenWorkspace(device_id) => self.open_device_window(&device_id,cx),
        }
        cx.notify();
    }
}

fn drawer_device_group_tab(
    mesh_pairing_snapshot: Option<MeshPairingSnapshot>,
    cx: &mut Context<RestoredDashboard>,
) -> Div {
    let mut content = div().flex().flex_col().gap_3();

    if let Some(snapshot) = mesh_pairing_snapshot {
        content = content.child(mesh_pairing_card(snapshot, cx));
    }

    content
}

fn drawer_security_tab(
    input_locked: bool,
    side_services: SideServiceUiState,
    owner: Arc<OriginalOwner>,
    cx: &mut Context<RestoredDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    let view = cx.weak_entity();
    let talkback_enabled = side_services.talkback.enabled;
    let clipboard_sync_enabled = side_services.clipboard_sync.enabled;
    let file_transfer_enabled = side_services.file_transfer.enabled;

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_size(px(12.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.content.primary)
                .child("Session Permissions"),
        )
        .child(security_toggle_row(
            "Keyboard & Pointer Input",
            "Send this device's input to the remote session",
            !input_locked,
            true,
            {
                let view = view.clone();
                move |cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.set_input_locked(!input_locked);
                        cx.notify();
                    });
                }
            },
            cx,
        ))
        .child(security_toggle_row(
            "Viewer Microphone Talkback",
            "Enable bidirectional Opus audio talkback stream",
            talkback_enabled,
            side_services.talkback.available,
            {
                let view = view.clone();
                let owner = owner.clone();
                move |cx| {
                    let _ = view.update(cx, |_this, cx| {
                        owner.set_talkback_enabled(!talkback_enabled);
                        cx.notify();
                    });
                }
            },
            cx,
        ))
        .child(security_toggle_row(
            "Bidirectional Clipboard Sync",
            "Synchronize text and images across the active session",
            clipboard_sync_enabled,
            side_services.clipboard_sync.available,
            {
                let view = view.clone();
                let owner = owner.clone();
                move |cx| {
                    let _ = view.update(cx, |_this, cx| {
                        owner.set_clipboard_sync_enabled(!clipboard_sync_enabled);
                        cx.notify();
                    });
                }
            },
            cx,
        ))
        .child(security_toggle_row(
            "File Transfer",
            "Transfer files across the active session",
            file_transfer_enabled,
            side_services.file_transfer.available,
            {
                let view = view.clone();
                let owner = owner.clone();
                move |cx| {
                    let _ = view.update(cx, |_this, cx| {
                        owner.set_file_transfer_enabled(!file_transfer_enabled);
                        cx.notify();
                    });
                }
            },
            cx,
        ))
        .when(
            file_transfer_enabled && side_services.file_transfer.available,
            |this| this.child(file_transfer_panel(owner, cx)),
        )
}

fn file_transfer_panel(owner: Arc<OriginalOwner>, cx: &mut Context<RestoredDashboard>) -> Div {
    let theme = cx.theme().clone();
    let view = cx.weak_entity();
    let mut panel = div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(px(8.))
        .bg(theme.surface.raised)
        .border_1()
        .border_color(color_border_fine())
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("Files & transfers"),
        );
    let send_owner = owner.clone();
    let share_owner = owner.clone();
    let folder_owner = owner.clone();
    panel = panel.child(
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                command_button("files_send", ActionVariantKind::Primary, cx)
                    .child("Send files")
                    .disabled(!owner.supports_file_transfer())
                    .on_click(move |_, _, _| pick_session_files(send_owner.clone(), false)),
            )
            .child(
                command_button("files_publish", ActionVariantKind::Neutral, cx)
                    .child("Publish files")
                    .on_click(move |_, _, _| pick_session_files(share_owner.clone(), true)),
            )
            .child(
                command_button("files_receive_dir", ActionVariantKind::Neutral, cx)
                    .child("Receive folder")
                    .on_click(move |_, _, _| {
                        let owner = folder_owner.clone();
                        tokio::spawn(async move {
                            if let Some(dir) = rfd::AsyncFileDialog::new().pick_folder().await {
                                owner.set_file_receive_dir(dir.path().to_path_buf());
                            }
                        });
                    }),
            ),
    );
    panel = panel.child(
        div()
            .text_size(px(10.))
            .text_color(theme.content.tertiary)
            .child(format!("Receive: {}", owner.receive_dir().display())),
    );
    let binding = owner.file_view_binding();
    let refresh_binding = binding.clone();
    let refresh = owner.clone();
    panel = panel.child(
        command_button("files_refresh", ActionVariantKind::Neutral, cx)
            .child("Refresh shared files")
            .on_click(move |_, _, cx| {
                refresh.list_bound_files(&refresh_binding, 0);
                let _ = view.update(cx, |_, cx| cx.notify());
            }),
    );
    let (files, previous, next, pending) = owner.file_snapshot();
    if files.is_empty() {
        panel = panel.child(
            div()
                .text_size(px(11.))
                .text_color(theme.content.tertiary)
                .child(if pending {
                    "Reading shared files…"
                } else {
                    "No files published by this device. Publish on the other device, then refresh."
                }),
        );
    }
    for file in files {
        let file_binding = binding.clone();
        let file_owner = owner.clone();
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.))
                        .child(format!("{} · {} bytes", file.name, file.size_bytes)),
                )
                .child(
                    command_button(format!("pull_{}", file.id), ActionVariantKind::Primary, cx)
                        .child("Pull")
                        .on_click(move |_, _, _| {
                            file_owner.fetch_bound_file(&file_binding, file.id)
                        }),
                ),
        );
    }
    let mut pager = div().flex().gap_2();
    if let Some(after) = previous {
        let page_binding = binding.clone();
        let o = owner.clone();
        pager = pager.child(
            command_button("files_previous", ActionVariantKind::Neutral, cx)
                .child("Previous")
                .disabled(pending)
                .on_click(move |_, _, _| o.list_bound_files(&page_binding, after)),
        );
    }
    if let Some(after) = next {
        let page_binding = binding.clone();
        let o = owner.clone();
        pager = pager.child(
            command_button("files_next", ActionVariantKind::Neutral, cx)
                .child("Next")
                .disabled(pending)
                .on_click(move |_, _, _| o.list_bound_files(&page_binding, after)),
        );
    }
    panel = panel.child(pager);
    for transfer in owner.file_transfer_snapshot() {
        let mut row = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded(px(6.))
            .bg(theme.surface.sunken)
            .child(div().text_size(px(11.)).child(format!(
                "{} · {:.0}%",
                transfer.label,
                transfer.progress() * 100.
            )))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(theme.content.secondary)
                    .child(transfer.detail.clone()),
            );
        if transfer.is_running()
            && let Some(target) = transfer.cancel_target
        {
            let transfer_binding = binding.clone();
            let o = owner.clone();
            row = row.child(
                command_button(
                    format!("cancel_{:?}", target),
                    ActionVariantKind::Danger,
                    cx,
                )
                .child("Cancel")
                .on_click(move |_, _, _| o.cancel_bound_transfer(&transfer_binding, target)),
            );
        }
        panel = panel.child(row);
    }
    panel
}
fn pick_session_files(owner: Arc<OriginalOwner>, share: bool) {
    owner.release_input();
    let target = owner.active_connection().map(|c| Arc::downgrade(&c));
    let scope = remote_core::shared_files::current_share_scope();
    tokio::spawn(async move {
        let Some(files) = rfd::AsyncFileDialog::new()
            .set_title(if share {
                "Publish only selected files"
            } else {
                "Send files to this connection"
            })
            .pick_files()
            .await
        else {
            return;
        };
        if scope != remote_core::shared_files::current_share_scope() {
            return;
        }
        let paths: Vec<_> = files.into_iter().map(|f| f.path().to_path_buf()).collect();
        if share {
            for path in paths {
                if scope != remote_core::shared_files::current_share_scope() {
                    break;
                }
                let _ = remote_core::shared_files::shared_file_catalog()
                    .publish(&path, scope)
                    .await;
            }
        } else if let Some(target) = target {
            owner.send_files_to(&target, scope, paths);
        }
    });
}

fn security_toggle_row(
    title: &'static str,
    detail: &'static str,
    enabled: bool,
    available: bool,
    on_toggle: impl Fn(&mut App) + 'static,
    cx: &Context<RestoredDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    let on_toggle = Arc::new(on_toggle);
    let on_toggle_click = on_toggle.clone();

    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .p_3()
        .bg(theme.surface.raised)
        .border_1()
        .border_color(color_border_fine())
        .rounded(px(8.0))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.content.primary)
                        .whitespace_nowrap()
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .mt(px(2.0))
                        .text_size(px(9.0))
                        .text_color(if available {
                            theme.content.tertiary
                        } else {
                            theme.content.disabled
                        })
                        .whitespace_nowrap()
                        .truncate()
                        .child(if available {
                            detail.to_string()
                        } else {
                            format!("{detail} / Unavailable")
                        }),
                ),
        )
        .child(
            command_button(format!("toggle_{title}"), ActionVariantKind::Neutral, cx)
                .disabled(!available)
                .h(px(24.0))
                .w(px(42.0))
                .px(px(4.0))
                .flex_none()
                .tooltip(
                    tooltip(format!(
                        "{title}: {}",
                        if !available {
                            "unavailable"
                        } else if enabled {
                            "on"
                        } else {
                            "off"
                        }
                    ))
                    .build(),
                )
                .on_click(move |_event, _window, cx| {
                    on_toggle_click(cx);
                })
                .child(
                    div()
                        .w(px(30.0))
                        .h(px(16.0))
                        .flex()
                        .items_center()
                        .when(enabled && available, |this| this.justify_end())
                        .when(!enabled || !available, |this| this.justify_start())
                        .px(px(2.0))
                        .rounded_full()
                        .bg(if enabled && available {
                            Hsla::from(color_accent_emerald())
                        } else {
                            theme.content.disabled
                        })
                        .child(div().size(px(12.0)).rounded_full().bg(rgb(0xf8fafc))),
                ),
        )
}

const HOST_TELEMETRY_STALE_AFTER: Duration = Duration::from_secs(5);

fn host_stats_available(stats: &HostStats) -> bool {
    stats.fps.is_finite()
        && stats.latency.is_finite()
        && stats.jitter.is_finite()
        && stats.updated_at.is_some_and(|updated_at| {
            Instant::now().saturating_duration_since(updated_at) <= HOST_TELEMETRY_STALE_AFTER
        })
        && (stats.fps > 0.0 || stats.latency > 0.0 || stats.jitter > 0.0 || stats.bitrate_kbps > 0)
}

fn compact_telemetry_label(stats: Option<&HostStats>) -> String {
    stats.map_or_else(
        || "Awaiting telemetry".to_string(),
        |stats| {
            if stats.e2e_latency_ms > 0.0 {
                format!(
                    "{:.0} FPS / E2E {:.1} ms (Net {:.1}ms | Enc {:.1}ms)",
                    stats.fps, stats.e2e_latency_ms, stats.rtt_ms, stats.latency
                )
            } else {
                format!("{:.0} FPS / enc {:.1} ms", stats.fps, stats.latency)
            }
        },
    )
}

#[allow(dead_code)]
fn viewer_media_status_label(status: &UnifiedViewerMediaStatus) -> &'static str {
    match status {
        UnifiedViewerMediaStatus::Disabled => "Viewer media disabled",
        UnifiedViewerMediaStatus::Ready => "Awaiting telemetry",
        UnifiedViewerMediaStatus::AudioUnavailable { .. } => "Video ready / audio unavailable",
        UnifiedViewerMediaStatus::Unavailable { .. } => "Viewer media unavailable",
    }
}

fn format_bitrate(bitrate_kbps: u32) -> String {
    if bitrate_kbps >= 1_000 {
        format!("{:.1} Mbps", bitrate_kbps as f32 / 1_000.0)
    } else {
        format!("{bitrate_kbps} kbps")
    }
}

fn drawer_network_telemetry_tab(
    selected_resolution: (u32, u32),
    selected_fps: u32,
    selected_bitrate_kbps: u32,
    media_controls: Div,
    host_stats: Option<&HostStats>,
    _owner: Arc<OriginalOwner>,
    cx: &mut Context<RestoredDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    let view = cx.weak_entity();
    let mut content = div().flex().flex_col().gap_3().child(
        div()
            .text_size(px(12.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.content.primary)
            .whitespace_nowrap()
            .truncate()
            .child("Live Transport & Latency Telemetry"),
    );

    if let Some(stats) = host_stats {
        if stats.e2e_latency_ms > 0.0 {
            content = content.child(telemetry_metric_row(
                "End-to-End Latency (Total)",
                format!("{:.1} ms", stats.e2e_latency_ms),
                color_accent_cyan(),
                cx,
            ));
        }
        if stats.rtt_ms > 0.0 {
            content = content.child(telemetry_metric_row(
                "Network RTT",
                format!("{:.1} ms", stats.rtt_ms),
                color_accent_purple(),
                cx,
            ));
        }
        content = content
            .child(telemetry_metric_row(
                "Video frame rate",
                format!("{:.0} FPS", stats.fps),
                color_accent_cyan(),
                cx,
            ))
            .child(telemetry_metric_row(
                "Video bitrate",
                format_bitrate(stats.bitrate_kbps),
                color_accent_emerald(),
                cx,
            ))
            .child(telemetry_metric_row(
                "Host encode time",
                format!("{:.1} ms", stats.latency),
                color_accent_amber(),
                cx,
            ))
            .child(telemetry_metric_row(
                "Network RTT",
                format!("{:.1} ms", stats.rtt_ms),
                color_accent_cyan(),
                cx,
            ))
            .child(telemetry_metric_row(
                "Client decode time",
                format!("{:.1} ms", stats.decode_latency_ms),
                color_accent_purple(),
                cx,
            ))
            .child(telemetry_metric_row(
                "Packet loss rate",
                format!("{:.1}%", stats.packet_loss_rate),
                if stats.packet_loss_rate > 2.0 {
                    color_accent_amber()
                } else {
                    color_accent_emerald()
                },
                cx,
            ))
            .child(telemetry_metric_row(
                "Link status",
                if stats.link_status.is_empty() {
                    "🟢 流畅极佳".to_string()
                } else {
                    stats.link_status.to_string()
                },
                color_accent_emerald(),
                cx,
            ));

        if let Some(report) = &stats.pipeline_report {
            for stage in protocol::StageId::ALL {
                let st = &report.stage_stats[stage as usize];
                if st.sample_count > 0 {
                    content = content.child(telemetry_metric_row(
                        format!("{}. {}", stage.wire_id() + 1, stage.display_name()),
                        format!(
                            "{:.1}ms (p50: {:.1}ms, p99: {:.1}ms)",
                            st.avg_us as f32 / 1000.0,
                            st.p50_us as f32 / 1000.0,
                            st.p99_us as f32 / 1000.0
                        ),
                        color_accent_purple(),
                        cx,
                    ));
                }
            }
        }
    } else {
        content = content.child(empty_state(
            "Waiting for telemetry",
            "Live measurements appear after the remote host begins streaming.",
            cx,
        ));
    }

    content = content.child(media_controls);
    // Full Stream Tuning Controls (Resolution, FPS, Bitrate)
    let cur_res = selected_resolution;
    let cur_fps = selected_fps;
    let cur_bitrate = selected_bitrate_kbps;

    content = content.child(
        div()
            .mt(px(2.0))
            .p_3()
            .bg(theme.surface.sunken)
            .border_1()
            .border_color(color_border_fine())
            .rounded(px(6.0))
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.content.primary)
                    .child("Stream Resolution, FPS & Bitrate Control"),
            )
            // 1. Resolution
            .child(
                div().flex().flex_col().gap_1().child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.content.secondary)
                        .child("Target Resolution:"),
                ).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1p5()
                        .children(vec![
                            (1280, 720, "720p HD"),
                            (1920, 1080, "1080p FHD"),
                            (2560, 1440, "2K QHD"),
                            (3840, 2160, "4K UHD"),
                        ].into_iter().map(|(w, h, label)| {
                            let view = view.clone();
                            let is_active = cur_res == (w, h);
                            let variant = if is_active {
                                ActionVariantKind::Primary
                            } else {
                                ActionVariantKind::Neutral
                            };
                            command_button(
                                format!("res_{w}x{h}"),
                                variant,
                                cx,
                            )
                            .h(px(24.0))
                            .text_size(px(10.0))
                            .on_click(move |_event, _window, cx| {
                                let view = view.clone();
                                let _ = view.update(cx, |this, cx| {
                                    this.dispatch_stream_settings_action(StreamSettingsAction::SelectResolution(w, h), cx);
                                });
                            })
                            .child(label)
                        }))
                )
            )
            // 2. Frame Rate (FPS)
            .child(
                div().flex().flex_col().gap_1().child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.content.secondary)
                        .child("Capture & Stream Frame Rate:"),
                ).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1p5()
                        .children(vec![
                            (30, "30 FPS (Power Save)"),
                            (60, "60 FPS (Standard)"),
                            (120, "120 FPS (High-Hz)"),
                        ].into_iter().map(|(fps, label)| {
                            let view = view.clone();
                            let is_active = cur_fps == fps;
                            let variant = if is_active {
                                ActionVariantKind::Primary
                            } else {
                                ActionVariantKind::Neutral
                            };
                            command_button(
                                format!("fps_{fps}"),
                                variant,
                                cx,
                            )
                            .h(px(24.0))
                            .text_size(px(10.0))
                            .on_click(move |_event, _window, cx| {
                                let view = view.clone();
                                let _ = view.update(cx, |this, cx| {
                                    this.dispatch_stream_settings_action(StreamSettingsAction::SelectFps(fps), cx);
                                });
                            })
                            .child(label)
                        }))
                )
            )
            // 3. Bitrate
            .child(
                div().flex().flex_col().gap_1().child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.content.secondary)
                        .child("Video Bitrate (CBR Data Rate Limit):"),
                ).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1p5()
                        .children(vec![
                            (2_000, "2 Mbps"),
                            (5_000, "5 Mbps"),
                            (10_000, "10 Mbps"),
                            (20_000, "20 Mbps"),
                            (40_000, "40 Mbps"),
                            (80_000, "80 Mbps"),
                        ].into_iter().map(|(kbps, label)| {
                            let view = view.clone();
                            let is_active = cur_bitrate == kbps;
                            let variant = if is_active {
                                ActionVariantKind::Primary
                            } else {
                                ActionVariantKind::Neutral
                            };
                            command_button(
                                format!("bitrate_{kbps}"),
                                variant,
                                cx,
                            )
                            .h(px(24.0))
                            .text_size(px(10.0))
                            .on_click(move |_event, _window, cx| {
                                let view = view.clone();
                                let _ = view.update(cx, |this, cx| {
                                    this.dispatch_stream_settings_action(StreamSettingsAction::SelectBitrate(kbps), cx);
                                });
                            })
                            .child(label)
                        }))
                )
            ),
    );

    content
}

fn telemetry_metric_row(
    label: impl Into<SharedString>,
    value: String,
    color: gpui::Rgba,
    cx: &Context<RestoredDashboard>,
) -> Div {
    let theme = cx.theme().clone();

    div()
        .flex()
        .p_2()
        .bg(theme.surface.raised)
        .border_1()
        .border_color(color_border_fine())
        .rounded(px(6.0))
        .flex()
        .justify_between()
        .items_center()
        .gap_2()
        .text_size(px(10.0))
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.content.primary)
                .whitespace_nowrap()
                .truncate()
                .child(label.into()),
        )
        .child(
            div()
                .font_family(crate::design_system::product_mono_font())
                .text_color(color)
                .whitespace_nowrap()
                .child(value),
        )
}

fn telemetry_waterfall_overlay(
    collapsed: bool,
    host_stats: Option<&HostStats>,
    cx: &mut Context<RestoredDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    let view = cx.weak_entity();

    if collapsed {
        div()
            .absolute()
            .bottom(px(12.0))
            .right(px(16.0))
            .on_any_mouse_down(cx.listener(|_this, _event, _window, cx| {
                cx.stop_propagation();
            }))
            .child(
                command_button("expand_hud_btn", ActionVariantKind::Neutral, cx)
                    .size(px(28.0))
                    .rounded_full()
                    .tooltip(tooltip("Show live telemetry").build())
                    .on_click(move |_event, _window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.telemetry_hud_collapsed = false;
                            this.persist_preferences();
                            cx.notify();
                        });
                    })
                    .child(icon(IconName::PingIndicator(3)).size(px(13.0))),
            )
    } else {
        div()
            .absolute()
            .bottom(px(12.0))
            .right(px(16.0))
            .flex()
            .flex_col()
            .gap_1p5()
            .p_3()
            .bg(color_glass_card())
            .border_1()
            .border_color(color_border_fine())
            .rounded(px(8.0))
            .shadow_lg()
            .on_any_mouse_down(cx.listener(|_this, _event, _window, cx| {
                cx.stop_propagation();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .text_size(px(10.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(color_accent_cyan())
                            .child("LIVE TELEMETRY"),
                    )
                    .child(
                        command_button("collapse_hud_btn", ActionVariantKind::Neutral, cx)
                            .size(px(18.0))
                            .rounded_full()
                            .text_size(px(8.0))
                            .tooltip(tooltip("Collapse live telemetry").build())
                            .on_click(move |_event, _window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.telemetry_hud_collapsed = true;
                                    this.persist_preferences();
                                    cx.notify();
                                });
                            })
                            .child(icon(IconName::Minimize).size(px(10.0))),
                    ),
            )
            .child(telemetry_hud_metrics_list(host_stats, &theme))
    }
}

fn telemetry_hud_metrics_list(host_stats: Option<&HostStats>, theme: &Theme) -> Div {
    if let Some(stats) = host_stats {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .text_size(px(10.0))
                    .font_family(crate::design_system::product_mono_font())
                    .child(div().text_color(theme.content.tertiary).child("Frame Rate"))
                    .child(
                        div()
                            .text_color(color_accent_cyan())
                            .child(format!("{:.0} FPS", stats.fps)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .text_size(px(10.0))
                    .font_family(crate::design_system::product_mono_font())
                    .child(div().text_color(theme.content.tertiary).child("Bitrate"))
                    .child(
                        div()
                            .text_color(theme.content.secondary)
                            .child(format_bitrate(stats.bitrate_kbps)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .text_size(px(10.0))
                    .font_family(crate::design_system::product_mono_font())
                    .child(div().text_color(theme.content.tertiary).child("End-to-End"))
                    .child(
                        div()
                            .text_color(color_accent_emerald())
                            .child(format!("{:.1} ms", stats.e2e_latency_ms)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .text_size(px(10.0))
                    .font_family(crate::design_system::product_mono_font())
                    .child(
                        div()
                            .text_color(theme.content.tertiary)
                            .child("Network RTT"),
                    )
                    .child(
                        div()
                            .text_color(theme.content.secondary)
                            .child(format!("{:.1} ms", stats.rtt_ms)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .text_size(px(10.0))
                    .font_family(crate::design_system::product_mono_font())
                    .child(
                        div()
                            .text_color(theme.content.tertiary)
                            .child("Host Encode"),
                    )
                    .child(
                        div()
                            .text_color(theme.content.secondary)
                            .child(format!("{:.1} ms", stats.latency)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .text_size(px(10.0))
                    .font_family(crate::design_system::product_mono_font())
                    .child(
                        div()
                            .text_color(theme.content.tertiary)
                            .child("Client Decode"),
                    )
                    .child(
                        div()
                            .text_color(theme.content.secondary)
                            .child(format!("{:.1} ms", stats.decode_latency_ms)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .text_size(px(10.0))
                    .font_family(crate::design_system::product_mono_font())
                    .child(
                        div()
                            .text_color(theme.content.tertiary)
                            .child("Packet Loss"),
                    )
                    .child(
                        div()
                            .text_color(if stats.packet_loss_rate > 2.0 {
                                color_accent_amber().into()
                            } else {
                                theme.content.secondary
                            })
                            .child(format!("{:.1}%", stats.packet_loss_rate)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .text_size(px(10.0))
                    .font_family(crate::design_system::product_mono_font())
                    .child(
                        div()
                            .text_color(theme.content.tertiary)
                            .child("Link Health"),
                    )
                    .child(
                        div()
                            .text_color(if stats.link_status.is_empty() {
                                color_accent_emerald().into()
                            } else {
                                theme.content.primary
                            })
                            .child(if stats.link_status.is_empty() {
                                "🟢 流畅极佳"
                            } else {
                                stats.link_status
                            }),
                    ),
            )
    } else {
        div()
            .text_size(px(10.0))
            .font_family(crate::design_system::product_mono_font())
            .text_color(theme.content.tertiary)
            .child("Waiting for telemetry...")
    }
}

fn mesh_pairing_card(snapshot: MeshPairingSnapshot, cx: &mut Context<RestoredDashboard>) -> Div {
    let theme = cx.theme().clone();
    let message_tone = pairing_message_tone(snapshot.message_kind);
    let message_color = status_color(message_tone, cx);
    let restart_text = snapshot
        .restart_required
        .then_some("Restart RemotePlay to activate this group.");
    let copy_view = cx.weak_entity();
    let join_view = cx.weak_entity();
    let create_view = cx.weak_entity();

    let mut card = div()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .bg(theme.surface.raised)
        .border_1()
        .border_color(color_border_fine())
        .rounded(px(8.0))
        .shadow_xs()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.content.primary)
                                .child("RemotePlay Device Group"),
                        )
                        .child(
                            div()
                                .mt(px(2.0))
                                .text_size(px(10.0))
                                .text_color(theme.content.tertiary)
                                .truncate()
                                .child(format!(
                                    "{}  /  {}",
                                    snapshot.network_name,
                                    compact_device_id(&snapshot.device_id)
                                )),
                        ),
                )
                .child(
                    div()
                        .px_2()
                        .h(px(20.0))
                        .flex()
                        .items_center()
                        .rounded(px(4.0))
                        .bg(theme.status.info.bg)
                        .text_size(px(9.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.status.info.fg)
                        .child("PRIVATE"),
                ),
        )
        .child(
            div()
                .h(px(32.0))
                .flex()
                .items_center()
                .px_3()
                .bg(theme.surface.sunken)
                .border_1()
                .border_color(theme.border.muted)
                .rounded(px(5.0))
                .text_size(px(10.0))
                .text_color(theme.content.primary)
                .font_family(crate::design_system::product_mono_font())
                .truncate()
                .child(snapshot.invite_code.clone()),
        )
        .when_some(
            encode_pairing_qr(&snapshot.invite_code, DEFAULT_CONTROL_PORT)
                .ok()
                .and_then(|payload| qr_matrix_from_payload(&payload).ok()),
            |card, matrix| {
                card.child(
                    div()
                        .flex()
                        .justify_center()
                        .p_2()
                        .child(qr_matrix_view(&matrix)),
                )
            },
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    command_button("unified_copy_mesh_invite", ActionVariantKind::Primary, cx)
                        .h(px(28.0))
                        .flex_1()
                        .px_2()
                        .text_size(px(10.0))
                        .on_click(move |_event, _window, cx| {
                            let _ = copy_view.update(cx, |this, cx| {
                                this.copy_mesh_invite_code(cx);
                                cx.notify();
                            });
                        })
                        .child("Copy Code"),
                )
                .child(
                    command_button(
                        "unified_join_mesh_invite_clipboard",
                        ActionVariantKind::Neutral,
                        cx,
                    )
                    .h(px(28.0))
                    .flex_1()
                    .px_2()
                    .text_size(px(10.0))
                    .on_click(move |_event, _window, cx| {
                        let _ = join_view.update(cx, |this, cx| {
                            this.join_mesh_group_from_clipboard(cx);
                            cx.notify();
                        });
                    })
                    .child("Join from Clipboard"),
                ),
        )
        .child(
            div().flex().gap_2().child(
                command_button("unified_create_mesh_group", ActionVariantKind::Neutral, cx)
                    .h(px(28.0))
                    .flex_1()
                    .px_2()
                    .text_size(px(10.0))
                    .on_click(move |_event, _window, cx| {
                        let _ = create_view.update(cx, |this, cx| {
                            this.create_mesh_group();
                            cx.notify();
                        });
                    })
                    .child("Create New Group"),
            ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(10.0))
                .text_color(message_color)
                .child(status_dot(message_tone, px(5.0), cx))
                .child(snapshot.message),
        );

    if let Some(text) = restart_text {
        card = card.child(
            div()
                .text_size(px(10.0))
                .text_color(status_color(StatusTone::Warning, cx))
                .child(text),
        );
    }
    card
}

fn product_mark(cx: &mut Context<RestoredDashboard>) -> Div {
    let theme = cx.theme().clone();
    div()
        .size(px(22.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .bg(color_accent_cyan())
        .text_color(theme.action.primary.fg)
        .child(
            div()
                .size(px(10.0))
                .flex()
                .flex_col()
                .justify_between()
                .child(div().h(px(3.0)).w_full().rounded(px(1.0)).bg(rgb(0x001f28)))
                .child(
                    div()
                        .h(px(3.0))
                        .w(px(6.0))
                        .rounded(px(1.0))
                        .bg(rgb(0x001f28)),
                ),
        )
}

fn qr_matrix_view(matrix: &remote_core::QrMatrix) -> Div {
    let cell = px(3.0);
    let width = matrix.width;
    div()
        .flex()
        .flex_col()
        .p_2()
        .bg(rgb(0xffffff))
        .rounded(px(6.0))
        .children((0..width).map(|y| {
            div().flex().flex_row().children((0..width).map(move |x| {
                div().size(cell).bg(if matrix.is_dark(x, y) {
                    rgb(0x0d0f12)
                } else {
                    rgb(0xffffff)
                })
            }))
        }))
}

fn command_button<T: 'static>(
    id: impl Into<ElementId>,
    variant: ActionVariantKind,
    cx: &Context<T>,
) -> Button {
    let focus = cx.theme().border.focus;
    button(id)
        .variant(variant)
        .focusable()
        .focus_visible(move |style| style.border_2().border_color(focus))

}

fn empty_state(
    title: &'static str,
    detail: &'static str,
    cx: &mut Context<RestoredDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .p_4()
        .rounded(px(6.0))
        .bg(theme.surface.sunken)
        .border_1()
        .border_color(theme.border.muted)
        .child(
            icon(IconName::Info)
                .size(px(16.0))
                .color(theme.content.tertiary),
        )
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.content.secondary)
                .child(title),
        )
        .child(
            div()
                .text_size(px(10.0))
                .text_color(theme.content.tertiary)
                .child(detail),
        )
}

#[derive(Clone, Copy)]
enum StatusTone {
    Neutral,
    Success,
    Warning,
    Error,
    #[allow(dead_code)]
    Info,
}

fn status_color(tone: StatusTone, cx: &Context<RestoredDashboard>) -> Hsla {
    let theme = cx.theme();
    match tone {
        StatusTone::Neutral => theme.content.disabled,
        StatusTone::Success => theme.status.success.bg,
        StatusTone::Warning => theme.status.warning.bg,
        StatusTone::Error => theme.status.error.bg,
        StatusTone::Info => theme.status.info.bg,
    }
}

fn status_dot(tone: StatusTone, size: Pixels, cx: &Context<RestoredDashboard>) -> Div {
    div()
        .size(size)
        .flex_none()
        .rounded_full()
        .bg(status_color(tone, cx))
}

fn pairing_message_tone(kind: MeshPairingMessageKind) -> StatusTone {
    match kind {
        MeshPairingMessageKind::Neutral => StatusTone::Neutral,
        MeshPairingMessageKind::Success => StatusTone::Success,
        MeshPairingMessageKind::Warning => StatusTone::Warning,
        MeshPairingMessageKind::Error => StatusTone::Error,
    }
}

fn discovery_scope_label(scope: remote_core::discovery::DiscoveryScope) -> &'static str {
    match scope {
        remote_core::discovery::DiscoveryScope::Lan => "LAN",
        remote_core::discovery::DiscoveryScope::P2p => "P2P Direct",
        remote_core::discovery::DiscoveryScope::Mesh => "Legacy Route",
        remote_core::discovery::DiscoveryScope::Relay => "Relay",
    }
}

fn compact_device_id(device_id: &str) -> String {
    if device_id.len() <= 8 {
        device_id.to_string()
    } else {
        format!("{}...", &device_id[..8])
    }
}

#[derive(Debug, Default)]
struct PointerInputTracker {
    scroll_remainder: (f32, f32),
    pressed_buttons: BTreeSet<u8>,
    pressed_keys: BTreeSet<u16>,
    modifiers: u8,
}

impl PointerInputTracker {
    fn reset_pointer(&mut self) {
        self.scroll_remainder = (0.0, 0.0);
    }

    fn button_changed(&mut self, button: u8, pressed: bool) {
        if pressed {
            self.pressed_buttons.insert(button);
        } else {
            self.pressed_buttons.remove(&button);
        }
    }

    fn key_changed(&mut self, key_code: u16, pressed: bool) {
        if pressed {
            self.pressed_keys.insert(key_code);
        } else {
            self.pressed_keys.remove(&key_code);
        }
    }

    fn release_events(&mut self) -> Vec<protocol::InputEvent> {
        let mut events = Vec::with_capacity(
            self.pressed_buttons.len() + self.pressed_keys.len() + usize::from(self.modifiers != 0),
        );
        for button in std::mem::take(&mut self.pressed_buttons) {
            events.push(protocol::InputEvent::MouseUp(button));
        }
        for key_code in std::mem::take(&mut self.pressed_keys) {
            events.push(protocol::InputEvent::Key {
                key_code,
                pressed: false,
                modifiers: 0,
            });
        }
        if self.modifiers != 0 {
            events.push(protocol::InputEvent::ModifiersChanged(0));
            self.modifiers = 0;
        }
        self.reset_pointer();
        events
    }

    fn scrolled(&mut self, delta: (f32, f32)) -> Option<protocol::InputEvent> {
        if !delta.0.is_finite() || !delta.1.is_finite() {
            return None;
        }
        let accumulated = (
            delta.0 + self.scroll_remainder.0,
            delta.1 + self.scroll_remainder.1,
        );
        let quantized = quantize_delta(accumulated);
        self.scroll_remainder = (
            accumulated.0 - quantized.0 as f32,
            accumulated.1 - quantized.1 as f32,
        );
        if quantized == (0, 0) {
            None
        } else {
            Some(protocol::InputEvent::MouseScroll {
                delta_x: quantized.0,
                delta_y: quantized.1,
            })
        }
    }
}

fn quantize_delta(delta: (f32, f32)) -> (i32, i32) {
    (
        delta.0.clamp(i32::MIN as f32, i32::MAX as f32).trunc() as i32,
        delta.1.clamp(i32::MIN as f32, i32::MAX as f32).trunc() as i32,
    )
}

pub(crate) fn absolute_pointer_event(
    position: (f32, f32),
    surface: (f32, f32, f32, f32),
    frame: (u32, u32),
    scale_mode: ViewportScaleMode,
) -> Option<protocol::InputEvent> {
    let (surface_x, surface_y, surface_width, surface_height) = surface;
    let (frame_width, frame_height) = (frame.0 as f32, frame.1 as f32);
    if !position.0.is_finite()
        || !position.1.is_finite()
        || surface_width <= 0.0
        || surface_height <= 0.0
        || frame_width <= 0.0
        || frame_height <= 0.0
    {
        return None;
    }

    let scale = match scale_mode {
        ViewportScaleMode::AspectFit => {
            (surface_width / frame_width).min(surface_height / frame_height)
        }
        ViewportScaleMode::Fill => (surface_width / frame_width).max(surface_height / frame_height),
    };
    let displayed_width = frame_width * scale;
    let displayed_height = frame_height * scale;
    let displayed_x = surface_x + (surface_width - displayed_width) * 0.5;
    let displayed_y = surface_y + (surface_height - displayed_height) * 0.5;
    if scale_mode == ViewportScaleMode::AspectFit
        && (position.0 < displayed_x
            || position.1 < displayed_y
            || position.0 > displayed_x + displayed_width
            || position.1 > displayed_y + displayed_height)
    {
        return None;
    }

    let normalized_x = ((position.0 - displayed_x) / displayed_width).clamp(0.0, 1.0);
    let normalized_y = ((position.1 - displayed_y) / displayed_height).clamp(0.0, 1.0);
    Some(protocol::InputEvent::MouseMoveAbsolute {
        x: (normalized_x * f32::from(u16::MAX)).round() as u16,
        y: (normalized_y * f32::from(u16::MAX)).round() as u16,
    })
}

pub(crate) fn protocol_mouse_button(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Right => Some(1),
        MouseButton::Middle => Some(2),
        MouseButton::Navigate(_) => None,
    }
}

pub(crate) fn protocol_modifiers(modifiers: Modifiers) -> u8 {
    use protocol::input_modifiers;
    let mut flags = 0;
    if modifiers.shift {
        flags |= input_modifiers::SHIFT;
    }
    if modifiers.control {
        flags |= input_modifiers::CONTROL;
    }
    if modifiers.alt {
        flags |= input_modifiers::ALT;
    }
    if modifiers.platform {
        flags |= input_modifiers::META;
    }
    if modifiers.function {
        flags |= input_modifiers::FUNCTION;
    }
    flags
}

pub(crate) fn protocol_key_event(
    keystroke: &Keystroke,
    pressed: bool,
) -> Option<protocol::InputEvent> {
    let key_code = macos_key_code(keystroke)?;
    let mut modifiers = protocol_modifiers(keystroke.modifiers);
    if is_shifted_macos_symbol(&keystroke.key) {
        modifiers |= protocol::input_modifiers::SHIFT;
    }
    Some(protocol::InputEvent::Key {
        key_code,
        pressed,
        modifiers,
    })
}

fn macos_key_code(keystroke: &Keystroke) -> Option<u16> {
    Some(match keystroke.key.as_str() {
        "a" => 0x00,
        "s" => 0x01,
        "d" => 0x02,
        "f" => 0x03,
        "h" => 0x04,
        "g" => 0x05,
        "z" => 0x06,
        "x" => 0x07,
        "c" => 0x08,
        "v" => 0x09,
        "b" => 0x0b,
        "q" => 0x0c,
        "w" => 0x0d,
        "e" => 0x0e,
        "r" => 0x0f,
        "y" => 0x10,
        "t" => 0x11,
        "1" | "!" => 0x12,
        "2" | "@" => 0x13,
        "3" | "#" => 0x14,
        "4" | "$" => 0x15,
        "6" | "^" => 0x16,
        "5" | "%" => 0x17,
        "=" | "+" => 0x18,
        "9" | "(" => 0x19,
        "7" | "&" => 0x1a,
        "-" | "_" => 0x1b,
        "8" | "*" => 0x1c,
        "0" | ")" => 0x1d,
        "]" | "}" => 0x1e,
        "o" => 0x1f,
        "u" => 0x20,
        "[" | "{" => 0x21,
        "i" => 0x22,
        "p" => 0x23,
        "enter" => 0x24,
        "l" => 0x25,
        "j" => 0x26,
        "'" | "\"" => 0x27,
        "k" => 0x28,
        ";" | ":" => 0x29,
        "\\" | "|" => 0x2a,
        "," | "<" => 0x2b,
        "/" | "?" => 0x2c,
        "n" => 0x2d,
        "m" => 0x2e,
        "." | ">" => 0x2f,
        "tab" => 0x30,
        "space" => 0x31,
        "`" | "~" => 0x32,
        "backspace" => 0x33,
        "escape" => 0x35,
        "f1" => 0x7a,
        "f2" => 0x78,
        "f3" => 0x63,
        "f4" => 0x76,
        "f5" => 0x60,
        "f6" => 0x61,
        "f7" => 0x62,
        "f8" => 0x64,
        "f9" => 0x65,
        "f10" => 0x6d,
        "f11" => 0x67,
        "f12" => 0x6f,
        "insert" => 0x72,
        "home" => 0x73,
        "pageup" => 0x74,
        "delete" => 0x75,
        "end" => 0x77,
        "pagedown" => 0x79,
        "left" => 0x7b,
        "right" => 0x7c,
        "down" => 0x7d,
        "up" => 0x7e,
        _ => return None,
    })
}

fn is_shifted_macos_symbol(key: &str) -> bool {
    matches!(
        key,
        "!" | "@"
            | "#"
            | "$"
            | "%"
            | "^"
            | "&"
            | "*"
            | "("
            | ")"
            | "_"
            | "+"
            | "{"
            | "}"
            | "|"
            | ":"
            | "\""
            | "<"
            | ">"
            | "?"
            | "~"
    )
}

struct DashboardSnapshot {
    role: RoleState,
    devices: Vec<AppDevice>,
    capture_sources: Arc<Vec<protocol::session::CaptureSourceInfo>>,
    source_binding: Option<SourceViewBinding>,
    active_capture_source: protocol::session::CaptureSource,
    pending_capture_source: Option<protocol::session::CaptureSource>,
    active_capture_supports_input: bool,
    capture_source_error: Option<String>,
}

#[cfg(test)]
mod input_tests {
    use super::{
        ControlIslandVisibility, PointerInputTracker, TOOLBAR_AUTO_HIDE_DELAY,
        TOOLBAR_TRIGGER_ZONE_HEIGHT_PX, ViewportScaleMode, absolute_pointer_event,
        control_island_visibility, dashboard_refresh_interval, display_capture_sources,
        host_stats_available, is_shifted_macos_symbol, macos_key_code, product_window_appearance,
        protocol_key_event, protocol_modifiers, protocol_mouse_button, should_auto_hide_toolbar,
        should_reveal_toolbar,
    };
    use crate::HostStats;
    use gpui::{
        AppContext, Context, FocusHandle, InteractiveElement, IntoElement, Keystroke, Modifiers,
        MouseButton, NavigationDirection, ParentElement, Render, Styled, TestAppContext, Window,
        WindowAppearance, canvas, div, point, px, size,
    };
    use protocol::InputEvent;
    use remote_core::role::RoleKind;
    use std::time::{Duration, Instant};

    #[test]
    fn product_appearance_matches_the_dark_video_and_glass_surfaces() {
        assert_eq!(product_window_appearance(), WindowAppearance::Dark);
    }

    #[test]
    fn dashboard_only_uses_frame_rate_refresh_while_viewing() {
        assert_eq!(
            dashboard_refresh_interval(RoleKind::Viewing),
            Duration::from_millis(16)
        );
        for role in [RoleKind::Idle, RoleKind::Connecting, RoleKind::Serving] {
            assert_eq!(dashboard_refresh_interval(role), Duration::from_millis(100));
        }
    }

    #[test]
    fn control_island_only_exposes_controls_relevant_to_the_current_role() {
        assert_eq!(
            control_island_visibility(RoleKind::Idle),
            ControlIslandVisibility {
                show_viewer_controls: false,
                show_viewer_telemetry: false,
                show_disconnect: false,
            }
        );
        assert_eq!(
            control_island_visibility(RoleKind::Connecting),
            ControlIslandVisibility {
                show_viewer_controls: false,
                show_viewer_telemetry: true,
                show_disconnect: true,
            }
        );
        assert_eq!(
            control_island_visibility(RoleKind::Viewing),
            ControlIslandVisibility {
                show_viewer_controls: true,
                show_viewer_telemetry: true,
                show_disconnect: true,
            }
        );
        assert_eq!(
            control_island_visibility(RoleKind::Serving),
            ControlIslandVisibility {
                show_viewer_controls: false,
                show_viewer_telemetry: false,
                show_disconnect: true,
            }
        );
    }

    #[test]
    fn auto_hide_toolbar_sensors_and_delay_rules() {
        // 顶部感应区：<= 16px 触发呈现，> 16px 不触发
        assert!(should_reveal_toolbar(0.0));
        assert!(!should_reveal_toolbar(-1.0));
        assert!(!should_reveal_toolbar(f32::NAN));
        assert!(!should_reveal_toolbar(f32::INFINITY));
        assert!(should_reveal_toolbar(8.0));
        assert!(should_reveal_toolbar(TOOLBAR_TRIGGER_ZONE_HEIGHT_PX));
        assert!(!should_reveal_toolbar(TOOLBAR_TRIGGER_ZONE_HEIGHT_PX + 0.5));
        assert!(!should_reveal_toolbar(100.0));

        let now = Instant::now();
        // 鼠标悬停在控制条上时，绝不自动隐藏
        assert!(!should_auto_hide_toolbar(
            true,
            false,
            now - Duration::from_secs(5),
            now,
            TOOLBAR_AUTO_HIDE_DELAY
        ));

        // 菜单展开时，绝不自动隐藏
        assert!(!should_auto_hide_toolbar(
            false,
            true,
            now - Duration::from_secs(5),
            now,
            TOOLBAR_AUTO_HIDE_DELAY
        ));

        // 鼠标移开但未达到延迟阈值，保持呈现
        assert!(!should_auto_hide_toolbar(
            false,
            false,
            now - Duration::from_millis(500),
            now,
            TOOLBAR_AUTO_HIDE_DELAY
        ));

        // 鼠标移开且超过延时阈值（1800ms），触发自隐藏
        assert!(should_auto_hide_toolbar(
            false,
            false,
            now - TOOLBAR_AUTO_HIDE_DELAY,
            now,
            TOOLBAR_AUTO_HIDE_DELAY
        ));
        assert!(should_auto_hide_toolbar(
            false,
            false,
            now - Duration::from_secs(3),
            now,
            TOOLBAR_AUTO_HIDE_DELAY
        ));
    }

    #[test]
    fn display_switcher_uses_real_sources_and_never_invents_display_two() {
        let one = vec![protocol::session::CaptureSourceInfo {
            source: protocol::session::CaptureSource::MainDisplay,
            title: "Physical display".into(),
            application: String::new(),
            process_id: None,
            width: 1920,
            height: 1080,
            supports_input: true,
        }];
        let displays = display_capture_sources(&one);
        assert_eq!(displays.len(), 1);
        assert_eq!(
            displays[0].source,
            protocol::session::CaptureSource::MainDisplay
        );

        let real_secondary = protocol::session::CaptureSourceInfo {
            source: protocol::session::CaptureSource::Display(77),
            title: "External".into(),
            application: String::new(),
            process_id: None,
            width: 2560,
            height: 1440,
            supports_input: false,
        };
        let displays = display_capture_sources(&[one[0].clone(), real_secondary.clone()]);
        assert_eq!(displays.len(), 2);
        assert_eq!(displays[1].source, real_secondary.source);
    }

    #[test]
    fn telemetry_is_only_available_while_recent_and_finite() {
        let mut stats = HostStats {
            fps: 60.0,
            latency: 4.0,
            jitter: 1.0,
            bitrate_kbps: 8_000,
            rtt_ms: 0.0,
            e2e_latency_ms: 0.0,
            decode_latency_ms: 0.0,
            updated_at: Some(Instant::now()),
            ..Default::default()
        };
        assert!(host_stats_available(&stats));

        stats.updated_at = Instant::now().checked_sub(Duration::from_secs(6));
        assert!(!host_stats_available(&stats));

        stats.updated_at = Some(Instant::now());
        stats.fps = f32::NAN;
        assert!(!host_stats_available(&stats));
    }

    #[test]
    fn absolute_pointer_mapping_uses_the_contained_video_rect() {
        let event = absolute_pointer_event(
            (500.0, 350.0),
            (100.0, 50.0, 800.0, 600.0),
            (1920, 1080),
            ViewportScaleMode::AspectFit,
        );
        assert!(matches!(
            event,
            Some(InputEvent::MouseMoveAbsolute { x, y })
                if (32_767..=32_768).contains(&x) && (32_767..=32_768).contains(&y)
        ));

        assert_eq!(
            absolute_pointer_event(
                (500.0, 75.0),
                (100.0, 50.0, 800.0, 600.0),
                (1920, 1080),
                ViewportScaleMode::AspectFit,
            ),
            None,
            "letterbox input must not move the remote pointer",
        );
    }

    #[test]
    fn absolute_pointer_mapping_accounts_for_cover_cropping() {
        let event = absolute_pointer_event(
            (100.0, 350.0),
            (100.0, 50.0, 800.0, 600.0),
            (1920, 1080),
            ViewportScaleMode::Fill,
        );
        assert!(matches!(
            event,
            Some(InputEvent::MouseMoveAbsolute { x, y })
                if (8_190..=8_193).contains(&x) && (32_767..=32_768).contains(&y)
        ));
    }

    #[gpui::test]
    fn input_surface_probe_records_window_coordinates(cx: &mut TestAppContext) {
        let recorded = std::sync::Arc::new(std::sync::Mutex::new(None));
        let recorded_for_probe = recorded.clone();
        let cx = cx.add_empty_window();

        cx.draw(
            point(px(120.0), px(80.0)),
            size(px(640.0), px(480.0)),
            move |_window, _cx| {
                div().relative().w(px(320.0)).h(px(180.0)).child(
                    canvas(
                        move |bounds, _window, _cx| {
                            *recorded_for_probe.lock().expect("probe bounds lock") = Some(bounds);
                        },
                        |_bounds, (), _window, _cx| {},
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .w_full()
                    .h_full(),
                )
            },
        );

        let bounds = recorded
            .lock()
            .expect("recorded bounds lock")
            .expect("probe should record bounds");
        assert_eq!(bounds.origin, point(px(120.0), px(80.0)));
        assert_eq!(bounds.size, size(px(320.0), px(180.0)));
    }

    #[gpui::test]
    fn focused_video_descendant_reaches_root_capture_handler(cx: &mut TestAppContext) {
        struct KeyboardCaptureView {
            focus: FocusHandle,
            captured: bool,
        }

        impl Render for KeyboardCaptureView {
            fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .capture_key_down(cx.listener(|this, _event, _window, cx| {
                        this.captured = true;
                        cx.stop_propagation();
                    }))
                    .child(div().track_focus(&self.focus))
            }
        }

        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_window, cx| {
                cx.new(|cx| KeyboardCaptureView {
                    focus: cx.focus_handle(),
                    captured: false,
                })
            })
            .expect("test window")
        });
        window
            .update(cx, |view, window, _cx| window.focus(&view.focus))
            .expect("focus video descendant");

        cx.dispatch_keystroke(*window, Keystroke::parse("a").expect("A keystroke"));

        window
            .update(cx, |view, _window, _cx| assert!(view.captured))
            .expect("read capture result");
    }

    #[test]
    fn precise_scroll_accumulates_subpixel_deltas() {
        let mut tracker = PointerInputTracker::default();
        assert_eq!(tracker.scrolled((0.4, -0.4)), None);
        assert_eq!(tracker.scrolled((0.4, -0.4)), None);
        assert!(matches!(
            tracker.scrolled((0.4, -0.4)),
            Some(InputEvent::MouseScroll {
                delta_x: 1,
                delta_y: -1
            })
        ));
    }

    #[test]
    fn locking_input_releases_tracked_buttons_keys_and_modifiers() {
        let mut tracker = PointerInputTracker::default();
        tracker.button_changed(0, true);
        tracker.key_changed(0x00, true);
        tracker.modifiers = protocol::input_modifiers::SHIFT;

        assert_eq!(
            tracker.release_events(),
            vec![
                InputEvent::MouseUp(0),
                InputEvent::Key {
                    key_code: 0x00,
                    pressed: false,
                    modifiers: 0,
                },
                InputEvent::ModifiersChanged(0),
            ]
        );
        assert!(tracker.release_events().is_empty());
    }

    #[test]
    fn gpui_mouse_buttons_use_the_platform_neutral_protocol_ids() {
        assert_eq!(protocol_mouse_button(MouseButton::Left), Some(0));
        assert_eq!(protocol_mouse_button(MouseButton::Right), Some(1));
        assert_eq!(protocol_mouse_button(MouseButton::Middle), Some(2));
        assert_eq!(
            protocol_mouse_button(MouseButton::Navigate(NavigationDirection::Back)),
            None
        );
    }

    #[test]
    fn gpui_keys_map_to_macos_virtual_key_codes() {
        let key = |name: &str| Keystroke {
            key: name.to_string(),
            ..Default::default()
        };

        assert_eq!(macos_key_code(&key("a")), Some(0x00));
        assert_eq!(macos_key_code(&key("enter")), Some(0x24));
        assert_eq!(macos_key_code(&key("left")), Some(0x7b));
        assert_eq!(macos_key_code(&key("f12")), Some(0x6f));
        assert_eq!(macos_key_code(&key("!")), Some(0x12));
        assert!(is_shifted_macos_symbol("!"));
        assert!(!is_shifted_macos_symbol("1"));
        assert_eq!(macos_key_code(&key("unsupported-key")), None);

        assert_eq!(
            protocol_key_event(&key("a"), true),
            Some(InputEvent::Key {
                key_code: 0x00,
                pressed: true,
                modifiers: 0,
            })
        );
        assert_eq!(
            protocol_key_event(&key("a"), false),
            Some(InputEvent::Key {
                key_code: 0x00,
                pressed: false,
                modifiers: 0,
            })
        );
    }

    #[test]
    fn gpui_modifiers_use_protocol_bit_flags() {
        let modifiers = Modifiers {
            shift: true,
            control: true,
            alt: true,
            platform: true,
            function: false,
        };
        assert_eq!(
            protocol_modifiers(modifiers),
            protocol::input_modifiers::SHIFT
                | protocol::input_modifiers::CONTROL
                | protocol::input_modifiers::ALT
                | protocol::input_modifiers::META,
        );
    }
}

#[cfg(not(any(
    target_os = "macos",
    all(target_os = "windows", feature = "native-windows-video"),
    all(target_os = "linux", feature = "native-linux-video")
)))]
fn decoded_video_frame_surface_with_fit(
    _frame: &MacDecodedVideoFrame,
    _fit: ObjectFit,
) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child("Native video adapter pending. This development preview is not the released viewer.")
        .into_any_element()
}

// Explicit local acceptance only. These are the application's own atomic lifecycle
// markers, not external process sampling, memory inspection, or user input.
static OWN_GUI_TRACE_ENABLED: AtomicBool = AtomicBool::new(false);
static OWN_GUI_STAGE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static OWN_GUI_STEPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn owned_gui_stage(stage: u8) {
    if OWN_GUI_TRACE_ENABLED.load(Ordering::Relaxed) {
        OWN_GUI_STAGE.store(stage, Ordering::Relaxed);
        OWN_GUI_STEPS.fetch_add(1, Ordering::Relaxed);
    }
}
fn start_owned_gui_trace() {
    let Ok(path) = std::env::var("REMOTE_PLAY_RESTORED_TEST_OUTPUT") else {
        return;
    };
    if OWN_GUI_TRACE_ENABLED.swap(true, Ordering::Relaxed) {
        return;
    }
    let path = std::path::PathBuf::from(path).with_extension("progress.json");
    std::thread::spawn(move || {
        let labels = [
            "initializing",
            "poll-start",
            "poll-done",
            "activity-sync",
            "waiting-update",
            "render-start",
            "frame-drained",
            "controls-start",
            "controls-done",
            "unused",
            "paint-ack-start",
            "paint-ack-done",
            "receipt-start",
            "close-start",
            "close-done",
        ];
        for second in 0..185u32 {
            let stage = OWN_GUI_STAGE.load(Ordering::Relaxed) as usize;
            let report = serde_json::json!({"elapsed_seconds":second,"stage":labels.get(stage).unwrap_or(&"unknown"),"steps":OWN_GUI_STEPS.load(Ordering::Relaxed),"self_report_only":true});
            let _ = std::fs::write(&path, serde_json::to_vec(&report).unwrap());
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux", target_os = "windows")))]
#[path = "restored_input_tests.rs"]
mod restored_input_tests;

#[cfg(all(target_os = "windows", feature = "native-windows-video"))]
fn decoded_video_frame_surface_with_fit(
    frame: &MacDecodedVideoFrame,
    fit: ObjectFit,
) -> AnyElement {
    if let Some(native) = &frame.native {
        surface(native.ready.frame.clone())
            .object_fit(fit)
            .into_any_element()
    } else {
        div()
            .child("Waiting for a native decoder frame; no CPU presentation fallback")
            .into_any_element()
    }
}

#[cfg(all(target_os="linux",feature="native-linux-video"))]
fn decoded_video_frame_surface_with_fit(frame:&MacDecodedVideoFrame,fit:ObjectFit)->AnyElement {
    match &frame.native_linux {
        Some(native)=>gpui::surface(native.frame.clone()).object_fit(fit).size_full().into_any_element(),
        None=>div().size_full().flex().items_center().justify_center().child("Native video is unavailable; see the decoder status.").into_any_element(),
    }
}

// Keep capability, display acknowledgement, and input geometry aligned. No
// unsupported platform or fallback placeholder can acknowledge a displayed frame.
const fn native_surface_enabled(macos:bool,windows:bool,linux:bool,windows_native:bool,linux_native:bool)->bool {
    macos || (windows&&windows_native) || (linux&&linux_native)
}
const fn native_presentation_available()->bool {
    native_surface_enabled(cfg!(target_os="macos"),cfg!(target_os="windows"),cfg!(target_os="linux"),cfg!(feature="native-windows-video"),cfg!(feature="native-linux-video"))
}
#[cfg(feature="gpui-native-video")]
fn native_frame_geometry(frame:&MacDecodedVideoFrame)->Option<gpui::native_video::VideoGeometry> {
    #[cfg(all(target_os="windows",feature="native-windows-video"))]
    { return frame.native.as_ref().map(|n|n.ready.frame.geometry()); }
    #[cfg(all(target_os="linux",feature="native-linux-video"))]
    { return frame.native_linux.as_ref().map(|n|n.frame.geometry()); }
    #[cfg(not(any(all(target_os="windows",feature="native-windows-video"),all(target_os="linux",feature="native-linux-video"))))]
    {let _=frame;None}
}
#[cfg(feature="gpui-native-video")]
fn native_pointer_event(geometry:gpui::native_video::VideoGeometry,viewport:gpui::native_video::VideoRect,fit:gpui::native_video::VideoFit,point:[f32;2])->Option<protocol::InputEvent> {
    let [x,y]=geometry.map_input(viewport,fit,point)?;
    Some(protocol::InputEvent::MouseMoveAbsolute{x:(x*f32::from(u16::MAX)).round() as u16,y:(y*f32::from(u16::MAX)).round() as u16})
}
#[cfg(test)] mod native_surface_input_tests {
    use super::native_surface_enabled;
    #[cfg(feature="gpui-native-video")]
    use super::native_pointer_event;
    #[test] fn all_supported_native_backends_have_a_paint_acknowledgement_path() {
        assert!(native_surface_enabled(true,false,false,false,false));
        assert!(native_surface_enabled(false,true,false,true,false));
        assert!(native_surface_enabled(false,false,true,false,true));
        assert!(!native_surface_enabled(false,true,false,false,true));
        assert!(!native_surface_enabled(false,false,true,true,false));
        assert!(!native_surface_enabled(false,false,false,true,true));
    }
    #[cfg(feature="gpui-native-video")]
    #[test] fn linux_and_windows_use_the_same_painted_geometry_for_pointer_input() {
        use gpui::native_video::*;
        let viewport=VideoRect{x:-150.,y:20.,width:1000.,height:1000.};
        let geometry=VideoGeometry{coded:[2048,1152],visible:[32,16,1920,1080],pixel_aspect:[1,1],rotation:Rotation::R90};
        assert!(native_pointer_event(geometry,viewport,VideoFit::Contain,[-140.,30.]).is_none());
        assert!(matches!(native_pointer_event(geometry,viewport,VideoFit::Contain,[350.,520.]),Some(protocol::InputEvent::MouseMoveAbsolute{x:32768,y:32768})));
        let view=VideoRect{x:-225.,y:30.,width:1500.,height:1500.};
        assert_eq!(native_pointer_event(geometry,viewport,VideoFit::Cover,[450.,420.]),native_pointer_event(geometry,view,VideoFit::Cover,[675.,630.]));
    }
}
