use crate::design_system::remote_play_themes;
use crate::mesh_admin::{mesh_setup_was_cancelled, run_mesh_admin_setup};
use crate::{
    AppDevice, MacDecodedVideoFrame, MeshPairingControl, MeshPairingMessageKind,
    MeshPairingSnapshot, RoleState, StreamStartOptions, UnifiedRuntimeConfig, UnifiedRuntimeHandle,
    UnifiedViewerMediaStatus, decoded_video_frame_surface, start_unified_runtime,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use remote_core::mesh::{EasyTierHealthIssue, EasyTierHealthSnapshot, EasyTierHealthState};
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_latest_frame();
        let snapshot = self.snapshot();
        let role = snapshot.role.clone();
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

fn session_panel(
    role: &RoleState,
    active_session: Option<&crate::RoleSession>,
    current_frame: Option<&MacDecodedVideoFrame>,
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
            .child(session_stage(role, current_frame, cx))
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
            .child(session_stage(role, None, cx))
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
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
    match role {
        RoleState::Viewing(_) | RoleState::Connecting(_) => video_view(
            frame,
            if matches!(role, RoleState::Connecting(_)) {
                "Establishing secure session"
            } else {
                "Waiting for video"
            },
            cx,
        ),
        RoleState::Serving(session) => serving_stage(&session.peer.display_name, cx),
        RoleState::Idle => idle_stage(cx),
    }
}

fn video_view(
    frame: Option<&MacDecodedVideoFrame>,
    fallback: &'static str,
    cx: &mut Context<UnifiedDashboard>,
) -> Div {
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

    div()
        .w_full()
        .flex_1()
        .min_h(px(280.0))
        .bg(rgb(0x050706))
        .border_1()
        .border_color(theme.border.default)
        .rounded(px(7.0))
        .overflow_hidden()
        .shadow_sm()
        .child(content)
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
