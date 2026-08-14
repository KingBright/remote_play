use crate::design_system::remote_play_themes;
use crate::mesh_admin::{mesh_setup_was_cancelled, run_mesh_admin_setup};
use crate::{
    AppDevice, MacDecodedVideoFrame, MeshPairingControl, MeshPairingMessageKind,
    MeshPairingSnapshot, RoleState, StreamStartOptions, UnifiedRuntimeConfig, UnifiedRuntimeHandle,
    UnifiedViewerMediaStatus, decoded_video_frame_surface, start_unified_runtime,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use remote_core::{
    VideoFrame,
    mesh::{EasyTierHealthIssue, EasyTierHealthSnapshot, EasyTierHealthState},
};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use yororen_ui::{
    assets::UiAsset,
    component::{self, Button, IconName, button, icon},
    theme::{ActionVariantKind, ActiveTheme, GlobalTheme},
};

pub async fn run_unified_gui(
    config: UnifiedRuntimeConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let runtime = start_unified_runtime(config).await?;
    remote_core::stats::Statistics::start_reporter(runtime.stats.clone(), "Unified", 1);

    let app = Application::new().with_assets(UiAsset);
    app.run(move |cx: &mut App| {
        component::init(cx);
        cx.set_global(GlobalTheme::new_with_themes(
            cx.window_appearance(),
            remote_play_themes(),
        ));
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds(
                point(px(120.0), px(90.0)),
                size(px(1180.0), px(760.0)),
            ))),
            window_min_size: Some(size(px(960.0), px(680.0))),
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
                        Timer::after(Duration::from_millis(16)).await;
                        if view.update(&mut *cx, |_view, cx| cx.notify()).is_err() {
                            break;
                        }
                    }
                }
            })
            .detach();

            view
        })
        .expect("failed to open RemotePlay window");
    });

    Ok(())
}

struct UnifiedDashboard {
    runtime: UnifiedRuntimeHandle,
    app_runtime: Arc<Mutex<crate::UnifiedAppRuntime>>,
    viewer_frame: Option<Arc<Mutex<Option<MacDecodedVideoFrame>>>>,
    current_frame: Option<MacDecodedVideoFrame>,
    mesh_health: Option<watch::Receiver<EasyTierHealthSnapshot>>,
    viewer_media_status: UnifiedViewerMediaStatus,
    mesh_pairing: Option<MeshPairingControl>,
    mesh_pairing_snapshot: Option<MeshPairingSnapshot>,
    pointer_input: PointerInputTracker,
    input_focus: FocusHandle,
    video_surface_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    input_session_id: Option<u32>,
    status: String,
}

impl UnifiedDashboard {
    fn new(runtime: UnifiedRuntimeHandle, cx: &mut Context<Self>) -> Self {
        let app_runtime = runtime.owner.runtime();
        let viewer_frame = runtime.viewer_frame.clone();
        let mesh_health = runtime.owner.mesh_health_rx();
        let viewer_media_status = runtime.viewer_media_status.clone();
        let mesh_pairing = runtime.mesh_pairing.clone();
        let mesh_pairing_snapshot = mesh_pairing.as_ref().map(MeshPairingControl::snapshot);
        Self {
            runtime,
            app_runtime,
            viewer_frame,
            current_frame: None,
            mesh_health,
            viewer_media_status,
            mesh_pairing,
            mesh_pairing_snapshot,
            pointer_input: PointerInputTracker::default(),
            input_focus: cx.focus_handle(),
            video_surface_bounds: Arc::new(Mutex::new(None)),
            input_session_id: None,
            status: "Ready".to_string(),
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
            self.current_frame = Some(frame);
        }
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

    fn sync_input_session(&mut self, role: &RoleState) {
        let input_session_id = match role {
            RoleState::Viewing(session) => Some(session.session_id),
            RoleState::Idle | RoleState::Connecting(_) | RoleState::Serving(_) => None,
        };
        if self.input_session_id != input_session_id {
            self.pointer_input = PointerInputTracker::default();
            self.input_session_id = input_session_id;
        }
    }

    fn queue_pointer_move(&mut self, position: Point<Pixels>) {
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
        ) {
            self.runtime.owner.queue_viewing_input(event);
        }
    }

