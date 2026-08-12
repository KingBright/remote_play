use crate::mesh_admin::{mesh_setup_was_cancelled, run_mesh_admin_setup};
use crate::{
    AppDevice, MacDecodedVideoFrame, MeshPairingControl, MeshPairingMessageKind,
    MeshPairingSnapshot, RoleState, StreamStartOptions, UnifiedRuntimeConfig, UnifiedRuntimeHandle,
    UnifiedViewerMediaStatus, decoded_video_frame_surface, start_unified_runtime,
};
use gpui::*;
use remote_core::mesh::{EasyTierHealthIssue, EasyTierHealthSnapshot, EasyTierHealthState};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

pub async fn run_unified_gui(
    config: UnifiedRuntimeConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let runtime = start_unified_runtime(config).await?;
    remote_core::stats::Statistics::start_reporter(runtime.stats.clone(), "Unified", 1);

    let app = Application::new();
    app.run(move |cx: &mut App| {
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds(
                point(px(120.0), px(90.0)),
                size(px(1180.0), px(760.0)),
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("RemotePlay".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let mut runtime = Some(runtime);
        cx.open_window(window_options, move |_, cx| {
            let view = cx.new(|_| {
                UnifiedDashboard::new(
                    runtime
                        .take()
                        .expect("runtime should be moved into window once"),
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
    status: String,
}

impl UnifiedDashboard {
    fn new(runtime: UnifiedRuntimeHandle) -> Self {
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
}

struct DashboardSnapshot {
    role: RoleState,
    devices: Vec<AppDevice>,
}

impl Render for UnifiedDashboard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_latest_frame();
        let snapshot = self.snapshot();
        let role = snapshot.role.clone();
        let active_session = role.session().cloned();
        let (role_text, role_color) = role_status(&role);
        let mesh_snapshot = self.mesh_snapshot();
        let needs_mesh_admin_setup = mesh_needs_admin_setup(mesh_snapshot.as_ref());

        div()
            .w_full()
            .h_full()
            .bg(rgb(0x101216))
            .text_color(rgb(0xe8edf4))
            .font_family("Inter")
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .h_full()
                    .p_5()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div().w(px(10.0)).h(px(10.0)).rounded_full().bg(role_color),
                                    )
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(rgb(0xf7fafc))
                                            .child("RemotePlay"),
                                    )
                                    .child(status_pill(role_text, role_color)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(status_pill(&self.status, rgb(0x7aa2f7)))
                                    .child(status_pill(
                                        viewer_media_status_text(&self.viewer_media_status),
                                        viewer_media_status_color(&self.viewer_media_status),
                                    ))
                                    .children(mesh_snapshot.as_ref().map(|snapshot| {
                                        status_pill(
                                            &mesh_status_text(snapshot),
                                            mesh_status_color(snapshot),
                                        )
                                    }))
                                    .child(mesh_admin_setup_button(needs_mesh_admin_setup, cx)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .gap_4()
                            .overflow_hidden()
                            .child(device_panel(
                                &snapshot.devices,
                                active_session.as_ref(),
                                self.mesh_pairing_snapshot.clone(),
                                self.runtime.owner.clone(),
                                cx,
                            ))
                            .child(session_panel(
                                active_session.as_ref(),
                                self.current_frame.as_ref(),
                                self.runtime.owner.clone(),
                                &self.viewer_media_status,
                                &self.status,
                                cx,
                            )),
                    ),
            )
    }
}

fn device_panel(
    devices: &[AppDevice],
    active_session: Option<&crate::RoleSession>,
    mesh_pairing_snapshot: Option<MeshPairingSnapshot>,
    owner: Arc<crate::UnifiedServiceOwner>,
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
    let panel = div()
        .w(px(460.0))
        .h_full()
        .flex()
        .flex_col()
        .bg(rgb(0x161a21))
        .border_1()
        .border_color(rgb(0x2a3038))
        .rounded_sm()
        .overflow_hidden()
        .child(panel_header("Devices", &format!("{} known", devices.len())));

    let mut list = div()
        .id("unified_device_list")
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .overflow_y_scroll();
    if let Some(snapshot) = mesh_pairing_snapshot {
        list = list.child(mesh_pairing_card(snapshot, cx));
    }
    if devices.is_empty() {
        list = list.child(empty_state("No devices online"));
    } else {
        for device in devices {
            list = list.child(device_row(device, active_session, owner.clone(), cx));
        }
    }
    panel.child(list)
}

fn mesh_pairing_card(snapshot: MeshPairingSnapshot, cx: &mut Context<UnifiedDashboard>) -> Div {
    let message_color = pairing_message_color(snapshot.message_kind);
    let restart_text = snapshot
        .restart_required
        .then_some("Mesh will use this group after restart.");
    let mut card = div()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .bg(rgb(0x1d222b))
        .border_1()
        .border_color(rgb(0x303642))
        .rounded_sm()
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
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(0xf4f7fb))
                                .child("Device Group"),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(rgb(0x9aa6b2))
                                .truncate()
                                .child(format!(
                                    "{} · {}",
                                    snapshot.network_name,
                                    compact_device_id(&snapshot.device_id)
                                )),
                        ),
                )
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(rgb(0x202631))
                        .text_xs()
                        .text_color(rgb(0xc7d0dc))
                        .child("RPM2"),
                ),
        )
        .child(
            div()
                .px_3()
                .py_2()
                .bg(rgb(0x101216))
                .border_1()
                .border_color(rgb(0x303642))
                .rounded_sm()
                .text_xs()
                .text_color(rgb(0xe8edf4))
                .truncate()
                .child(snapshot.invite_code),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .id("unified_copy_mesh_invite")
                        .bg(rgb(0x2563eb))
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .text_sm()
                        .text_color(rgb(0xffffff))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _event, _window, cx| {
                                this.copy_mesh_invite_code(cx);
                                cx.notify();
                            }),
                        )
                        .child("Copy Code"),
                )
                .child(
                    div()
                        .id("unified_join_mesh_invite_clipboard")
                        .bg(rgb(0x303642))
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .text_sm()
                        .text_color(rgb(0xffffff))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _event, _window, cx| {
                                this.join_mesh_group_from_clipboard(cx);
                                cx.notify();
                            }),
                        )
                        .child("Join Clipboard"),
                )
                .child(
                    div()
                        .id("unified_create_mesh_group")
                        .bg(rgb(0x303642))
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .text_sm()
                        .text_color(rgb(0xffffff))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _event, _window, cx| {
                                this.create_mesh_group();
                                cx.notify();
                            }),
                        )
                        .child("New Group"),
                )
                .child(
                    div()
                        .id("unified_mesh_admin_setup_from_group")
                        .bg(rgb(0x303642))
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .text_sm()
                        .text_color(rgb(0xffffff))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _event, _window, cx| {
                                this.install_mesh_admin_setup(cx);
                                cx.notify();
                            }),
                        )
                        .child("Repair Mesh"),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(message_color)
                .child(snapshot.message),
        );
    if let Some(text) = restart_text {
        card = card.child(div().text_xs().text_color(rgb(0xe0af68)).child(text));
    }
    card
}

