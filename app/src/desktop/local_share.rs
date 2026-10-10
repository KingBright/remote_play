//! Scalar sharing view model and existing Ely controls. Portal capabilities,
//! cancellation and worker lifetimes belong to the Linux owner below.
use crate::product_components::theme::ActiveTheme;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    theme::ControlSize,
};
use gpui::{App, IntoElement, RenderOnce, Window, div, prelude::*, px};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShareKind {
    Window,
    Display,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetRow {
    pub key: String,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Refresh,
    Select(String, ShareKind),
    Cancel,
    Stop(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    Refresh(u64),
    Select {
        attempt: u64,
        key: String,
        kind: ShareKind,
    },
    Cancel(u64),
    Stop(String),
}
#[derive(Clone, Default)]
pub(crate) struct State {
    serial: u64,
    refresh: Option<u64>,
    pub rows: Vec<TargetRow>,
    pending: Option<(u64, String)>,
    message: Option<String>,
}
impl State {
    pub fn reduce(&mut self, action: Action) -> Option<Effect> {
        match action {
            Action::Refresh if self.pending.is_none() => {
                self.serial += 1;
                self.refresh = Some(self.serial);
                Some(Effect::Refresh(self.serial))
            }
            Action::Select(key, kind)
                if self.pending.is_none() && self.rows.iter().any(|row| row.key == key) =>
            {
                self.serial += 1;
                self.refresh = None;
                self.pending = Some((self.serial, key.clone()));
                self.message = None;
                Some(Effect::Select {
                    attempt: self.serial,
                    key,
                    kind,
                })
            }
            Action::Cancel => self
                .pending
                .take()
                .map(|(attempt, _)| Effect::Cancel(attempt)),
            Action::Stop(key) => Some(Effect::Stop(key)),
            _ => None,
        }
    }
    pub fn refreshed(&mut self, receipt: u64, rows: Vec<TargetRow>) -> bool {
        if self.refresh != Some(receipt) {
            return false;
        }
        self.refresh = None;
        self.rows = rows;
        true
    }
    pub fn refresh_failed(&mut self, receipt: u64, message: String) -> bool {
        if self.refresh != Some(receipt) {
            return false;
        }
        self.refresh = None;
        self.message = Some(message);
        true
    }
    pub fn completed(&mut self, receipt: u64, result: Result<(), String>) -> bool {
        if self.pending.as_ref().map(|(id, _)| *id) != Some(receipt) {
            return false;
        }
        self.pending = None;
        self.message = result.err();
        true
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
}

#[derive(IntoElement)]
pub(crate) struct Controls {
    state: State,
    active: Vec<String>,
    send: Rc<dyn Fn(Action, &mut App)>,
}
impl Controls {
    pub fn new(
        state: State,
        active: Vec<String>,
        send: impl Fn(Action, &mut App) + 'static,
    ) -> Self {
        Self {
            state,
            active,
            send: Rc::new(send),
        }
    }
}
impl RenderOnce for Controls {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let pending = self.state.pending();
        let refresh = self.send.clone();
        let cancel = self.send.clone();
        let mut content=div().flex().flex_col().gap_2().p_3().rounded(px(8.0))
            .bg(cx.theme().surface.sunken).text_size(px(11.0)).text_color(cx.theme().content.primary)
            .child("Share this computer")
            .child(div().text_color(cx.theme().content.tertiary).child("Choose a connected viewer, then a window or display. The viewer selects the shared source in Sources."))
            .child(Button::new("share_refresh","Refresh viewers").debug_selector(||"share-refresh".into())
                .control_size(ControlSize::Sm).variant(ButtonVariant::Secondary).rounded_full().h(px(28.0)).disabled(pending)
                .on_click(move |_,_,cx|refresh(Action::Refresh,cx)));
        if pending {
            content = content.child("Waiting for your selection…").child(
                Button::new("share_cancel", "Cancel selection")
                    .debug_selector(|| "share-cancel".into())
                    .control_size(ControlSize::Sm)
                    .rounded_full()
                    .on_click(move |_, _, cx| cancel(Action::Cancel, cx)),
            );
        }
        if self.state.rows.is_empty() {
            content =
                content.child("No connected viewers. Connect from another device, then refresh.");
        }
        for row in self.state.rows {
            let active = self.active.contains(&row.key);
            let window = self.send.clone();
            let display = self.send.clone();
            let stop = self.send.clone();
            let window_key = row.key.clone();
            let display_key = row.key.clone();
            let stop_key = row.key;
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(row.label)
                    .when(active, |row| row.child("Shared source available"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new(format!("share_window_{window_key}"), "Window")
                                    .debug_selector({
                                        let key = window_key.clone();
                                        move || format!("share-window-{key}")
                                    })
                                    .control_size(ControlSize::Sm)
                                    .rounded_full()
                                    .h(px(28.0))
                                    .disabled(pending)
                                    .on_click(move |_, _, cx| {
                                        window(
                                            Action::Select(window_key.clone(), ShareKind::Window),
                                            cx,
                                        )
                                    }),
                            )
                            .child(
                                Button::new(format!("share_display_{display_key}"), "Display")
                                    .control_size(ControlSize::Sm)
                                    .rounded_full()
                                    .h(px(28.0))
                                    .disabled(pending)
                                    .on_click(move |_, _, cx| {
                                        display(
                                            Action::Select(display_key.clone(), ShareKind::Display),
                                            cx,
                                        )
                                    }),
                            )
                            .when(active, |row| {
                                row.child(
                                    Button::new(format!("share_stop_{stop_key}"), "Stop")
                                        .control_size(ControlSize::Sm)
                                        .rounded_full()
                                        .h(px(28.0))
                                        .on_click(move |_, _, cx| {
                                            stop(Action::Stop(stop_key.clone()), cx)
                                        }),
                                )
                            }),
                    ),
            );
        }
        if let Some(message) = self.state.message {
            content = content.child(message);
        }
        content
    }
}

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(test)]
mod tests;