    fn queue_pointer_button(&self, button: MouseButton, pressed: bool) {
        let Some(button) = protocol_mouse_button(button) else {
            return;
        };
        let event = if pressed {
            protocol::InputEvent::MouseDown(button)
        } else {
            protocol::InputEvent::MouseUp(button)
        };
        self.runtime.owner.queue_viewing_input(event);
    }

    fn queue_pointer_scroll(&mut self, delta: ScrollDelta) {
        let delta = delta.pixel_delta(px(40.0));
        if let Some(event) = self
            .pointer_input
            .scrolled((f32::from(delta.x), f32::from(delta.y)))
        {
            self.runtime.owner.queue_viewing_input(event);
        }
    }

    fn queue_key(&self, keystroke: &Keystroke, pressed: bool) {
        let Some(event) = protocol_key_event(keystroke, pressed) else {
            return;
        };
        self.runtime.owner.queue_viewing_input(event);
    }

    fn queue_modifiers(&self, event: &ModifiersChangedEvent) {
        let mut modifiers = protocol_modifiers(event.modifiers);
        if event.capslock.on {
            modifiers |= protocol::input_modifiers::CAPS_LOCK;
        }
        self.runtime
            .owner
            .queue_viewing_input(protocol::InputEvent::ModifiersChanged(modifiers));
    }
}

#[derive(Debug, Default)]
struct PointerInputTracker {
    scroll_remainder: (f32, f32),
}

impl PointerInputTracker {
    fn reset_pointer(&mut self) {
        self.scroll_remainder = (0.0, 0.0);
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

    let scale = (surface_width / frame_width).min(surface_height / frame_height);
    let displayed_width = frame_width * scale;
    let displayed_height = frame_height * scale;
    let displayed_x = surface_x + (surface_width - displayed_width) * 0.5;
    let displayed_y = surface_y + (surface_height - displayed_height) * 0.5;
    if position.0 < displayed_x
        || position.1 < displayed_y
        || position.0 > displayed_x + displayed_width
        || position.1 > displayed_y + displayed_height
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

impl Render for UnifiedDashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_latest_frame();
        let snapshot = self.snapshot();
        let role = snapshot.role.clone();
        self.sync_input_session(&role);
        let active_session = role.session().cloned();
        let mesh_snapshot = self.mesh_snapshot();
        let needs_mesh_admin_setup = mesh_needs_admin_setup(mesh_snapshot.as_ref());
        let compact = f32::from(window.viewport_size().width) < 1080.0;
        let theme = cx.theme().clone();

        div()
            .w_full()
            .h_full()
            .min_w_0()
            .bg(theme.surface.canvas)
            .text_color(theme.content.primary)
            .font_family(".SystemUIFont")
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
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .h_full()
                    .child(app_bar(
                        &role,
                        &self.status,
                        mesh_snapshot.as_ref(),
                        &self.viewer_media_status,
                        needs_mesh_admin_setup,
                        compact,
                        cx,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .overflow_hidden()
                            .child(device_panel(
                                &snapshot.devices,
                                active_session.as_ref(),
                                self.mesh_pairing_snapshot.clone(),
                                self.runtime.owner.clone(),
                                compact,
                                cx,
                            ))
                            .child(session_panel(
                                &role,
                                active_session.as_ref(),
                                self.current_frame.as_ref(),
                                self.input_focus.clone(),
                                self.video_surface_bounds.clone(),
                                self.runtime.owner.clone(),
                                &self.viewer_media_status,
                                &self.status,
                                compact,
                                cx,
                            )),
                    ),
            )
    }
}

