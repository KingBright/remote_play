//! A bounded native window for the production recovery controls. No network,
//! profile, media, device permissions or input forwarding is constructed.
#[cfg(target_os = "macos")]
use remote_play_app::product_components;
#[cfg(target_os = "macos")]
#[path = "../src/design_system.rs"]
mod design_system;
#[cfg(target_os = "macos")]
#[path = "../src/desktop/session_recovery.rs"]
mod session_recovery;

#[cfg(target_os = "macos")]
mod native {
    use crate::{design_system, session_recovery::ConnectionRecoveryControls};
    use ely_gpui_component::{buttons::Button, primitives::FocusScope};
    use gpui::{prelude::*, *};
    use remote_core::session_tabs::{SessionPeer, SessionTabsAction, SessionTabsState};
    use remote_play_app::product_components::{self, assets::UiAsset, theme::ActiveTheme};
    use std::{path::PathBuf, time::Duration};

    struct Smoke {
        root: FocusHandle,
        state: SessionTabsState,
        stale: Option<(String, u64)>,
        events: Vec<String>,
        renders: u64,
        output: PathBuf,
    }
    impl Smoke {
        fn begin(&mut self, key: &str) {
            let plan = self.state.plan_open(
                SessionPeer {
                    device_id: key.into(),
                    name: format!("Device {key}"),
                    endpoint: "127.0.0.1:1".parse().unwrap(),
                },
                None,
                0,
            );
            self.events.push(format!("begin:{key}:{:?}", plan.decision));
        }
        fn fail(&mut self) {
            if let Some(current) = self.state.connecting().cloned() {
                let completion =
                    self.state
                        .complete(&current.peer.device_id, current.attempt, false, 0);
                self.events.push(format!(
                    "failure:{}:{}:{completion:?}",
                    current.peer.device_id, current.attempt
                ));
            }
        }
        fn action(&mut self, action: SessionTabsAction) {
            if self.state.project([]).effect(action).is_none() {
                return;
            }
            self.events.push(format!("control:{action:?}"));
            match action {
                SessionTabsAction::Reconnect => {
                    let key = self.state.failed().unwrap().device_id.clone();
                    self.begin(&key);
                }
                SessionTabsAction::Disconnect => {
                    self.state.disconnect(0);
                }
                _ => {}
            }
        }
        fn save(&self) {
            let current = self
                .state
                .connecting()
                .map(|v| serde_json::json!({"device":v.peer.device_id,"attempt":v.attempt}));
            let failed = self
                .state
                .failed()
                .map(|v| serde_json::json!({"device":v.device_id,"attempt":v.attempt}));
            let data = serde_json::json!({"pid":std::process::id(),"render_calls":self.renders,
                "connecting":current,"failed":failed,"events":self.events,
                "can_retry":self.state.project([]).can_reconnect,"network_runtime":false,"profile_opened":false});
            let _ = std::fs::write(
                self.output.join("state.json"),
                serde_json::to_vec_pretty(&data).unwrap(),
            );
        }
    }
    impl Render for Smoke {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.renders += 1;
            self.save();
            FocusScope::new(&self.root).root().child(
                div()
                    .size_full()
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .bg(cx.theme().surface.canvas)
                    .text_color(cx.theme().content.primary)
                    .child("RemotePlay · production recovery controls")
                    .child("Offline fixture · no remote connection or capture")
                    .child(ConnectionRecoveryControls::new(self.state.project([]), {
                        let view = cx.weak_entity();
                        move |action, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.action(action);
                                cx.notify();
                            });
                        }
                    }))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(Button::new("fail", "Fail current").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.fail();
                                    cx.notify();
                                },
                            )))
                            .child(Button::new("choose_b", "Choose B").on_click(cx.listener(
                                |this, _, _, cx| {
                                    if let Some(v) = this.state.connecting() {
                                        this.stale = Some((v.peer.device_id.clone(), v.attempt));
                                    }
                                    this.begin("B");
                                    cx.notify();
                                },
                            )))
                            .child(Button::new("late", "Deliver old failure").on_click(
                                cx.listener(|this, _, _, cx| {
                                    if let Some((key, attempt)) = this.stale.take() {
                                        let result = this.state.complete(&key, attempt, false, 0);
                                        this.events.push(format!(
                                            "old-failure:{key}:{attempt}:{result:?}"
                                        ));
                                    }
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(format!("Events: {}", self.events.join(" · ")))
                    .child(Button::new("quit", "Finish validation").on_click(|_, _, cx| cx.quit())),
            )
        }
    }
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let output = PathBuf::from("/tmp/remoteplay-session-recovery-native");
        std::fs::create_dir_all(&output)?;
        Application::new()
            .with_assets(UiAsset)
            .run(move |cx: &mut App| {
                product_components::component::init(cx).expect("production Ely fonts/assets");
                product_components::theme::install(
                    WindowAppearance::Dark,
                    design_system::remote_play_themes(),
                    cx,
                );
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds(
                            point(px(90.), px(80.)),
                            size(px(760.), px(490.)),
                        ))),
                        titlebar: Some(TitlebarOptions {
                            title: Some("RemotePlay connection recovery offline validation".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    move |window, cx| {
                        cx.new(|cx| {
                            let root = cx.focus_handle();
                            window.focus(&root);
                            let mut smoke = Smoke {
                                root,
                                state: SessionTabsState::default(),
                                stale: None,
                                events: vec![],
                                renders: 0,
                                output,
                            };
                            smoke.begin("A");
                            smoke.fail();
                            smoke
                        })
                    },
                )
                .expect("bounded native fixture window");
                cx.activate(true);
                cx.spawn(async move |cx| {
                    Timer::after(Duration::from_secs(180)).await;
                    let _ = cx.update(|cx| cx.quit());
                })
                .detach();
            });
        Ok(())
    }
}
#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    native::run()
}
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Native recovery fixture currently supports macOS only");
    std::process::exit(1);
}
