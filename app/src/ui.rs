use crate::design_system::{
    color_accent_amber, color_accent_cyan, color_accent_emerald, color_accent_purple,
    color_border_fine, color_glass_card, remote_play_themes,
};
use crate::mesh_admin::{mesh_setup_was_cancelled, run_mesh_admin_setup};
use crate::{
    AppDevice, HostStats, MacDecodedVideoFrame, MeshPairingControl, MeshPairingMessageKind,
    MeshPairingSnapshot, RoleState, StreamStartOptions, UnifiedRuntimeConfig, UnifiedRuntimeHandle,
    UnifiedViewerMediaStatus, decoded_video_frame_surface_with_fit,
    start_unified_runtime,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use remote_core::{
    VideoFrame,
    mesh::{EasyTierHealthIssue, EasyTierHealthSnapshot, EasyTierHealthState},
    role::RoleKind,
};
use std::collections::BTreeSet;
use std::error::Error;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use yororen_ui::{
    assets::UiAsset,
    component::{self, Button, IconName, button, icon, tooltip},
    theme::{ActionVariantKind, ActiveTheme, GlobalTheme, Theme},
};

pub async fn run_unified_gui(
    config: UnifiedRuntimeConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let runtime = start_unified_runtime(config).await?;
    remote_core::stats::Statistics::start_reporter(runtime.stats.clone(), "Unified", 1);

    let app = Application::new().with_assets(UiAsset);
    app.run(move |cx: &mut App| {
        apply_product_window_appearance();
        let appearance = product_window_appearance();
        component::init(cx);
        cx.set_global(GlobalTheme::new_with_themes(
            appearance,
            remote_play_themes(),
        ));
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds(
                point(px(80.0), px(60.0)),
                size(px(1280.0), px(800.0)),
            ))),
            window_min_size: Some(size(px(960.0), px(640.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("RemotePlay".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let mut runtime = Some(runtime);
        cx.open_window(window_options, move |_, cx| {
            let view = cx.new(|cx| {
                UnifiedDashboard::new(
                    runtime
                        .take()
                        .expect("runtime should be moved into window once"),
                    cx,
                )
            });

            cx.spawn({
                let view = view.clone();
                async move |cx| {
                    loop {
                        let Ok(refresh_interval) = view.update(&mut *cx, |view, cx| {
                            cx.notify();
                            view.refresh_interval()
                        }) else {
                            break;
                        };
                        Timer::after(refresh_interval).await;
                    }
                    let _ = cx.update(|cx| {
                        cx.quit();
                    });
                }
            })
            .detach();

            view
        })
        .expect("failed to open RemotePlay window");
    });

    Ok(())
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
    Mesh,
    Security,
    Network,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceFilterKind {
    All,
    Lan,
    Mesh,
    Relay,
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

struct UnifiedDashboard {
    runtime: UnifiedRuntimeHandle,
    app_runtime: Arc<Mutex<crate::UnifiedAppRuntime>>,
    viewer_frame: Option<Arc<Mutex<Option<MacDecodedVideoFrame>>>>,
    current_frame: Option<Arc<MacDecodedVideoFrame>>,
    presentation_frame: Arc<Mutex<Option<Arc<MacDecodedVideoFrame>>>>,
    host_stats: Option<Arc<RwLock<HostStats>>>,
    mesh_health: Option<watch::Receiver<EasyTierHealthSnapshot>>,
    viewer_media_status: UnifiedViewerMediaStatus,
    mesh_pairing: Option<MeshPairingControl>,
    mesh_pairing_snapshot: Option<MeshPairingSnapshot>,
    pointer_input: PointerInputTracker,
    input_focus: FocusHandle,
    video_surface_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    input_session_id: Option<u32>,
    status: String,

    drawer_open: bool,
    active_tab: DrawerTab,
    device_filter: DeviceFilterKind,
    scale_mode: ViewportScaleMode,
    telemetry_hud_collapsed: bool,
    input_locked: bool,
    selected_resolution: (u32, u32),
    selected_fps: u32,
    selected_bitrate_kbps: u32,
}

impl UnifiedDashboard {
    fn new(runtime: UnifiedRuntimeHandle, cx: &mut Context<Self>) -> Self {
        let app_runtime = runtime.owner.runtime();
        let viewer_frame = runtime.viewer_frame.clone();
        let host_stats = runtime.host_stats.clone();
        let mesh_health = runtime.owner.mesh_health_rx();
        let viewer_media_status = runtime.viewer_media_status.clone();
        let mesh_pairing = runtime.mesh_pairing.clone();
        let mesh_pairing_snapshot = mesh_pairing.as_ref().map(MeshPairingControl::snapshot);

        let prefs = crate::preferences::UserPreferences::load_or_default();
        if runtime.owner.supports_clipboard_sync() {
            runtime.owner.set_clipboard_sync_enabled(prefs.side_services.clipboard_sync);
        }
        if runtime.owner.supports_file_transfer() {
            runtime.owner.set_file_transfer_enabled(prefs.side_services.file_transfer);
        }
        if runtime.owner.supports_talkback() {
            runtime.owner.set_talkback_enabled(prefs.side_services.talkback);
        }
        let scale_mode = match prefs.ui.scale_mode.as_str() {
            "fill" => ViewportScaleMode::Fill,
            _ => ViewportScaleMode::AspectFit,
        };
        let selected_resolution = (prefs.stream.width, prefs.stream.height);
        let selected_fps = prefs.stream.fps;
        let selected_bitrate_kbps = prefs.stream.bitrate_kbps;
        let telemetry_hud_collapsed = prefs.ui.telemetry_hud_collapsed;

        Self {
            runtime,
            app_runtime,
            viewer_frame,
            current_frame: None,
            presentation_frame: Arc::new(Mutex::new(None)),
            host_stats,
            mesh_health,
            viewer_media_status,
            mesh_pairing,
            mesh_pairing_snapshot,
            pointer_input: PointerInputTracker::default(),
            input_focus: cx.focus_handle(),
            video_surface_bounds: Arc::new(Mutex::new(None)),
            input_session_id: None,
            status: "Ready".to_string(),

            drawer_open: true,
            active_tab: DrawerTab::Devices,
            device_filter: DeviceFilterKind::All,
            scale_mode,
            telemetry_hud_collapsed,
            input_locked: false,
            selected_resolution,
            selected_fps,
            selected_bitrate_kbps,
        }
    }

    fn persist_preferences(&self) {
        let mut prefs = crate::preferences::UserPreferences::load_or_default();
        prefs.stream.width = self.selected_resolution.0;
        prefs.stream.height = self.selected_resolution.1;
        prefs.stream.fps = self.selected_fps;
        prefs.stream.bitrate_kbps = self.selected_bitrate_kbps;
        prefs.ui.scale_mode = match self.scale_mode {
            ViewportScaleMode::AspectFit => "aspect_fit".to_string(),
            ViewportScaleMode::Fill => "fill".to_string(),
        };
        prefs.ui.telemetry_hud_collapsed = self.telemetry_hud_collapsed;
        prefs.side_services.talkback = self.runtime.owner.talkback_enabled();
        prefs.side_services.clipboard_sync = self.runtime.owner.clipboard_sync_enabled();
        prefs.side_services.file_transfer = self.runtime.owner.file_transfer_enabled();
        if let Err(err) = prefs.save() {
            eprintln!("Failed to persist user preferences: {err}");
        }
    }

    fn snapshot(&self) -> DashboardSnapshot {
        let runtime = self.app_runtime.lock().expect("unified runtime lock");
        DashboardSnapshot {
            role: runtime.role_state().clone(),
            devices: runtime.devices(),
        }
    }

    fn mesh_snapshot(&self) -> Option<EasyTierHealthSnapshot> {
        self.mesh_health.as_ref().map(|rx| rx.borrow().clone())
    }

    fn drain_latest_frame(&mut self) {
        let Some(viewer_frame) = &self.viewer_frame else {
            return;
        };
        if let Some(frame) = viewer_frame.lock().expect("viewer frame lock").take() {
            if let Some(stats) = &self.host_stats
                && let Ok(mut stats) = stats.write()
                && frame.decode_cost_ms > 0.0
            {
                if stats.decode_latency_ms <= 0.01 {
                    stats.decode_latency_ms = frame.decode_cost_ms;
                } else {
                    stats.decode_latency_ms =
                        stats.decode_latency_ms * 0.9 + frame.decode_cost_ms * 0.1;
                }
            }
            let frame = Arc::new(frame);
            *self
                .presentation_frame
                .lock()
                .expect("presentation frame lock") = Some(frame.clone());
            self.current_frame = Some(frame);
        }
    }

    fn host_stats_snapshot(&self, role: &RoleState) -> Option<HostStats> {
        if !matches!(role, RoleState::Viewing(_)) {
            return None;
        }
        self.host_stats
            .as_ref()
            .and_then(|stats| stats.read().ok().map(|stats| stats.clone()))
            .filter(host_stats_available)
    }

    fn reset_host_stats(&self) {
        if let Some(stats) = &self.host_stats
            && let Ok(mut stats) = stats.write()
        {
            *stats = HostStats::default();
        }
    }

    fn refresh_interval(&self) -> Duration {
        let runtime = self.app_runtime.lock().expect("unified runtime lock");
        dashboard_refresh_interval(runtime.role_state().kind())
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
        if let Some(control) = &self.mesh_pairing {
            self.mesh_pairing_snapshot = Some(control.create_new_group());
        }
    }

    fn join_mesh_group_from_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(control) = &self.mesh_pairing else {
            return;
        };
        let invite_code = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        self.mesh_pairing_snapshot = Some(control.join_from_invite_code(&invite_code));
    }

    fn install_mesh_admin_setup(&mut self, cx: &mut Context<Self>) {
        self.status = "Opening Mesh setup".to_string();
        let background = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = background
                .spawn(async move { run_mesh_admin_setup() })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                match result {
                    Ok(()) => {
                        this.status = "Mesh setup installed".to_string();
                        if let Some(control) = &this.mesh_pairing {
                            this.mesh_pairing_snapshot =
                                Some(control.request_runtime_reload("Mesh setup installed."));
                        }
                    }
                    Err(err) => {
                        this.status = if mesh_setup_was_cancelled(&err) {
                            "Mesh setup cancelled".to_string()
                        } else {
                            format!("Mesh setup failed: {}", setup_error_summary(&err))
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
                        Timer::after(Duration::from_millis(16)).await;
                        if view.update(&mut *cx, |_view, cx| cx.notify()).is_err() {
                            break;
                        }
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

    fn sync_input_session(&mut self, role: &RoleState) {
        let input_session_id = match role {
            RoleState::Viewing(session) => Some(session.session_id),
            RoleState::Idle | RoleState::Connecting(_) | RoleState::Serving(_) => None,
        };
        if self.input_session_id != input_session_id {
            if self.input_session_id.is_none() && input_session_id.is_some() {
                self.drawer_open = false;
                self.status = "Connected".to_string();
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
        if self.input_locked {
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
            self.runtime.owner.queue_viewing_input(event);
        }
    }

    fn queue_pointer_button(&mut self, button: MouseButton, pressed: bool) {
        if self.input_locked {
            return;
        }
        let Some(button) = protocol_mouse_button(button) else {
            return;
        };
        let event = if pressed {
            protocol::InputEvent::MouseDown(button)
        } else {
            protocol::InputEvent::MouseUp(button)
        };
        if self.runtime.owner.queue_viewing_input(event) {
            self.pointer_input.button_changed(button, pressed);
        }
    }

    fn queue_pointer_scroll(&mut self, delta: ScrollDelta) {
        if self.input_locked {
            return;
        }
        let delta = delta.pixel_delta(px(40.0));
        if let Some(event) = self
            .pointer_input
            .scrolled((f32::from(delta.x), f32::from(delta.y)))
        {
            self.runtime.owner.queue_viewing_input(event);
        }
    }

    fn queue_key(&mut self, keystroke: &Keystroke, pressed: bool) {
        if self.input_locked {
            return;
        }
        let Some(event) = protocol_key_event(keystroke, pressed) else {
            return;
        };
        let protocol::InputEvent::Key { key_code, .. } = &event else {
            return;
        };
        let key_code = *key_code;
        if self.runtime.owner.queue_viewing_input(event) {
            self.pointer_input.key_changed(key_code, pressed);
        }
    }

    fn queue_modifiers(&mut self, event: &ModifiersChangedEvent) {
        if self.input_locked {
            return;
        }
        let mut modifiers = protocol_modifiers(event.modifiers);
        if event.capslock.on {
            modifiers |= protocol::input_modifiers::CAPS_LOCK;
        }
        if self
            .runtime
            .owner
            .queue_viewing_input(protocol::InputEvent::ModifiersChanged(modifiers))
        {
            self.pointer_input.modifiers = modifiers;
        }
    }

    fn set_input_locked(&mut self, locked: bool) {
        if locked && !self.input_locked {
            for event in self.pointer_input.release_events() {
                self.runtime.owner.queue_viewing_input(event);
            }
        }
        self.input_locked = locked;
    }
}

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

fn stream_status_capsule_card(
    title: String,
    is_live: bool,
    compact_stats: String,
    compact: bool,
    theme: &Theme,
    action_slot: Option<AnyElement>,
) -> Div {
    div()
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
                        .bg(if is_live {
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
        .child(
            div()
                .w(px(1.0))
                .h(px(16.0))
                .bg(theme.border.divider),
        )
        .child(
            div()
                .text_size(px(10.0))
                .font_family("monospace")
                .text_color(theme.content.secondary)
                .whitespace_nowrap()
                .child(compact_stats),
        )
        .when_some(action_slot, |this, slot| this.child(slot))
}

struct PopoutStreamView {
    presentation_frame: Arc<Mutex<Option<Arc<MacDecodedVideoFrame>>>>,
    host_stats: Option<Arc<RwLock<HostStats>>>,
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
            .and_then(|stats| stats.read().ok().map(|stats| stats.clone()))
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
            .tooltip(tooltip(match scale_mode {
                ViewportScaleMode::AspectFit => "Scale mode: Aspect Fit (click for Fill)",
                ViewportScaleMode::Fill => "Scale mode: Fill (click for Fit)",
            }).build())
            .on_click({
                let view = view.clone();
                move |_event, _window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.scale_mode = match this.scale_mode {
                            ViewportScaleMode::AspectFit => ViewportScaleMode::Fill,
                            ViewportScaleMode::Fill => ViewportScaleMode::AspectFit,
                        };
                        let mut prefs = crate::preferences::UserPreferences::load_or_default();
                        prefs.ui.scale_mode = match this.scale_mode {
                            ViewportScaleMode::AspectFit => "aspect_fit".to_string(),
                            ViewportScaleMode::Fill => "fill".to_string(),
                        };
                        let _ = prefs.save();
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
            .font_family(".SystemUIFont")
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
            .child(
                if self.telemetry_hud_collapsed {
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
                                            let mut prefs = crate::preferences::UserPreferences::load_or_default();
                                            prefs.ui.telemetry_hud_collapsed = false;
                                            let _ = prefs.save();
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
                                    command_button("collapse_pip_hud_btn", ActionVariantKind::Neutral, cx)
                                        .size(px(18.0))
                                        .rounded_full()
                                        .text_size(px(8.0))
                                        .tooltip(tooltip("Collapse live telemetry").build())
                                        .on_click({
                                            let view = view.clone();
                                            move |_event, _window, cx| {
                                                let _ = view.update(cx, |this, cx| {
                                                    this.telemetry_hud_collapsed = true;
                                                    let mut prefs = crate::preferences::UserPreferences::load_or_default();
                                                    prefs.ui.telemetry_hud_collapsed = true;
                                                    let _ = prefs.save();
                                                    cx.notify();
                                                });
                                            }
                                        })
                                        .child(icon(IconName::Minimize).size(px(10.0))),
                                ),
                        )
                        .child(telemetry_hud_metrics_list(stats.as_ref(), &theme))
                },
            )
    }
}

impl Render for UnifiedDashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_latest_frame();
        let snapshot = self.snapshot();
        let role = snapshot.role.clone();
        self.sync_input_session(&role);
        let active_session = role.session().cloned();
        let mesh_snapshot = self.mesh_snapshot();
        let theme = cx.theme().clone();
        let show_drawer = self.drawer_open;
        let is_fullscreen = window.is_fullscreen();
        let compact = f32::from(window.viewport_size().width) < 1200.0;
        let host_stats = self.host_stats_snapshot(&role);
        let side_services = SideServiceUiState {
            talkback: FeatureToggleState {
                available: self.runtime.owner.supports_talkback(),
                enabled: self.runtime.owner.talkback_enabled(),
            },
            clipboard_sync: FeatureToggleState {
                available: self.runtime.owner.supports_clipboard_sync(),
                enabled: self.runtime.owner.clipboard_sync_enabled(),
            },
            file_transfer: FeatureToggleState {
                available: self.runtime.owner.supports_file_transfer(),
                enabled: self.runtime.owner.file_transfer_enabled(),
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
            .font_family(".SystemUIFont")
            .overflow_hidden()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
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
            .child(floating_control_island(
                &role,
                active_session.as_ref(),
                &self.viewer_media_status,
                &self.status,
                self.input_locked,
                side_services,
                self.scale_mode,
                is_fullscreen,
                compact,
                host_stats.as_ref(),
                self.runtime.owner.clone(),
                cx,
            ))
            .child(drawer_trigger_capsule(
                &snapshot.devices,
                show_drawer,
                mesh_snapshot.as_ref(),
                compact,
                cx,
            ))
            .when(show_drawer, |this| {
                this.child(slide_over_management_drawer(
                    &snapshot.devices,
                    active_session.as_ref(),
                    mesh_snapshot.as_ref(),
                    self.mesh_pairing_snapshot.clone(),
                    self.active_tab,
                    self.device_filter,
                    self.input_locked,
                    side_services,
                    self.selected_resolution,
                    self.selected_fps,
                    self.selected_bitrate_kbps,
                    host_stats.as_ref(),
                    self.runtime.owner.clone(),
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
    }
}

fn full_canvas_stream_viewport(
    role: &RoleState,
    frame: Option<&MacDecodedVideoFrame>,
    input_focus: FocusHandle,
    video_surface_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    scale_mode: ViewportScaleMode,
    cx: &mut Context<UnifiedDashboard>,
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
    cx: &mut Context<UnifiedDashboard>,
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
        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, _cx| {
            this.queue_pointer_move(event.position);
        }))
        .on_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
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

fn full_idle_canvas_stage(cx: &mut Context<UnifiedDashboard>) -> Div {
    let theme = cx.theme().clone();
    div()
        .w_full()
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(0x0e0f12))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_4()
                .p_8()
                .rounded(px(8.0))
                .bg(color_glass_card())
                .border_1()
                .border_color(color_border_fine())
                .shadow_lg()
                .child(
                    div()
                        .size(px(60.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(rgb(0x12151c))
                        .border_1()
                        .border_color(color_accent_cyan())
                        .child(
                            icon(IconName::Server)
                                .size(px(26.0))
                                .color(color_accent_cyan()),
                        ),
                )
                .child(
                    div()
                        .text_size(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.content.primary)
                        .child("RemotePlay"),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.content.tertiary)
                        .child("Ready for incoming and outgoing sessions"),
                ),
        )
}

fn full_serving_stage(peer_name: &str, cx: &mut Context<UnifiedDashboard>) -> Div {
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
    viewer_media_status: &UnifiedViewerMediaStatus,
    status: &str,
    input_locked: bool,
    side_services: SideServiceUiState,
    scale_mode: ViewportScaleMode,
    is_fullscreen: bool,
    compact: bool,
    host_stats: Option<&HostStats>,
    owner: Arc<crate::UnifiedServiceOwner>,
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
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
        .absolute()
        .top(px(12.0))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .on_any_mouse_down(cx.listener(|_this, _event, _window, cx| {
            cx.stop_propagation();
        }))
        .child(
            div()
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
                            .h(px(26.0))
                            .px_2()
                            .text_size(px(10.0))
                            .on_click(move |_event, _window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.set_input_locked(!input_locked);
                                    cx.notify();
                                });
                            })
                            .child(if compact {
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
                            let owner = owner.clone();
                            let view = view.clone();
                            this.child(
                                command_button("island_disconnect", ActionVariantKind::Danger, cx)
                                    .h(px(26.0))
                                    .px(if compact { px(8.0) } else { px(12.0) })
                                    .text_size(px(10.0))
                                    .on_click(move |_event, _window, cx| {
                                        let owner = owner.clone();
                                        let _ = view.update(cx, |this, cx| {
                                            this.status = "Disconnecting".to_string();
                                            cx.spawn(async move |this: WeakEntity<UnifiedDashboard>, cx| {
                                                let result = owner.disconnect_active().await;
                                                let _ = this.update(cx, |this, cx| {
                                                    match result {
                                                        Ok(_) => {
                                                            this.status = "Ready".to_string();
                                                            this.drawer_open = true;
                                                        }
                                                        Err(err) => {
                                                            this.status = format!("Disconnect failed: {err}");
                                                        }
                                                    }
                                                    cx.notify();
                                                });
                                            })
                                            .detach();
                                            cx.notify();
                                        });
                                    })
                                    .child(if compact { "End" } else { "Disconnect" }),
                            )
                        }),
                ),
        )
}

fn scale_mode_control(current: ViewportScaleMode, cx: &Context<UnifiedDashboard>) -> Div {
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
    mesh_snapshot: Option<&EasyTierHealthSnapshot>,
    compact: bool,
    cx: &mut Context<UnifiedDashboard>,
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
                            .child("Devices & Mesh"),
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
                )
                .when(mesh_snapshot.is_some() && !compact, |this| {
                    this.child(
                        div()
                            .size(px(6.0))
                            .flex_none()
                            .rounded_full()
                            .bg(color_accent_emerald()),
                    )
                }),
        ),
    )
}

#[allow(clippy::too_many_arguments)]
fn slide_over_management_drawer(
    devices: &[AppDevice],
    active_session: Option<&crate::RoleSession>,
    mesh_snapshot: Option<&EasyTierHealthSnapshot>,
    mesh_pairing_snapshot: Option<MeshPairingSnapshot>,
    active_tab: DrawerTab,
    device_filter: DeviceFilterKind,
    input_locked: bool,
    side_services: SideServiceUiState,
    selected_resolution: (u32, u32),
    selected_fps: u32,
    selected_bitrate_kbps: u32,
    host_stats: Option<&HostStats>,
    owner: Arc<crate::UnifiedServiceOwner>,
    cx: &mut Context<UnifiedDashboard>,
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
                .child(drawer_tab_button("Mesh", DrawerTab::Mesh, active_tab, cx))
                .child(drawer_tab_button(
                    "Security",
                    DrawerTab::Security,
                    active_tab,
                    cx,
                ))
                .child(drawer_tab_button(
                    "Network",
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
                        active_session,
                        device_filter,
                        owner.clone(),
                        cx,
                    )
                    .into_any_element(),
                    DrawerTab::Mesh => {
                        drawer_mesh_tab(mesh_snapshot, mesh_pairing_snapshot, cx).into_any_element()
                    }
                    DrawerTab::Security => {
                        drawer_security_tab(input_locked, side_services, owner, cx)
                            .into_any_element()
                    }
                    DrawerTab::Network => {
                        drawer_network_telemetry_tab(
                            selected_resolution,
                            selected_fps,
                            selected_bitrate_kbps,
                            mesh_snapshot,
                            host_stats,
                            owner,
                            cx,
                        )
                        .into_any_element()
                    }
                }),
        )
}

fn drawer_tab_button(
    label: &'static str,
    tab: DrawerTab,
    current_tab: DrawerTab,
    cx: &Context<UnifiedDashboard>,
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
    devices: &[AppDevice],
    active_session: Option<&crate::RoleSession>,
    filter: DeviceFilterKind,
    owner: Arc<crate::UnifiedServiceOwner>,
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    let view = cx.weak_entity();

    let mut content = div().flex().flex_col().gap_3().child(
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(filter_pill("All", DeviceFilterKind::All, filter, cx))
            .child(filter_pill("LAN", DeviceFilterKind::Lan, filter, cx))
            .child(filter_pill("Mesh", DeviceFilterKind::Mesh, filter, cx))
            .child(filter_pill("Relay", DeviceFilterKind::Relay, filter, cx)),
    );

    let filtered_devices: Vec<&AppDevice> = devices
        .iter()
        .filter(|d| match filter {
            DeviceFilterKind::All => true,
            DeviceFilterKind::Lan => d.scope == remote_core::discovery::DiscoveryScope::Lan,
            DeviceFilterKind::Mesh => d.scope == remote_core::discovery::DiscoveryScope::Mesh,
            DeviceFilterKind::Relay => d.scope == remote_core::discovery::DiscoveryScope::Relay,
        })
        .collect();

    if filtered_devices.is_empty() {
        content = content.child(empty_state(
            "No devices found",
            "No peers are currently online in this device group.",
            cx,
        ));
    } else {
        for device in filtered_devices {
            let is_active = active_session
                .map(|s| s.peer.device_id == device.device_id)
                .unwrap_or(false);
            let is_connectable = device.is_streamable() && !is_active;
            let device_id = device.device_id.clone();
            let owner = owner.clone();

            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .bg(if is_active {
                        rgb(0x192830).into()
                    } else {
                        theme.surface.raised
                    })
                    .border_1()
                    .border_color(if is_active {
                        color_accent_cyan()
                    } else {
                        color_border_fine()
                    })
                    .rounded(px(8.0))
                    .shadow_xs()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().size(px(8.0)).flex_none().rounded_full().bg(
                                        if device.online {
                                            Hsla::from(color_accent_emerald())
                                        } else {
                                            theme.content.disabled
                                        },
                                    ))
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.content.primary)
                                            .whitespace_nowrap()
                                            .truncate()
                                            .child(device.display_name.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_full()
                                    .bg(theme.surface.sunken)
                                    .text_size(px(9.0))
                                    .text_color(theme.content.tertiary)
                                    .child(discovery_scope_label(device.scope)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_size(px(10.0))
                            .text_color(theme.content.tertiary)
                            .child(format!("Endpoint: {}", device.endpoint))
                            .child(if is_active {
                                "Connected Live"
                            } else if device.online {
                                "Available"
                            } else {
                                "Standby"
                            }),
                    )
                    .child(div().flex().gap_2().child({
                        let view = view.clone();
                        command_button(
                            format!("drawer_connect_{device_id}"),
                            if is_connectable {
                                ActionVariantKind::Primary
                            } else {
                                ActionVariantKind::Neutral
                            },
                            cx,
                        )
                        .disabled(!is_connectable)
                        .h(px(28.0))
                        .flex_1()
                        .text_size(px(10.0))
                        .on_click(move |_event, _window, cx| {
                            let owner = owner.clone();
                            let device_id = device_id.clone();
                            let _ = view.update(cx, |this, cx| {
                                this.status = format!("Connecting to {device_id}");
                                this.reset_host_stats();
                                let start_opts = StreamStartOptions {
                                    width: this.selected_resolution.0,
                                    height: this.selected_resolution.1,
                                    fps: this.selected_fps,
                                    bitrate_kbps: this.selected_bitrate_kbps,
                                };
                                cx.spawn(async move |this: WeakEntity<UnifiedDashboard>, cx| {
                                    let result = owner
                                        .connect_device(
                                            &device_id,
                                            start_opts,
                                            crate::unix_now_ms(),
                                        )
                                        .await;
                                    let _ = this.update(cx, |this, cx| {
                                        match result {
                                            Ok(_) => {
                                                this.status = "Waiting for video".to_string();
                                            }
                                            Err(err) => {
                                                this.status = format!("Connection failed: {err}");
                                                this.drawer_open = true;
                                            }
                                        }
                                        cx.notify();
                                    });
                                })
                                .detach();
                                cx.notify();
                            });
                        })
                        .child(if is_active {
                            "Active Viewport"
                        } else {
                            "Connect Stream"
                        })
                    })),
            );
        }
    }

    content
}

fn filter_pill(
    label: &'static str,
    kind: DeviceFilterKind,
    current_filter: DeviceFilterKind,
    cx: &Context<UnifiedDashboard>,
) -> Button {
    let view = cx.weak_entity();
    let is_selected = kind == current_filter;
    command_button(
        format!("filter_{label}"),
        if is_selected {
            ActionVariantKind::Primary
        } else {
            ActionVariantKind::Neutral
        },
        cx,
    )
    .h(px(24.0))
    .flex_1()
    .text_size(px(9.0))
    .on_click(move |_event, _window, cx| {
        let _ = view.update(cx, |this, cx| {
            this.device_filter = kind;
            cx.notify();
        });
    })
    .child(label)
}

fn drawer_mesh_tab(
    mesh_snapshot: Option<&EasyTierHealthSnapshot>,
    mesh_pairing_snapshot: Option<MeshPairingSnapshot>,
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    let mut content = div().flex().flex_col().gap_3();

    if let Some(snapshot) = mesh_snapshot {
        content = content.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .bg(theme.surface.raised)
                .border_1()
                .border_color(color_border_fine())
                .rounded(px(8.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.content.primary)
                                .child("EasyTier Sidecar Status"),
                        )
                        .child(status_pill(
                            &mesh_status_text(snapshot),
                            mesh_status_tone(snapshot),
                            cx,
                        )),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.content.tertiary)
                        .child(format!(
                            "Virtual IP: {}",
                            snapshot
                                .virtual_ip
                                .map(|ip| ip.to_string())
                                .unwrap_or_else(|| "Pending...".into())
                        )),
                ),
        );
    }

    if let Some(snapshot) = mesh_pairing_snapshot {
        content = content.child(mesh_pairing_card(snapshot, cx));
    }

    content
}