fn app_bar(
    role: &RoleState,
    status: &str,
    mesh_snapshot: Option<&EasyTierHealthSnapshot>,
    viewer_media_status: &UnifiedViewerMediaStatus,
    needs_mesh_admin_setup: bool,
    compact: bool,
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    let (role_text, role_tone) = role_status(role);

    div()
        .h(px(70.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_5()
        .bg(theme.surface.base)
        .border_b_1()
        .border_color(theme.border.divider)
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .min_w_0()
                .child(product_mark(cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(16.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.content.primary)
                                .child("RemotePlay"),
                        )
                        .child(
                            div()
                                .mt(px(2.0))
                                .text_size(px(11.0))
                                .text_color(theme.content.tertiary)
                                .child("Device workspace"),
                        ),
                )
                .when(!matches!(role, RoleState::Idle), |this| {
                    this.child(status_pill(role_text, role_tone, cx))
                }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .min_w_0()
                .when(!compact && status != "Ready", |this| {
                    this.child(status_pill(status, StatusTone::Info, cx))
                })
                .when(!compact, |this| {
                    this.child(status_pill(
                        viewer_media_status_text(viewer_media_status),
                        viewer_media_status_tone(viewer_media_status),
                        cx,
                    ))
                })
                .children(mesh_snapshot.map(|snapshot| {
                    status_pill(&mesh_status_text(snapshot), mesh_status_tone(snapshot), cx)
                }))
                .when(!compact && mesh_snapshot.is_none(), |this| {
                    this.child(status_pill("Local only", StatusTone::Neutral, cx))
                })
                .when(needs_mesh_admin_setup, |this| {
                    this.child(mesh_admin_setup_button(true, compact, cx))
                }),
        )
}

fn product_mark(cx: &mut Context<UnifiedDashboard>) -> Div {
    let theme = cx.theme();
    div()
        .size(px(34.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.0))
        .bg(theme.action.primary.bg)
        .text_color(theme.action.primary.fg)
        .child(
            div()
                .size(px(16.0))
                .flex()
                .flex_col()
                .justify_between()
                .child(
                    div()
                        .h(px(5.0))
                        .w_full()
                        .rounded(px(1.0))
                        .bg(theme.action.primary.fg),
                )
                .child(
                    div()
                        .h(px(5.0))
                        .w(px(10.0))
                        .rounded(px(1.0))
                        .bg(theme.action.primary.fg),
                ),
        )
}

fn device_panel(
    devices: &[AppDevice],
    active_session: Option<&crate::RoleSession>,
    mesh_pairing_snapshot: Option<MeshPairingSnapshot>,
    owner: Arc<crate::UnifiedServiceOwner>,
    compact: bool,
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    let panel = div()
        .w(if compact { px(342.0) } else { px(382.0) })
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .min_h_0()
        .bg(theme.surface.base)
        .border_r_1()
        .border_color(theme.border.divider)
        .overflow_hidden()
        .child(panel_header(
            "Devices",
            &format!(
                "{} available",
                devices.iter().filter(|device| device.online).count()
            ),
            cx,
        ));

    let mut list = div()
        .id("unified_device_list")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .gap_3()
        .px_4()
        .pt_2()
        .pb_4()
        .overflow_y_scroll();
    if let Some(snapshot) = mesh_pairing_snapshot {
        list = list.child(mesh_pairing_card(snapshot, cx));
    }
    if devices.is_empty() {
        list = list.child(empty_state(
            "No devices yet",
            "Devices in this private network will appear automatically.",
            cx,
        ));
    } else {
        for device in devices {
            list = list.child(device_row(device, active_session, owner.clone(), cx));
        }
    }
    panel.child(list)
}

fn mesh_pairing_card(snapshot: MeshPairingSnapshot, cx: &mut Context<UnifiedDashboard>) -> Div {
    let theme = cx.theme();
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
        .p_4()
        .bg(theme.surface.raised)
        .border_1()
        .border_color(theme.border.default)
        .rounded(px(7.0))
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
                                .text_size(px(13.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.content.primary)
                                .child("Private network"),
                        )
                        .child(
                            div()
                                .mt(px(3.0))
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
                        .h(px(22.0))
                        .flex()
                        .items_center()
                        .rounded(px(4.0))
                        .bg(theme.status.info.bg)
                        .text_size(px(9.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.status.info.fg)
                        .child("SECURE"),
                ),
        )
        .child(
            div()
                .h(px(36.0))
                .flex()
                .items_center()
                .px_3()
                .bg(theme.surface.sunken)
                .border_1()
                .border_color(theme.border.muted)
                .rounded(px(5.0))
                .text_size(px(11.0))
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
                        .h(px(32.0))
                        .flex_1()
                        .px_3()
                        .text_size(px(11.0))
                        .on_click(move |_event, _window, cx| {
                            let _ = copy_view.update(cx, |this, cx| {
                                this.copy_mesh_invite_code(cx);
                                cx.notify();
                            });
                        })
                        .child("Copy code"),
                )
                .child(
                    command_button(
                        "unified_join_mesh_invite_clipboard",
                        ActionVariantKind::Neutral,
                        cx,
                    )
                    .h(px(32.0))
                    .flex_1()
                    .px_3()
                    .text_size(px(11.0))
                    .on_click(move |_event, _window, cx| {
                        let _ = join_view.update(cx, |this, cx| {
                            this.join_mesh_group_from_clipboard(cx);
                            cx.notify();
                        });
                    })
                    .child("Join copied"),
                ),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    command_button("unified_create_mesh_group", ActionVariantKind::Neutral, cx)
                        .h(px(30.0))
                        .flex_1()
                        .px_2()
                        .text_size(px(10.0))
                        .on_click(move |_event, _window, cx| {
                            let _ = create_view.update(cx, |this, cx| {
                                this.create_mesh_group();
                                cx.notify();
                            });
                        })
                        .child("New network"),
                )
                .child(
                    command_button(
                        "unified_mesh_admin_setup_from_group",
                        ActionVariantKind::Neutral,
                        cx,
                    )
                    .h(px(30.0))
                    .flex_1()
                    .px_2()
                    .text_size(px(10.0))
                    .on_click(move |_event, _window, cx| {
                        let _ = repair_view.update(cx, |this, cx| {
                            this.install_mesh_admin_setup(cx);
                            cx.notify();
                        });
                    })
                    .child("Repair mesh"),
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

fn device_row(
    device: &AppDevice,
    active_session: Option<&crate::RoleSession>,
    owner: Arc<crate::UnifiedServiceOwner>,
    cx: &mut Context<UnifiedDashboard>,
) -> impl IntoElement {
    let theme = cx.theme();
    let is_active = active_session
        .map(|session| session.peer.device_id == device.device_id)
        .unwrap_or(false);
    let status_tone = if is_active {
        StatusTone::Success
    } else if device.is_streamable() {
        StatusTone::Info
    } else if device.online {
        StatusTone::Warning
    } else {
        StatusTone::Neutral
    };
    let is_connectable = device.is_streamable() && !is_active;
    let device_id = device.device_id.clone();
    let device_id_for_status = device_id.clone();
    let view = cx.weak_entity();
    let action = command_button(
        format!("connect_device_{device_id}"),
        if is_connectable {
            ActionVariantKind::Primary
        } else {
            ActionVariantKind::Neutral
        },
        cx,
    )
    .disabled(!is_connectable)
    .h(px(30.0))
    .flex_none()
    .px_3()
    .text_size(px(10.0))
    .on_click(move |_event, _window, cx| {
        let owner = owner.clone();
        let device_id = device_id.clone();
        let _ = view.update(cx, |this, cx| {
            this.status = format!("Connecting to {device_id_for_status}");
            cx.background_executor()
                .spawn(async move {
                    let _ = owner
                        .connect_device(
                            &device_id,
                            StreamStartOptions::default(),
                            crate::unix_now_ms(),
                        )
                        .await;
                })
                .detach();
            cx.notify();
        });
    })
    .child(if is_active { "Active" } else { "Connect" });

    div()
        .id(format!("device_row_{}", device.device_id))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .min_h(px(58.0))
        .px_3()
        .py_2()
        .bg(if is_active {
            theme.surface.hover
        } else {
            theme.surface.base
        })
        .border_1()
        .border_color(if is_active {
            theme.border.focus
        } else {
            theme.border.muted
        })
        .rounded(px(6.0))
        .hover(|style| style.bg(theme.surface.hover))
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .min_w_0()
                .child(
                    div()
                        .size(px(32.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .bg(theme.surface.sunken)
                        .child(
                            icon(IconName::Server)
                                .size(px(14.0))
                                .color(theme.content.secondary),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.content.primary)
                                .truncate()
                                .child(device.display_name.clone()),
                        )
                        .child(
                            div()
                                .mt(px(3.0))
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(10.0))
                                .text_color(theme.content.tertiary)
                                .truncate()
                                .child(status_dot(status_tone, px(5.0), cx))
                                .child(format!(
                                    "{}  /  {}",
                                    device_row_status(device, is_active),
                                    discovery_scope_label(device.scope)
                                )),
                        ),
                ),
        )
        .child(action)
}

#[allow(clippy::too_many_arguments)]
fn session_panel(
    role: &RoleState,
    active_session: Option<&crate::RoleSession>,
    current_frame: Option<&MacDecodedVideoFrame>,
    input_focus: FocusHandle,
    video_surface_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    owner: Arc<crate::UnifiedServiceOwner>,
    viewer_media_status: &UnifiedViewerMediaStatus,
    status: &str,
    compact: bool,
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
    let theme = cx.theme().clone();
    let disconnect_view = cx.weak_entity();
    let panel = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .min_h_0()
        .bg(theme.surface.canvas)
        .overflow_hidden()
        .child(session_header(role, active_session, status, cx));

    let body = if let Some(session) = active_session {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_4()
            .p_5()
            .child(session_stage(
                role,
                current_frame,
                input_focus.clone(),
                video_surface_bounds.clone(),
                cx,
            ))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_4()
                    .min_h(px(70.0))
                    .px_4()
                    .py_3()
                    .bg(theme.surface.base)
                    .border_1()
                    .border_color(theme.border.muted)
                    .rounded(px(7.0))
                    .child(session_metric(
                        "Connection",
                        &session.peer.endpoint.to_string(),
                        cx,
                    ))
                    .child(metric_divider(cx))
                    .child(session_metric(
                        "Media",
                        &viewer_media_status_detail(viewer_media_status),
                        cx,
                    ))
                    .when(!compact, |this| {
                        this.child(metric_divider(cx)).child(session_metric(
                            "Activity",
                            &activity_text(session.last_activity_ms),
                            cx,
                        ))
                    })
                    .child(
                        command_button("disconnect_active_session", ActionVariantKind::Danger, cx)
                            .flex_none()
                            .h(px(34.0))
                            .px_3()
                            .text_size(px(11.0))
                            .on_click(move |_event, _window, cx| {
                                let owner = owner.clone();
                                let _ = disconnect_view.update(cx, |this, cx| {
                                    this.status = "Disconnecting".to_string();
                                    cx.background_executor()
                                        .spawn(async move {
                                            let _ = owner.disconnect_active().await;
                                        })
                                        .detach();
                                    cx.notify();
                                });
                            })
                            .child("Disconnect"),
                    ),
            )
    } else {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_4()
            .p_5()
            .child(session_stage(
                role,
                None,
                input_focus,
                video_surface_bounds,
                cx,
            ))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_4()
                    .min_h(px(70.0))
                    .px_4()
                    .py_3()
                    .bg(theme.surface.base)
                    .border_1()
                    .border_color(theme.border.muted)
                    .rounded(px(7.0))
                    .child(session_metric("Incoming sessions", "Available", cx))
                    .child(metric_divider(cx))
                    .child(session_metric(
                        "Viewer media",
                        &viewer_media_status_detail(viewer_media_status),
                        cx,
                    )),
            )
    };

    panel.child(body)
}

