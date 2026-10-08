use super::device_list::{
    DeviceConnectionStatus, DeviceFilterKind, DeviceListAction, DeviceListViewModel,
};
use crate::design_system::{color_accent_cyan, color_accent_emerald, color_border_fine};
use crate::product_components::theme::ActiveTheme;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    theme::ControlSize,
};
use gpui::{App, FontWeight, IntoElement, RenderOnce, Window, div, prelude::*, px, rgb};
use remote_core::discovery::DiscoveryScope;
use std::rc::Rc;

type ActionSink = Rc<dyn Fn(DeviceListAction, &mut App)>;

#[derive(IntoElement)]
pub(crate) struct DeviceDrawer {
    model: DeviceListViewModel,
    filter: DeviceFilterKind,
    send: ActionSink,
}

impl DeviceDrawer {
    pub(crate) fn new(
        model: DeviceListViewModel,
        filter: DeviceFilterKind,
        send: impl Fn(DeviceListAction, &mut App) + 'static,
    ) -> Self {
        Self {
            model,
            filter,
            send: Rc::new(send),
        }
    }
}

impl RenderOnce for DeviceDrawer {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let mut filters = div().flex().gap_1();
        for (label, filter) in [
            ("All", DeviceFilterKind::All),
            ("LAN", DeviceFilterKind::Lan),
            ("P2P", DeviceFilterKind::P2p),
            ("Relay", DeviceFilterKind::Relay),
        ] {
            let send = self.send.clone();
            filters = filters.child(
                Button::new(format!("filter_{label}"), label)
                    .debug_selector(move || format!("filter-{label}"))
                    .variant(if filter == self.filter {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Secondary
                    })
                    .control_size(ControlSize::Sm)
                    .h(px(24.0))
                    .flex_1()
                    .text_size(px(9.0))
                    .on_click(move |_, _, cx| send(DeviceListAction::SelectFilter(filter), cx)),
            );
        }
        let mut content = div().flex().flex_col().gap_3().child(filters);
        if self.model.rows.is_empty() {
            return content.child(
                div()
                    .p_4()
                    .rounded_md()
                    .bg(theme.surface.sunken)
                    .text_color(theme.content.tertiary)
                    .text_size(px(11.0))
                    .child("No devices in this filter. Join a device group or choose All."),
            );
        }
        for row in self.model.rows {
            let scope = match row.scope {
                DiscoveryScope::Lan => "LAN",
                DiscoveryScope::P2p => "P2P Direct",
                DiscoveryScope::Relay => "Relay",
                DiscoveryScope::Mesh => "Legacy Route",
            };
            let connect_label = match row.connection {
                DeviceConnectionStatus::Connecting => "Connecting…",
                DeviceConnectionStatus::Viewing => "Active Viewport",
                DeviceConnectionStatus::Serving => "Serving",
                _ => "Connect Stream",
            };
            let stream_id = row.device_id.clone();
            let files_id = row.device_id.clone();
            let workspace_id = row.device_id.clone();
            let stream = self.send.clone();
            let files = self.send.clone();
            let workspace = self.send.clone();
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded(px(8.0))
                    .shadow_xs()
                    .bg(if row.is_active() {
                        rgb(0x192830).into()
                    } else {
                        theme.surface.raised
                    })
                    .border_1()
                    .border_color(if row.is_active() {
                        color_accent_cyan()
                    } else {
                        color_border_fine()
                    })
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
                                    .child(div().size(px(8.0)).rounded_full().bg(if row.online {
                                        color_accent_emerald().into()
                                    } else {
                                        theme.content.disabled
                                    }))
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.content.primary)
                                            .truncate()
                                            .child(row.display_name),
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
                                    .child(scope),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(10.0))
                            .text_color(theme.content.tertiary)
                            .child(format!("Endpoint: {}", row.endpoint))
                            .child(row.connection.label()),
                    )
                    .child(
                        Button::new(format!("drawer_connect_{stream_id}"), connect_label)
                            .debug_selector({
                                let id = stream_id.clone();
                                move || format!("connect-{id}")
                            })
                            .control_size(ControlSize::Sm)
                            .full_width()
                            .variant(if row.can_connect {
                                ButtonVariant::Primary
                            } else {
                                ButtonVariant::Secondary
                            })
                            .disabled(!row.can_connect)
                            .h(px(28.0))
                            .text_size(px(10.0))
                            .on_click(move |_, _, cx| {
                                stream(DeviceListAction::Connect(stream_id.clone()), cx)
                            }),
                    )
                    .child(
                        Button::new(format!("drawer_files_{files_id}"), "Files")
                            .debug_selector({
                                let id = files_id.clone();
                                move || format!("files-{id}")
                            })
                            .control_size(ControlSize::Sm)
                            .full_width()
                            .disabled(!row.can_open_files)
                            .on_click(move |_, _, cx| {
                                files(DeviceListAction::OpenFiles(files_id.clone()), cx)
                            }),
                    )
                    .child(
                        Button::new(
                            format!("workspace-{workspace_id}"),
                            "Open multi-window workspace",
                        )
                        .control_size(ControlSize::Sm)
                        .full_width()
                        .disabled(!row.can_open_workspace)
                        .on_click(move |_, _, cx| {
                            workspace(DeviceListAction::OpenWorkspace(workspace_id.clone()), cx)
                        }),
                    ),
            );
        }
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppDevice;
    use ely_gpui_component::primitives::FocusScope;
    use gpui::{Context, FocusHandle, Modifiers, TestAppContext, WindowAppearance};
    use remote_core::role::{RoleSession, RoleState};
    use std::cell::RefCell;

    struct DrawerFixture {
        root: FocusHandle,
        model: DeviceListViewModel,
        output: Rc<RefCell<Vec<DeviceListAction>>>,
    }
    impl Render for DrawerFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let output = self.output.clone();
            FocusScope::new(&self.root)
                .root()
                .w(px(360.0))
                .child(DeviceDrawer::new(
                    self.model.clone(),
                    DeviceFilterKind::All,
                    move |action, _| output.borrow_mut().push(action),
                ))
        }
    }
    fn device(id: &str, online: bool) -> AppDevice {
        AppDevice {
            device_id: id.into(),
            display_name: id.into(),
            endpoint: "127.0.0.1:9000".parse().unwrap(),
            scope: DiscoveryScope::Lan,
            can_stream: true,
            can_view: true,
            online,
            last_seen_ms: 0,
        }
    }

    #[gpui::test]
    fn drawer_disabled_and_reordered_controls_keep_device_targets(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::product_components::component::init(cx).unwrap();
            crate::product_components::theme::install(
                WindowAppearance::Dark,
                crate::design_system::remote_play_themes(),
                cx,
            );
        });
        let devices = [
            device("a", true),
            device("b", true),
            device("offline", false),
        ];
        let role = RoleState::Viewing(RoleSession::new(devices[0].role_peer(), 7, 0));
        let model = DeviceListViewModel::project(&devices, &role, DeviceFilterKind::All);
        let output = Rc::new(RefCell::new(Vec::new()));
        let (view, cx) = cx.add_window_view({
            let output = output.clone();
            move |_, cx| DrawerFixture {
                root: cx.focus_handle(),
                model,
                output,
            }
        });
        cx.run_until_parked();
        for selector in ["connect-a", "connect-offline", "files-offline"] {
            let bounds = cx.debug_bounds(selector).expect("device control rendered");
            cx.simulate_click(bounds.center(), Modifiers::none());
        }
        assert!(
            output.borrow().is_empty(),
            "disabled controls must not dispatch effects"
        );
        let bounds = cx.debug_bounds("connect-b").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        assert_eq!(*output.borrow(), [DeviceListAction::Connect("b".into())]);
        output.borrow_mut().clear();
        let role = RoleState::Connecting(RoleSession::new(devices[1].role_peer(), 8, 1));
        let reversed = [devices[1].clone(), devices[0].clone(), devices[2].clone()];
        view.update(cx, |view, cx| {
            view.model = DeviceListViewModel::project(&reversed, &role, DeviceFilterKind::All);
            cx.notify();
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds("connect-b").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        assert!(output.borrow().is_empty());
        let bounds = cx.debug_bounds("connect-a").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        let bounds = cx.debug_bounds("files-b").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        assert_eq!(
            *output.borrow(),
            [
                DeviceListAction::Connect("a".into()),
                DeviceListAction::OpenFiles("b".into())
            ]
        );
    }

    struct FocusFixture {
        root: FocusHandle,
        first: FocusHandle,
        disabled: FocusHandle,
        last: FocusHandle,
    }
    impl Render for FocusFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            FocusScope::new(&self.root).root().child(
                div()
                    .flex()
                    .flex_col()
                    .child(Button::new("first", "First").focus_handle(&self.first))
                    .child(
                        Button::new("disabled", "Disabled")
                            .focus_handle(&self.disabled)
                            .disabled(true),
                    )
                    .child(Button::new("last", "Last").focus_handle(&self.last)),
            )
        }
    }
    #[gpui::test]
    fn ely_tab_scope_skips_disabled_controls_in_both_directions(cx: &mut TestAppContext) {
        cx.update(|cx| crate::product_components::component::init(cx).unwrap());
        let (view, cx) = cx.add_window_view(|window, cx| {
            let first = cx.focus_handle().tab_stop(true);
            let root = cx.focus_handle();
            window.focus(&root);
            FocusFixture {
                root,
                first,
                disabled: cx.focus_handle().tab_stop(true),
                last: cx.focus_handle().tab_stop(true),
            }
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| assert!(view.read(cx).first.is_focused(window)));
        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| assert!(view.read(cx).last.is_focused(window)));
        cx.simulate_keystrokes("shift-tab");
        cx.update(|window, cx| assert!(view.read(cx).first.is_focused(window)));
        cx.update(|window, cx| assert!(!view.read(cx).disabled.is_focused(window)));
    }
}