fn drawer_security_tab(
    input_locked: bool,
    side_services: SideServiceUiState,
    owner: Arc<crate::UnifiedServiceOwner>,
    cx: &mut Context<UnifiedDashboard>,
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
            "Reliable File Transfer",
            "Enable verified file transfer for the active session",
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
}

fn security_toggle_row(
    title: &'static str,
    detail: &'static str,
    enabled: bool,
    available: bool,
    on_toggle: impl Fn(&mut App) + 'static,
    cx: &Context<UnifiedDashboard>,
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
    mesh_snapshot: Option<&EasyTierHealthSnapshot>,
    host_stats: Option<&HostStats>,
    owner: Arc<crate::UnifiedServiceOwner>,
    cx: &mut Context<UnifiedDashboard>,
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
                if stats.packet_loss_rate > 2.0 { color_accent_amber() } else { color_accent_emerald() },
                cx,
            ))
            .child(telemetry_metric_row(
                "Link status",
                if stats.link_status.is_empty() { "🟢 流畅极佳".to_string() } else { stats.link_status.to_string() },
                color_accent_emerald(),
                cx,
            ));
    } else {
        content = content.child(empty_state(
            "Waiting for telemetry",
            "Live measurements appear after the remote host begins streaming.",
            cx,
        ));
    }

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
                            let owner = owner.clone();
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
                                let owner = owner.clone();
                                let view = view.clone();
                                let _ = view.update(cx, |this, cx| {
                                    this.selected_resolution = (w, h);
                                    this.persist_preferences();
                                    let (send_w, send_h) = this.selected_resolution;
                                    let send_fps = this.selected_fps;
                                    let send_bitrate = this.selected_bitrate_kbps;
                                    cx.spawn(async move |_this: WeakEntity<UnifiedDashboard>, _cx| {
                                        let _ = owner.update_stream_settings(send_w, send_h, send_fps, send_bitrate).await;
                                    }).detach();
                                    cx.notify();
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
                            let owner = owner.clone();
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
                                let owner = owner.clone();
                                let view = view.clone();
                                let _ = view.update(cx, |this, cx| {
                                    this.selected_fps = fps;
                                    this.persist_preferences();
                                    let (send_w, send_h) = this.selected_resolution;
                                    let send_fps = this.selected_fps;
                                    let send_bitrate = this.selected_bitrate_kbps;
                                    cx.spawn(async move |_this: WeakEntity<UnifiedDashboard>, _cx| {
                                        let _ = owner.update_stream_settings(send_w, send_h, send_fps, send_bitrate).await;
                                    }).detach();
                                    cx.notify();
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
                            let owner = owner.clone();
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
                                let owner = owner.clone();
                                let view = view.clone();
                                let _ = view.update(cx, |this, cx| {
                                    this.selected_bitrate_kbps = kbps;
                                    this.persist_preferences();
                                    let (send_w, send_h) = this.selected_resolution;
                                    let send_fps = this.selected_fps;
                                    let send_bitrate = this.selected_bitrate_kbps;
                                    cx.spawn(async move |_this: WeakEntity<UnifiedDashboard>, _cx| {
                                        let _ = owner.update_stream_settings(send_w, send_h, send_fps, send_bitrate).await;
                                    }).detach();
                                    cx.notify();
                                });
                            })
                            .child(label)
                        }))
                )
            ),
    );

    content.child(
        div()
            .mt(px(2.0))
            .p_3()
            .bg(theme.surface.sunken)
            .border_1()
            .border_color(color_border_fine())
            .rounded(px(6.0))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_3()
                    .text_size(px(10.0))
                    .text_color(theme.content.secondary)
                    .child(div().whitespace_nowrap().truncate().child("EasyTier mesh"))
                    .child(
                        div().whitespace_nowrap().truncate().child(
                            mesh_snapshot
                                .map(mesh_status_text)
                                .unwrap_or_else(|| "Not configured".to_string()),
                        ),
                    ),
            ),
    )
}

