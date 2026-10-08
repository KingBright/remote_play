//! Bounded native drawer smoke. Reuses production source and records actions only.
//! No runtime, profile, mesh, capture, input forwarding, or Owner is constructed.
#[cfg(target_os = "macos")]
use remote_play_app::{AppDevice, product_components};
#[cfg(target_os = "macos")]
#[path = "../src/design_system.rs"]
mod design_system;
#[cfg(target_os = "macos")]
#[path = "../src/desktop/device_drawer.rs"]
mod device_drawer;
#[cfg(target_os = "macos")]
#[path = "../src/desktop/device_list.rs"]
mod device_list;

#[cfg(target_os = "macos")]
mod native {
    use crate::{AppDevice, design_system, device_drawer::DeviceDrawer, device_list::*};
    use ely_gpui_component::{
        buttons::Button,
        primitives::FocusScope,
        theme::{Mode, Theme as ElyTheme},
    };
    use gpui::{prelude::*, *};
    use objc2::{
        Encode, Encoding, msg_send,
        runtime::{AnyClass, AnyObject},
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use remote_core::{discovery::DiscoveryScope, role::RoleState};
    use remote_play_app::product_components::{self, assets::UiAsset, theme::ActiveTheme};
    use serde_json::{Value, json};
    use std::{path::PathBuf, time::Duration};

    struct Smoke {
        root: FocusHandle,
        state: DeviceListState,
        empty: bool,
        selected: Option<String>,
        actions: Vec<String>,
        renders: u64,
        output: PathBuf,
        command_id: u64,
        window_number: i32,
        last_error: Option<String>,
    }
    fn devices() -> Vec<AppDevice> {
        [
            ("alpha", "Studio · test", DiscoveryScope::Lan, true),
            ("beta", "Laptop · test", DiscoveryScope::Relay, true),
            ("offline", "Offline · test", DiscoveryScope::P2p, false),
        ]
        .into_iter()
        .map(|(id, name, scope, online)| AppDevice {
            device_id: id.into(),
            display_name: name.into(),
            scope,
            online,
            endpoint: "127.0.0.1:1".parse().unwrap(),
            can_stream: true,
            can_view: true,
            last_seen_ms: 0,
        })
        .collect()
    }
    fn window_number(window: &Window) -> Result<i32, Box<dyn std::error::Error>> {
        let handle = HasWindowHandle::window_handle(window)?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return Err("not an AppKit window".into());
        };
        // Borrow the current window's own view. Never enumerate other processes/windows.
        let view = handle.ns_view.as_ptr().cast::<AnyObject>();
        let native_window: *mut AnyObject = unsafe { msg_send![view, window] };
        if native_window.is_null() {
            return Err("native window absent".into());
        }
        let number: isize = unsafe { msg_send![native_window, windowNumber] };
        Ok(number as i32)
    }
    #[derive(Clone, Copy)]
    #[repr(C)]
    struct NativePoint {
        x: f64,
        y: f64,
    }
    unsafe impl Encode for NativePoint {
        const ENCODING: Encoding =
            Encoding::Struct("CGPoint", &[Encoding::Double, Encoding::Double]);
    }
    fn post_own_event(
        window: &Window,
        number: i32,
        click: Option<(f64, f64)>,
        key: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if window_number(window)? != number || number <= 0 {
            return Err("own window identity changed".into());
        }
        let cls = AnyClass::get(c"NSEvent").ok_or("NSEvent unavailable")?;
        let app_cls = AnyClass::get(c"NSApplication").ok_or("NSApplication unavailable")?;
        let app: *mut AnyObject = unsafe { msg_send![app_cls, sharedApplication] };
        let nil = std::ptr::null_mut::<AnyObject>();
        if let Some((x, y)) = click {
            if x < 0.
                || y < 0.
                || x >= f64::from(f32::from(window.bounds().size.width))
                || y >= f64::from(f32::from(window.bounds().size.height))
            {
                return Err("point outside own window".into());
            }
            let p = NativePoint {
                x,
                y: f64::from(f32::from(window.bounds().size.height)) - y,
            };
            for kind in [1usize, 2usize] {
                let event: *mut AnyObject = unsafe {
                    msg_send![cls,mouseEventWithType:kind, location:p, modifierFlags:0usize, timestamp:0f64, windowNumber:number as isize, context:nil, eventNumber:1isize, clickCount:1isize, pressure:1f32]
                };
                if event.is_null() {
                    return Err("own mouse event absent".into());
                }
                unsafe {
                    let _: () = msg_send![app,postEvent:event, atStart:false];
                }
            }
        } else {
            let flags = match key {
                Some("tab") => 0usize,
                Some("shift-tab") => 1usize << 17,
                _ => return Err("only Tab/Shift-Tab permitted".into()),
            };
            let text_cls = AnyClass::get(c"NSString").ok_or("NSString unavailable")?;
            let text: *mut AnyObject =
                unsafe { msg_send![text_cls,stringWithUTF8String:c"\t".as_ptr()] };
            for kind in [10usize, 11usize] {
                let event: *mut AnyObject = unsafe {
                    msg_send![cls,keyEventWithType:kind, location:NativePoint{x:0.,y:0.}, modifierFlags:flags, timestamp:0f64, windowNumber:number as isize, context:nil, characters:text, charactersIgnoringModifiers:text, isARepeat:false, keyCode:48u16]
                };
                if event.is_null() {
                    return Err("own key event absent".into());
                }
                unsafe {
                    let _: () = msg_send![app,postEvent:event, atStart:false];
                }
            }
        }
        Ok(())
    }
    impl Smoke {
        fn save(&self, window: &Window, cx: &App) {
            let gpu = window.gpu_specs().map(
                |g| json!({"device":g.device_name,"software_emulated":g.is_software_emulated}),
            );
            let rows = DeviceListModel {
                devices: &if self.empty { vec![] } else { devices() },
                role: &RoleState::Idle,
            }
            .project(self.state.filter);
            let data = json!({"pid":std::process::id(),"window_number":self.window_number,"title":"RemotePlay Ely drawer offline smoke", "render_calls":self.renders,
                "command_id":self.command_id,"mode":format!("{:?}",ElyTheme::global(cx).mode()),"empty":self.empty,"filter":format!("{:?}",self.state.filter),
                "rows":rows.rows.iter().map(|r|json!({"id":r.device_id,"can_connect":r.can_connect,"can_open_files":r.can_open_files})).collect::<Vec<_>>(),
                "selected":self.selected,"actions":self.actions,"focused":window.focused(cx).map(|f|format!("{f:?}")),"scale_factor":window.scale_factor(),"bounds":format!("{:?}",window.bounds()),"gpu":gpu,"last_error":self.last_error,
                "network_started":false,"owner_created":false,"capture_started":false,"input_forwarded":false,"event_route":"own NSApplication event queue","installed_app_changed":false});
            let tmp = self.output.join("state.tmp");
            if std::fs::write(&tmp, serde_json::to_vec_pretty(&data).unwrap())
                .and_then(|_| std::fs::rename(tmp, self.output.join("state.json")))
                .is_err()
            {
                eprintln!("smoke state write failed");
            }
        }
        fn select(&mut self, action: DeviceListAction, cx: &mut Context<Self>) {
            self.actions.push(format!("{action:?}"));
            if let Some(effect) = self.state.reduce(action) {
                self.selected = Some(format!("{effect:?}"));
            }
            cx.notify();
        }
        fn command(&mut self, command: Value, window: &mut Window, cx: &mut Context<Self>) {
            let id = command["id"].as_u64().unwrap_or(0);
            if id <= self.command_id {
                return;
            }
            self.command_id = id;
            match command["action"].as_str().unwrap_or("") {
                "empty" => {
                    self.empty = true;
                    self.selected = None;
                    self.state.filter = DeviceFilterKind::All;
                    cx.notify();
                }
                "multiple" => {
                    self.empty = false;
                    self.selected = None;
                    self.state.filter = DeviceFilterKind::All;
                    cx.notify();
                }
                "dark" => {
                    ElyTheme::set_mode_now(Mode::Dark, cx);
                    cx.notify();
                }
                "light" => {
                    ElyTheme::set_mode_now(Mode::Light, cx);
                    cx.notify();
                }
                "key" => {
                    if let Err(error) =
                        post_own_event(window, self.window_number, None, command["key"].as_str())
                    {
                        self.last_error = Some(error.to_string());
                    }
                }
                "click" => {
                    let point = (
                        command["x"].as_f64().unwrap_or(-1.),
                        command["y"].as_f64().unwrap_or(-1.),
                    );
                    if let Err(error) =
                        post_own_event(window, self.window_number, Some(point), None)
                    {
                        self.last_error = Some(error.to_string());
                    }
                }
                "quit" => cx.quit(),
                _ => self.last_error = Some("unsupported smoke command".into()),
            }
            self.save(window, cx);
        }
    }
    impl Render for Smoke {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.renders += 1;
            let theme = cx.theme();
            let weak = cx.weak_entity();
            let devices = if self.empty { vec![] } else { devices() };
            let model = DeviceListModel {
                devices: &devices,
                role: &RoleState::Idle,
            }
            .project(self.state.filter);
            let mut toolbar = div().flex().gap_2();
            for (id, label) in [
                ("empty", "Empty"),
                ("multiple", "Multiple"),
                ("dark", "Dark"),
                ("light", "Light"),
            ] {
                toolbar = toolbar.child(Button::new(id, label).on_click(cx.listener(
                    move |this, _, _, cx| {
                        match id {
                            "empty" => {
                                this.empty = true;
                                this.selected = None;
                            }
                            "multiple" => {
                                this.empty = false;
                                this.selected = None;
                            }
                            "dark" => ElyTheme::set_mode_now(Mode::Dark, cx),
                            "light" => ElyTheme::set_mode_now(Mode::Light, cx),
                            _ => {}
                        }
                        this.state.filter = DeviceFilterKind::All;
                        cx.notify();
                    },
                )));
            }
            FocusScope::new(&self.root).root().size_full().child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .bg(theme.surface.canvas)
                    .text_color(theme.content.primary)
                    .font_family(design_system::product_ui_font())
                    .child(
                        div()
                            .text_size(px(18.))
                            .child("Device drawer · offline native smoke"),
                    )
                    .child(
                        div().text_size(px(11.)).child(
                            "Test data only. Actions are recorded; no connection is executed.",
                        ),
                    )
                    .child(toolbar)
                    .child(
                        div()
                            .id("drawer-scroll")
                            .flex_1()
                            .overflow_y_scroll()
                            .child(div().w(px(360.)).child(DeviceDrawer::new(
                                model,
                                self.state.filter,
                                move |action, cx| {
                                    let _ = weak.update(cx, |this, cx| this.select(action, cx));
                                },
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                self.selected
                                    .clone()
                                    .unwrap_or_else(|| "No selection".into()),
                            )
                            .child(
                                Button::new("cancel", "Cancel / return")
                                    .disabled(self.selected.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selected = None;
                                        this.actions.push("Cancel".into());
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
        }
    }
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let output = PathBuf::from(
            std::env::args()
                .nth(1)
                .ok_or("fresh absolute output directory required")?,
        );
        if !output.is_absolute() || output.exists() {
            return Err("output must be fresh and absolute".into());
        }
        std::fs::create_dir(&output)?;
        // Bound even a native-service stall before the event loop/timer starts.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(185));
            eprintln!("offline smoke watchdog expired");
            std::process::exit(2);
        });
        Application::new()
            .with_assets(UiAsset)
            .run(move |cx: &mut App| {
                product_components::component::init(cx).expect("real Ely assets/fonts");
                product_components::theme::install(
                    WindowAppearance::Dark,
                    design_system::remote_play_themes(),
                    cx,
                );
                let view = cx
                    .open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(bounds(
                                point(px(90.), px(60.)),
                                size(px(620.), px(800.)),
                            ))),
                            titlebar: Some(TitlebarOptions {
                                title: Some("RemotePlay Ely drawer offline smoke".into()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        move |window, cx| {
                            cx.new(|cx| {
                                let root = cx.focus_handle();
                                window.focus(&root);
                                Smoke {
                                    root,
                                    state: DeviceListState::default(),
                                    empty: false,
                                    selected: None,
                                    actions: vec![],
                                    renders: 0,
                                    output,
                                    command_id: 0,
                                    window_number: window_number(window)
                                        .expect("own native window"),
                                    last_error: None,
                                }
                            })
                        },
                    )
                    .expect("native smoke window");
                cx.activate(true);
                cx.spawn(async move |cx| {
                    for _ in 0..900 {
                        Timer::after(Duration::from_millis(200)).await;
                        if view
                            .update(cx, |this, window, cx| {
                                if let Ok(bytes) = std::fs::read(this.output.join("command.json")) {
                                    if bytes.len() < 4096 {
                                        if let Ok(command) = serde_json::from_slice(&bytes) {
                                            this.command(command, window, cx);
                                        }
                                    }
                                }
                                this.save(window, cx);
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
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
    eprintln!("This bounded native smoke currently supports macOS only.");
    std::process::exit(1);
}