fn device_row(
    device: &AppDevice,
    active_session: Option<&crate::RoleSession>,
    owner: Arc<crate::UnifiedServiceOwner>,
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
    let is_active = active_session
        .map(|session| session.peer.device_id == device.device_id)
        .unwrap_or(false);
    let status_color = if is_active {
        rgb(0x9ece6a)
    } else if device.is_streamable() {
        rgb(0x7aa2f7)
    } else if device.online {
        rgb(0xe0af68)
    } else {
        rgb(0x565f6b)
    };
    let button_label = if is_active { "Active" } else { "Connect" };
    let mut action = div()
        .px_3()
        .py_2()
        .rounded_sm()
        .text_sm()
        .text_color(rgb(0xffffff))
        .bg(if device.is_streamable() && !is_active {
            rgb(0x2563eb)
        } else {
            rgb(0x303642)
        })
        .child(button_label);

    if device.is_streamable() && !is_active {
        let device_id = device.device_id.clone();
        action = action.cursor_pointer().on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _event, _window, cx| {
                this.status = format!("Connecting to {}", device_id);
                let owner = owner.clone();
                let device_id = device_id.clone();
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
            }),
        );
    }

    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .p_3()
        .bg(rgb(0x1d222b))
        .border_1()
        .border_color(rgb(0x303642))
        .rounded_sm()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .min_w_0()
                .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(status_color))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xf4f7fb))
                                .truncate()
                                .child(device.display_name.clone()),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(rgb(0x9aa6b2))
                                .truncate()
                                .child(device_row_status(device, is_active)),
                        ),
                ),
        )
        .child(action)
}

fn session_panel(
    active_session: Option<&crate::RoleSession>,
    current_frame: Option<&MacDecodedVideoFrame>,
    owner: Arc<crate::UnifiedServiceOwner>,
    viewer_media_status: &UnifiedViewerMediaStatus,
    status: &str,
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
    let panel = div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .bg(rgb(0x161a21))
        .border_1()
        .border_color(rgb(0x2a3038))
        .rounded_sm()
        .overflow_hidden()
        .child(panel_header("Session", status));

    let body = if let Some(session) = active_session {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(video_view(current_frame, "Waiting for video"))
            .child(metric_row("Peer", &session.peer.display_name))
            .child(metric_row("Endpoint", &session.peer.endpoint.to_string()))
            .child(metric_row("Session", &session.session_id.to_string()))
            .child(metric_row(
                "Media",
                &viewer_media_status_detail(viewer_media_status),
            ))
            .child(metric_row(
                "Activity",
                &format!("{} ms", session.last_activity_ms),
            ))
            .child(
                div().mt_2().flex().gap_2().child(
                    div()
                        .id("disconnect_active_session")
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .bg(rgb(0xbe3455))
                        .text_sm()
                        .text_color(rgb(0xffffff))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _event, _window, cx| {
                                this.status = "Disconnecting".to_string();
                                let owner = owner.clone();
                                cx.background_executor()
                                    .spawn(async move {
                                        let _ = owner.disconnect_active().await;
                                    })
                                    .detach();
                                cx.notify();
                            }),
                        )
                        .child("Disconnect"),
                ),
            )
    } else {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(video_view(None, "No active stream"))
            .child(metric_row("Role", "Idle"))
            .child(metric_row(
                "Viewer",
                &viewer_media_status_detail(viewer_media_status),
            ))
            .child(metric_row("Streamer", "Available"))
            .child(empty_state("Select an online device"))
    };

    panel.child(body)
}