fn telemetry_metric_row(
    label: &'static str,
    value: String,
    color: gpui::Rgba,
    cx: &Context<UnifiedDashboard>,
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
                .child(label),
        )
        .child(
            div()
                .font_family("monospace")
                .text_color(color)
                .whitespace_nowrap()
                .child(value),
        )
}

fn telemetry_waterfall_overlay(
    collapsed: bool,
    host_stats: Option<&HostStats>,
    cx: &mut Context<UnifiedDashboard>,
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
                    .font_family("monospace")
                    .child(
                        div()
                            .text_color(theme.content.tertiary)
                            .child("Frame Rate"),
                    )
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
                    .font_family("monospace")
                    .child(
                        div()
                            .text_color(theme.content.tertiary)
                            .child("Bitrate"),
                    )
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
                    .font_family("monospace")
                    .child(
                        div()
                            .text_color(theme.content.tertiary)
                            .child("End-to-End"),
                    )
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
                    .font_family("monospace")
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
                    .font_family("monospace")
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
                    .font_family("monospace")
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
                    .font_family("monospace")
                    .child(
                        div()
                            .text_color(theme.content.tertiary)
                            .child("Packet Loss"),
                    )
                    .child(
                        div()
                            .text_color(if stats.packet_loss_rate > 2.0 { color_accent_amber().into() } else { theme.content.secondary })
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
                    .font_family("monospace")
                    .child(
                        div()
                            .text_color(theme.content.tertiary)
                            .child("Link Health"),
                    )
                    .child(
                        div()
                            .text_color(if stats.link_status.is_empty() { color_accent_emerald().into() } else { theme.content.primary })
                            .child(if stats.link_status.is_empty() { "🟢 流畅极佳" } else { stats.link_status }),
                    ),
            )
    } else {
        div()
            .text_size(px(10.0))
            .font_family("monospace")
            .text_color(theme.content.tertiary)
            .child("Waiting for telemetry...")
    }
}