fn session_header(
    role: &RoleState,
    session: Option<&crate::RoleSession>,
    status: &str,
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
    let theme = cx.theme();
    let title = session
        .map(|session| session.peer.display_name.clone())
        .unwrap_or_else(|| "No active session".to_string());
    let subtitle = session
        .map(|session| session_context(role, session.session_id))
        .unwrap_or_else(|| {
            if status == "Ready" {
                "This device is available".to_string()
            } else {
                status.to_string()
            }
        });
    let (_, tone) = role_status(role);

    div()
        .h(px(76.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_5()
        .bg(theme.surface.canvas)
        .border_b_1()
        .border_color(theme.border.divider)
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.content.tertiary)
                        .child("REMOTE SESSION"),
                )
                .child(
                    div()
                        .mt(px(4.0))
                        .text_size(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.content.primary)
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .mt(px(2.0))
                        .text_size(px(10.0))
                        .text_color(theme.content.tertiary)
                        .truncate()
                        .child(subtitle),
                ),
        )
        .child(status_pill(session_state_label(role), tone, cx))
}

fn panel_header(title: &'static str, detail: &str, cx: &mut Context<UnifiedDashboard>) -> Div {
    let theme = cx.theme();
    div()
        .h(px(64.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_4()
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(14.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.content.primary)
                        .child(title),
                )
                .child(
                    div()
                        .mt(px(2.0))
                        .text_size(px(10.0))
                        .text_color(theme.content.tertiary)
                        .child(detail.to_string()),
                ),
        )
}

