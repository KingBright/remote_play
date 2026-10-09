//! Recovery controls render scalar session state; the Owner owns all resources.
use crate::product_components::theme::ActiveTheme;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    theme::ControlSize,
};
use gpui::{App, IntoElement, RenderOnce, Window, div, prelude::*, px};
use remote_core::session_tabs::{SessionTabsAction, SessionTabsViewModel};
use std::rc::Rc;

#[derive(IntoElement)]
pub(crate) struct ConnectionRecoveryControls {
    model: SessionTabsViewModel,
    send: Rc<dyn Fn(SessionTabsAction, &mut App)>,
}
impl ConnectionRecoveryControls {
    pub(crate) fn new(
        model: SessionTabsViewModel,
        send: impl Fn(SessionTabsAction, &mut App) + 'static,
    ) -> Self {
        Self {
            model,
            send: Rc::new(send),
        }
    }
}
impl RenderOnce for ConnectionRecoveryControls {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (label, back) = if let Some(failed) = &self.model.failed {
            (
                format!("Could not connect to {}", failed.name),
                "Back to devices",
            )
        } else if let Some(pending) = &self.model.connecting {
            (
                format!("Connecting to {}…", pending.peer.name),
                "Cancel connection",
            )
        } else {
            return div();
        };
        let retry = self.send.clone();
        let cancel = self.send;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(px(8.0))
            .bg(cx.theme().surface.sunken)
            .text_size(px(11.0))
            .text_color(cx.theme().content.primary)
            .child(label)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("connection_retry", "Retry")
                            .debug_selector(|| "connection-retry".into())
                            .variant(ButtonVariant::Primary)
                            .control_size(ControlSize::Sm)
                            .rounded_full()
                            .h(px(28.0))
                            .disabled(!self.model.can_reconnect)
                            .on_click(move |_, _, cx| retry(SessionTabsAction::Reconnect, cx)),
                    )
                    .child(
                        Button::new("connection_back", back)
                            .debug_selector(|| "connection-back".into())
                            .variant(ButtonVariant::Secondary)
                            .control_size(ControlSize::Sm)
                            .rounded_full()
                            .h(px(28.0))
                            .on_click(move |_, _, cx| cancel(SessionTabsAction::Disconnect, cx)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ely_gpui_component::primitives::FocusScope;
    use gpui::{Context, FocusHandle, Modifiers, Render, TestAppContext, WindowAppearance};
    use remote_core::session_tabs::{Completion, OpenDecision, SessionPeer, SessionTabsState};
    use std::cell::RefCell;

    struct Fixture {
        root: FocusHandle,
        model: SessionTabsViewModel,
        output: Rc<RefCell<Vec<SessionTabsAction>>>,
    }
    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let output = self.output.clone();
            FocusScope::new(&self.root)
                .root()
                .child(div().w(px(450.0)).child(ConnectionRecoveryControls::new(
                    self.model.clone(),
                    move |action, _| output.borrow_mut().push(action),
                )))
        }
    }
    fn begin(state: &mut SessionTabsState, key: &str) -> u64 {
        let OpenDecision::Start(attempt) = state
            .plan_open(
                SessionPeer {
                    device_id: key.into(),
                    name: key.into(),
                    endpoint: "127.0.0.1:1".parse().unwrap(),
                },
                None,
                0,
            )
            .decision
        else {
            panic!("fresh attempt expected")
        };
        attempt
    }
    fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::product_components::component::init(cx).unwrap();
            crate::product_components::theme::install(
                WindowAppearance::Dark,
                crate::design_system::remote_play_themes(),
                cx,
            );
        });
    }
    #[gpui::test]
    fn retry_click_pending_disable_cancel_and_back_use_production_controls(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let mut state = SessionTabsState::default();
        let first = begin(&mut state, "a");
        assert_eq!(state.complete("a", first, false, 0), Completion::Selected);
        let output = Rc::new(RefCell::new(vec![]));
        let (view, cx) = cx.add_window_view({
            let output = output.clone();
            let model = state.project([]);
            move |_, cx| Fixture {
                root: cx.focus_handle(),
                model,
                output,
            }
        });
        cx.run_until_parked();
        let center = cx.debug_bounds("connection-retry").unwrap().center();

        cx.simulate_click(center, Modifiers::none());
        assert_eq!(*output.borrow(), [SessionTabsAction::Reconnect]);
        let second = begin(&mut state, "a");
        assert!(second > first);
        view.update(cx, |this, cx| {
            this.model = state.project([]);
            cx.notify();
        });
        cx.run_until_parked();
        output.borrow_mut().clear();
        for _ in 0..2 {
            let center = cx.debug_bounds("connection-retry").unwrap().center();

            cx.simulate_click(center, Modifiers::none());
        }
        assert!(output.borrow().is_empty());
        let center = cx.debug_bounds("connection-back").unwrap().center();

        cx.simulate_click(center, Modifiers::none());
        assert_eq!(*output.borrow(), [SessionTabsAction::Disconnect]);
        state.disconnect(0);
        assert_eq!(state.complete("a", second, false, 0), Completion::Cancelled);
        let third = begin(&mut state, "b");
        state.complete("b", third, false, 0);
        view.update(cx, |this, cx| {
            this.model = state.project([]);
            cx.notify();
        });
        cx.run_until_parked();
        output.borrow_mut().clear();
        let center = cx.debug_bounds("connection-back").unwrap().center();

        cx.simulate_click(center, Modifiers::none());
        assert_eq!(*output.borrow(), [SessionTabsAction::Disconnect]);
        state.disconnect(0);
        view.update(cx, |this, cx| {
            this.model = state.project([]);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(!state.project([]).can_reconnect);
        assert!(state.failed().is_none());
        output.borrow_mut().clear();
        // GPUI retains removed selectors in its debug-bounds map. Exercise the
        // former hit area instead: returning must remove its event handler.
        cx.simulate_click(center, Modifiers::none());
        assert!(output.borrow().is_empty());
    }
    #[gpui::test]
    fn old_failure_does_not_replace_new_device_recovery_target(cx: &mut TestAppContext) {
        init(cx);
        let mut state = SessionTabsState::default();
        let old = begin(&mut state, "a");
        let new = begin(&mut state, "b");
        state.complete("b", new, false, 0);
        assert_eq!(state.complete("a", old, false, 0), Completion::Background);
        assert_eq!(state.failed().unwrap().device_id, "b");
        let output = Rc::new(RefCell::new(vec![]));
        let (_, cx) = cx.add_window_view({
            let output = output.clone();
            let model = state.project([]);
            move |_, cx| Fixture {
                root: cx.focus_handle(),
                model,
                output,
            }
        });
        cx.run_until_parked();
        let center = cx.debug_bounds("connection-retry").unwrap().center();

        cx.simulate_click(center, Modifiers::none());
        assert_eq!(*output.borrow(), [SessionTabsAction::Reconnect]);
        assert_eq!(state.failed().unwrap().attempt, new);
    }
}