fn mesh_pairing_card(snapshot: MeshPairingSnapshot, cx: &mut Context<UnifiedDashboard>) -> Div {
    let theme = cx.theme().clone();
    let message_tone = pairing_message_tone(snapshot.message_kind);
    let message_color = status_color(message_tone, cx);
    let restart_text = snapshot
        .restart_required
        .then_some("Restart RemotePlay to activate this group.");
    let copy_view = cx.weak_entity();
    let join_view = cx.weak_entity();
    let create_view = cx.weak_entity();
    let repair_view = cx.weak_entity();

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
                                .child("EasyTier Private Mesh"),
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
                .font_family("monospace")
                .truncate()
                .child(snapshot.invite_code),
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
            div()
                .flex()
                .gap_2()
                .child(
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
                )
                .child(
                    command_button(
                        "unified_mesh_admin_setup_from_group",
                        ActionVariantKind::Neutral,
                        cx,
                    )
                    .h(px(28.0))
                    .flex_1()
                    .px_2()
                    .text_size(px(10.0))
                    .on_click(move |_event, _window, cx| {
                        let _ = repair_view.update(cx, |this, cx| {
                            this.install_mesh_admin_setup(cx);
                            cx.notify();
                        });
                    })
                    .child("Repair Mesh"),
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

fn product_mark(cx: &mut Context<UnifiedDashboard>) -> Div {
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
        .active(|style| style.opacity(0.82))
}

fn setup_error_summary(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return "unknown error".to_string();
    }

    let first_line = trimmed.lines().next().unwrap_or(trimmed).trim();
    const MAX_LEN: usize = 96;
    if first_line.chars().count() <= MAX_LEN {
        first_line.to_string()
    } else {
        format!(
            "{}...",
            first_line.chars().take(MAX_LEN).collect::<String>()
        )
    }
}

fn empty_state(
    title: &'static str,
    detail: &'static str,
    cx: &mut Context<UnifiedDashboard>,
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

fn status_color(tone: StatusTone, cx: &Context<UnifiedDashboard>) -> Hsla {
    let theme = cx.theme();
    match tone {
        StatusTone::Neutral => theme.content.disabled,
        StatusTone::Success => theme.status.success.bg,
        StatusTone::Warning => theme.status.warning.bg,
        StatusTone::Error => theme.status.error.bg,
        StatusTone::Info => theme.status.info.bg,
    }
}

fn status_pill(label: &str, tone: StatusTone, cx: &mut Context<UnifiedDashboard>) -> Div {
    let theme = cx.theme().clone();
    div()
        .h(px(22.0))
        .flex_none()
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .rounded_full()
        .bg(theme.surface.sunken)
        .border_1()
        .border_color(theme.border.muted)
        .child(status_dot(tone, px(5.0), cx))
        .child(
            div()
                .text_size(px(9.0))
                .text_color(theme.content.secondary)
                .truncate()
                .child(label.to_string()),
        )
}

fn status_dot(tone: StatusTone, size: Pixels, cx: &Context<UnifiedDashboard>) -> Div {
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
        remote_core::discovery::DiscoveryScope::Lan => "LAN P2P",
        remote_core::discovery::DiscoveryScope::Mesh => "EasyTier Mesh",
        remote_core::discovery::DiscoveryScope::Relay => "Relay Tunnel",
    }
}

fn compact_device_id(device_id: &str) -> String {
    if device_id.len() <= 8 {
        device_id.to_string()
    } else {
        format!("{}...", &device_id[..8])
    }
}

fn mesh_status_text(snapshot: &EasyTierHealthSnapshot) -> String {
    if mesh_needs_admin_setup(Some(snapshot)) {
        return "Mesh needs admin setup".to_string();
    }

    match snapshot.state {
        EasyTierHealthState::Ready => snapshot
            .virtual_ip
            .map(|ip| format!("Mesh {ip}"))
            .unwrap_or_else(|| "Mesh ready".to_string()),
        EasyTierHealthState::Starting => "Mesh starting".to_string(),
        EasyTierHealthState::Degraded => "Mesh degraded".to_string(),
        EasyTierHealthState::Stopped => "Mesh stopped".to_string(),
    }
}

fn mesh_needs_admin_setup(snapshot: Option<&EasyTierHealthSnapshot>) -> bool {
    snapshot.is_some_and(|snapshot| {
        snapshot.issue == Some(EasyTierHealthIssue::RequiresAdminPrivileges)
    })
}

fn mesh_status_tone(snapshot: &EasyTierHealthSnapshot) -> StatusTone {
    match snapshot.state {
        EasyTierHealthState::Ready => StatusTone::Success,
        EasyTierHealthState::Starting => StatusTone::Warning,
        EasyTierHealthState::Degraded => StatusTone::Error,
        EasyTierHealthState::Stopped => StatusTone::Neutral,
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

fn absolute_pointer_event(
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

fn protocol_mouse_button(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Right => Some(1),
        MouseButton::Middle => Some(2),
        MouseButton::Navigate(_) => None,
    }
}

fn protocol_modifiers(modifiers: Modifiers) -> u8 {
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

fn protocol_key_event(keystroke: &Keystroke, pressed: bool) -> Option<protocol::InputEvent> {
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
}

#[cfg(test)]
mod input_tests {
    use super::{
        ControlIslandVisibility, PointerInputTracker, ViewportScaleMode, absolute_pointer_event,
        control_island_visibility, dashboard_refresh_interval, host_stats_available,
        is_shifted_macos_symbol, macos_key_code, product_window_appearance, protocol_key_event,
        protocol_modifiers, protocol_mouse_button,
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