fn session_stage(
    role: &RoleState,
    frame: Option<&MacDecodedVideoFrame>,
    input_focus: FocusHandle,
    video_surface_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    cx: &mut Context<UnifiedDashboard>,
) -> AnyElement {
    match role {
        RoleState::Viewing(_) | RoleState::Connecting(_) => video_view(
            frame,
            if matches!(role, RoleState::Connecting(_)) {
                "Establishing secure session"
            } else {
                "Waiting for video"
            },
            input_focus,
            video_surface_bounds,
            cx,
        ),
        RoleState::Serving(session) => {
            serving_stage(&session.peer.display_name, cx).into_any_element()
        }
        RoleState::Idle => idle_stage(cx).into_any_element(),
    }
}

fn video_view(
    frame: Option<&MacDecodedVideoFrame>,
    fallback: &'static str,
    input_focus: FocusHandle,
    video_surface_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    cx: &mut Context<UnifiedDashboard>,
) -> AnyElement {
    let theme = cx.theme();
    let content = if let Some(frame) = frame {
        decoded_video_frame_surface(frame)
    } else {
        div()
            .w_full()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0x050706))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(36.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(7.0))
                            .bg(theme.surface.raised)
                            .child(
                                icon(IconName::Maximize(false))
                                    .size(px(16.0))
                                    .color(theme.content.tertiary),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.content.tertiary)
                            .child(fallback),
                    ),
            )
            .into_any_element()
    };

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
        .id("unified_video_input_surface")
        .track_focus(&input_focus)
        .relative()
        .w_full()
        .flex_1()
        .min_h(px(280.0))
        .bg(rgb(0x050706))
        .border_1()
        .border_color(theme.border.default)
        .rounded(px(7.0))
        .overflow_hidden()
        .shadow_sm()
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