fn panel_header(title: &'static str, detail: &str) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_3()
        .bg(rgb(0x1b2028))
        .border_b_1()
        .border_color(rgb(0x2a3038))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(0xf4f7fb))
                .child(title),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(0x9aa6b2))
                .truncate()
                .child(detail.to_string()),
        )
}

fn metric_row(label: &'static str, value: &str) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .py_2()
        .border_b_1()
        .border_color(rgb(0x252b35))
        .child(div().text_xs().text_color(rgb(0x8792a2)).child(label))
        .child(
            div()
                .text_sm()
                .text_color(rgb(0xe8edf4))
                .truncate()
                .child(value.to_string()),
        )
}

fn video_view(frame: Option<&MacDecodedVideoFrame>, fallback: &'static str) -> Div {
    let content = if let Some(frame) = frame {
        decoded_video_frame_surface(frame)
    } else {
        div()
            .w_full()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0x0b0d11))
            .child(div().text_sm().text_color(rgb(0x8792a2)).child(fallback))
            .into_any_element()
    };

    div()
        .w_full()
        .h(px(360.0))
        .bg(rgb(0x0b0d11))
        .border_1()
        .border_color(rgb(0x303642))
        .rounded_sm()
        .overflow_hidden()
        .child(content)
}

fn status_pill(label: &str, color: Rgba) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_sm()
        .bg(rgb(0x202631))
        .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(color))
        .child(
            div()
                .text_xs()
                .text_color(rgb(0xc7d0dc))
                .child(label.to_string()),
        )
}

fn mesh_admin_setup_button(
    needs_admin_setup: bool,
    cx: &mut Context<UnifiedDashboard>,
) -> impl IntoElement {
    div()
        .id("unified_mesh_admin_setup")
        .px_3()
        .py_1()
        .rounded_sm()
        .bg(rgb(0x2563eb))
        .text_xs()
        .text_color(rgb(0xffffff))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _event, _window, cx| {
                this.install_mesh_admin_setup(cx);
                cx.notify();
            }),
        )
        .child(mesh_admin_setup_label(needs_admin_setup))
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

fn empty_state(text: &'static str) -> Div {
    div()
        .p_4()
        .rounded_sm()
        .bg(rgb(0x1d222b))
        .border_1()
        .border_color(rgb(0x303642))
        .text_sm()
        .text_color(rgb(0x9aa6b2))
        .child(text)
}

fn role_status(role: &RoleState) -> (&'static str, Rgba) {
    match role {
        RoleState::Idle => ("Idle", rgb(0x9aa6b2)),
        RoleState::Connecting(_) => ("Connecting", rgb(0xe0af68)),
        RoleState::Viewing(_) => ("Viewing", rgb(0x9ece6a)),
        RoleState::Serving(_) => ("Serving", rgb(0x7aa2f7)),
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

fn pairing_message_color(kind: MeshPairingMessageKind) -> Rgba {
    match kind {
        MeshPairingMessageKind::Neutral => rgb(0x9aa6b2),
        MeshPairingMessageKind::Success => rgb(0x9ece6a),
        MeshPairingMessageKind::Warning => rgb(0xe0af68),
        MeshPairingMessageKind::Error => rgb(0xf7768e),
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

fn mesh_status_color(snapshot: &EasyTierHealthSnapshot) -> Rgba {
    match snapshot.state {
        EasyTierHealthState::Ready => rgb(0x9ece6a),
        EasyTierHealthState::Starting => rgb(0xe0af68),
        EasyTierHealthState::Degraded => rgb(0xf7768e),
        EasyTierHealthState::Stopped => rgb(0x565f6b),
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

fn viewer_media_status_color(status: &UnifiedViewerMediaStatus) -> Rgba {
    match status {
        UnifiedViewerMediaStatus::Ready => rgb(0x9ece6a),
        UnifiedViewerMediaStatus::AudioUnavailable { .. } => rgb(0xe0af68),
        UnifiedViewerMediaStatus::Disabled | UnifiedViewerMediaStatus::Unavailable { .. } => {
            rgb(0x565f6b)
        }
    }
}