fn idle_stage(cx: &mut Context<UnifiedDashboard>) -> Div {
    let theme = cx.theme();
    div()
        .w_full()
        .flex_1()
        .min_h(px(280.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.0))
        .bg(theme.surface.base)
        .border_1()
        .border_color(theme.border.muted)
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .size(px(52.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.0))
                        .bg(theme.surface.raised)
                        .border_1()
                        .border_color(theme.border.default)
                        .child(
                            icon(IconName::Server)
                                .size(px(22.0))
                                .color(theme.content.secondary),
                        ),
                )
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.content.primary)
                        .child("Available for incoming sessions"),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.content.tertiary)
                        .child("No active connection"),
                ),
        )
}

fn serving_stage(peer_name: &str, cx: &mut Context<UnifiedDashboard>) -> Div {
    let theme = cx.theme();
    div()
        .w_full()
        .flex_1()
        .min_h(px(280.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.0))
        .bg(theme.surface.sunken)
        .border_1()
        .border_color(theme.border.muted)
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .size(px(52.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.0))
                        .bg(theme.status.info.bg)
                        .child(
                            icon(IconName::User)
                                .size(px(22.0))
                                .color(theme.status.info.fg),
                        ),
                )
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.content.primary)
                        .child("Incoming session active"),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.content.tertiary)
                        .child(format!("Connected with {peer_name}")),
                ),
        )
}

fn session_metric(label: &'static str, value: &str, cx: &mut Context<UnifiedDashboard>) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .text_size(px(9.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.content.tertiary)
                .child(label),
        )
        .child(
            div()
                .mt(px(5.0))
                .text_size(px(11.0))
                .text_color(theme.content.primary)
                .truncate()
                .child(value.to_string()),
        )
}

fn metric_divider(cx: &mut Context<UnifiedDashboard>) -> Div {
    div()
        .w(px(1.0))
        .h(px(30.0))
        .flex_none()
        .bg(cx.theme().border.divider)
}

fn status_pill(label: &str, tone: StatusTone, cx: &mut Context<UnifiedDashboard>) -> Div {
    let theme = cx.theme();
    div()
        .h(px(26.0))
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .rounded(px(5.0))
        .bg(theme.surface.raised)
        .border_1()
        .border_color(theme.border.muted)
        .child(status_dot(tone, px(6.0), cx))
        .child(
            div()
                .text_size(px(10.0))
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

fn mesh_admin_setup_button(
    needs_admin_setup: bool,
    compact: bool,
    cx: &mut Context<UnifiedDashboard>,
) -> impl IntoElement {
    let view = cx.weak_entity();
    command_button(
        "unified_mesh_admin_setup",
        if needs_admin_setup {
            ActionVariantKind::Primary
        } else {
            ActionVariantKind::Neutral
        },
        cx,
    )
    .h(px(32.0))
    .px_3()
    .text_size(px(10.0))
    .on_click(move |_event, _window, cx| {
        let _ = view.update(cx, |this, cx| {
            this.install_mesh_admin_setup(cx);
            cx.notify();
        });
    })
    .child(if compact {
        if needs_admin_setup { "Setup" } else { "Repair" }
    } else {
        mesh_admin_setup_label(needs_admin_setup)
    })
}

fn command_button(
    id: impl Into<ElementId>,
    variant: ActionVariantKind,
    cx: &Context<UnifiedDashboard>,
) -> Button {
    let focus = cx.theme().border.focus;
    button(id)
        .variant(variant)
        .focusable()
        .focus_visible(move |style| style.border_2().border_color(focus))
        .active(|style| style.opacity(0.82))
}

fn mesh_admin_setup_label(needs_admin_setup: bool) -> &'static str {
    if needs_admin_setup {
        "Setup Mesh"
    } else {
        "Repair Mesh"
    }
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
    let theme = cx.theme();
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

fn role_status(role: &RoleState) -> (&'static str, StatusTone) {
    match role {
        RoleState::Idle => ("Ready", StatusTone::Neutral),
        RoleState::Connecting(_) => ("Connecting", StatusTone::Warning),
        RoleState::Viewing(_) => ("Controlling", StatusTone::Success),
        RoleState::Serving(_) => ("Sharing", StatusTone::Info),
    }
}

fn session_state_label(role: &RoleState) -> &'static str {
    role_status(role).0
}

fn session_context(role: &RoleState, session_id: u32) -> String {
    let action = match role {
        RoleState::Connecting(_) => "Securing connection",
        RoleState::Viewing(_) => "Remote control session",
        RoleState::Serving(_) => "Incoming control session",
        RoleState::Idle => "Ready",
    };
    format!("{action}  /  Session {session_id}")
}

fn activity_text(last_activity_ms: u64) -> String {
    let elapsed_ms = crate::unix_now_ms().saturating_sub(last_activity_ms);
    match elapsed_ms {
        0..=999 => "Active now".to_string(),
        1_000..=59_999 => format!("{} seconds ago", elapsed_ms / 1_000),
        _ => format!("{} minutes ago", elapsed_ms / 60_000),
    }
}

fn device_row_status(device: &AppDevice, is_active: bool) -> &'static str {
    if is_active {
        "Connected"
    } else if device.is_streamable() {
        "Connectable"
    } else if device.online {
        "Online"
    } else {
        "Offline"
    }
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
        remote_core::discovery::DiscoveryScope::Mesh => "Mesh",
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

fn viewer_media_status_text(status: &UnifiedViewerMediaStatus) -> &'static str {
    match status {
        UnifiedViewerMediaStatus::Disabled => "Media off",
        UnifiedViewerMediaStatus::Ready => "Media ready",
        UnifiedViewerMediaStatus::AudioUnavailable { .. } => "Audio off",
        UnifiedViewerMediaStatus::Unavailable { .. } => "Media off",
    }
}

fn viewer_media_status_detail(status: &UnifiedViewerMediaStatus) -> String {
    match status {
        UnifiedViewerMediaStatus::Disabled => "Disabled".to_string(),
        UnifiedViewerMediaStatus::Ready => "Ready".to_string(),
        UnifiedViewerMediaStatus::AudioUnavailable { reason } => {
            format!("Video ready, audio unavailable: {reason}")
        }
        UnifiedViewerMediaStatus::Unavailable { reason } => format!("Unavailable: {reason}"),
    }
}

fn viewer_media_status_tone(status: &UnifiedViewerMediaStatus) -> StatusTone {
    match status {
        UnifiedViewerMediaStatus::Ready => StatusTone::Success,
        UnifiedViewerMediaStatus::AudioUnavailable { .. } => StatusTone::Warning,
        UnifiedViewerMediaStatus::Disabled | UnifiedViewerMediaStatus::Unavailable { .. } => {
            StatusTone::Neutral
        }
    }
}

#[cfg(test)]
mod input_tests {
    use super::{
        PointerInputTracker, absolute_pointer_event, is_shifted_macos_symbol, macos_key_code,
        protocol_key_event, protocol_modifiers, protocol_mouse_button,
    };
    use gpui::{
        AppContext, Context, FocusHandle, InteractiveElement, IntoElement, Keystroke, Modifiers,
        MouseButton, NavigationDirection, ParentElement, Render, Styled, TestAppContext, Window,
        canvas, div, point, px, size,
    };
    use protocol::InputEvent;

    #[test]
    fn absolute_pointer_mapping_uses_the_contained_video_rect() {
        let event =
            absolute_pointer_event((500.0, 350.0), (100.0, 50.0, 800.0, 600.0), (1920, 1080));
        assert!(matches!(
            event,
            Some(InputEvent::MouseMoveAbsolute { x, y })
                if (32_767..=32_768).contains(&x) && (32_767..=32_768).contains(&y)
        ));

        assert_eq!(
            absolute_pointer_event((500.0, 75.0), (100.0, 50.0, 800.0, 600.0), (1920, 1080),),
            None,
            "letterbox input must not move the remote pointer",
        );
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
