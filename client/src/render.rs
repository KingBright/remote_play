use crate::video_decode::MacDecodedVideoFrame;
use core_foundation::base::TCFType;
use gpui::*;
use protocol::ControlMessage;
use remote_core::net::UdpSender;
use remote_core::stats::Statistics;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};

#[derive(PartialEq)]
pub enum ViewState {
    HostList,
    Streaming,
}

pub async fn run_client(
    udp_sender: UdpSender,
    shared_frame: Arc<Mutex<Option<MacDecodedVideoFrame>>>,
    active_session_id: Arc<std::sync::atomic::AtomicU32>,
    host_stats: Arc<RwLock<crate::HostStats>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let app = gpui::Application::new();

    app.run(move |cx: &mut gpui::App| {
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds(
                point(px(100.0), px(100.0)),
                size(px(1280.0), px(720.0)),
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("RemotePlay Client".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        cx.bind_keys([
            gpui::KeyBinding::new("f3", ToggleStats, None),
            gpui::KeyBinding::new("escape", TogglePanel, None),
            gpui::KeyBinding::new("f4", TogglePanel, None),
        ]);

        cx.open_window(window_options, move |_, cx| {
            let active_session_id_clone = active_session_id.clone();
            let view = cx.new(|cx| RemotePlayView::new(udp_sender.clone(), active_session_id_clone, host_stats, cx));

            // 60FPS Render loop
            cx.spawn({
                let view = view.clone();
                async move |mut cx| {
                    loop {
                        gpui::Timer::after(std::time::Duration::from_millis(16)).await;

                        let new_frame = {
                            let mut guard = shared_frame.lock().unwrap();
                            guard.take()
                        };

                        if let Some(frame) = new_frame {
                            if view
                                .update(&mut *cx, |view, cx| {
                                    view.process_frame(frame, cx);
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            })
            .detach();

            // Heartbeat loop
            cx.spawn({
                let view = view.clone();
                let sender = udp_sender.clone();
                async move |mut cx| {
                    loop {
                        gpui::Timer::after(std::time::Duration::from_millis(1000)).await;
                        let addr = view.update(&mut *cx, |this, _| {
                            if this.state == ViewState::Streaming {
                                Some(this._host_addr)
                            } else {
                                None
                            }
                        });

                        if let Ok(Some(addr)) = addr {
                            let _ = sender.send_control(&ControlMessage::Heartbeat, addr).await;
                        } else if addr.is_err() {
                            break;
                        }
                    }
                }
            })
            .detach();

            view
        })
        .expect("failed to open window");
    });

    Ok(())
}

pub struct RemotePlayView {
    _udp_sender: UdpSender,
    _host_addr: SocketAddr,
    active_session_id: Arc<std::sync::atomic::AtomicU32>,
    host_stats: Arc<RwLock<crate::HostStats>>,
    current_frame: Option<MacDecodedVideoFrame>,
    show_stats: bool,
    focus_handle: gpui::FocusHandle,
    state: ViewState,
    hosts: Vec<SocketAddr>,
    show_panel: bool,
    last_mouse_move: std::time::Instant,

    last_frame_time: Option<std::time::Instant>,
    current_fps: f32,
    last_fps_update: std::time::Instant,
    frames_since_update: usize,
    client_fps: f32,
    client_latency_ms: f32,
    client_jitter_ms: f32,
    global_latency_ms: f32,
    global_jitter_ms: f32,
    target_width: u32,
    target_height: u32,
    target_fps: u32,
    target_bitrate_kbps: u32,
}

impl RemotePlayView {
    pub fn new(
        udp_sender: UdpSender,
        active_session_id: Arc<std::sync::atomic::AtomicU32>,
        host_stats: Arc<RwLock<crate::HostStats>>,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        Self {
            _udp_sender: udp_sender,
            _host_addr: "127.0.0.1:8000".parse().unwrap(),
            active_session_id,
            host_stats,
            current_frame: None,
            show_stats: false,
            focus_handle,
            state: ViewState::HostList,
            hosts: vec![
                "127.0.0.1:8000".parse().unwrap(),
                "192.168.1.100:8000".parse().unwrap(),
            ],
            show_panel: false,
            last_mouse_move: std::time::Instant::now(),
            last_frame_time: None,
            current_fps: 0.0,
            last_fps_update: std::time::Instant::now(),
            frames_since_update: 0,
            client_fps: 0.0,
            client_latency_ms: 0.0,
            client_jitter_ms: 0.0,
            global_latency_ms: 0.0,
            global_jitter_ms: 0.0,
            target_width: 1920,
            target_height: 1080,
            target_fps: 60,
            target_bitrate_kbps: 8000,
        }
    }

    pub fn process_frame(&mut self, frame: MacDecodedVideoFrame, cx: &mut gpui::Context<Self>) {
        if self.state != ViewState::Streaming {
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u32;

        let c_latency = now_ms.wrapping_sub(frame.recv_time) as f32;
        let c_d = (c_latency - self.client_latency_ms).abs();
        self.client_jitter_ms = self.client_jitter_ms + (c_d - self.client_jitter_ms) / 16.0;
        if self.client_latency_ms == 0.0 {
            self.client_latency_ms = c_latency;
        } else {
            self.client_latency_ms =
                self.client_latency_ms + (c_latency - self.client_latency_ms) / 16.0;
        }

        let g_latency = now_ms.wrapping_sub(frame.timestamp) as f32;
        let g_d = (g_latency - self.global_latency_ms).abs();
        self.global_jitter_ms = self.global_jitter_ms + (g_d - self.global_jitter_ms) / 16.0;
        if self.global_latency_ms == 0.0 {
            self.global_latency_ms = g_latency;
        } else {
            self.global_latency_ms =
                self.global_latency_ms + (g_latency - self.global_latency_ms) / 16.0;
        }

        self.current_frame = Some(frame);
        self.frames_since_update += 1;

        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_fps_update).as_secs_f32();
        if elapsed >= 1.0 {
            self.client_fps = self.frames_since_update as f32 / elapsed;
            self.frames_since_update = 0;
            self.last_fps_update = now;
        }
        cx.notify();
    }
}

#[derive(Clone, PartialEq, gpui::Action)]
struct ToggleStats;

#[derive(Clone, PartialEq, gpui::Action)]
struct TogglePanel;

impl Render for RemotePlayView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        match self.state {
            ViewState::HostList => {
                let mut list = div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .h_full()
                    .bg(rgb(0x1e1e1e))
                    .p_8();
                list = list.child(
                    div()
                        .text_xl()
                        .text_color(rgb(0xffffff))
                        .mb_6()
                        .child("Select a Host to Connect"),
                );

                for host in &self.hosts {
                    let addr = *host;
                    let sender = self._udp_sender.clone();
                    list = list.child(
                        div()
                            .flex()
                            .justify_between()
                            .p_4()
                            .mb_2()
                            .bg(rgb(0x2d2d2d))
                            .rounded_md()
                            .child(div().text_color(rgb(0xffffff)).child(host.to_string()))
                            .child(
                                div()
                                    .id(format!("connect-{}", addr))
                                    .bg(rgb(0x007acc))
                                    .p_2()
                                    .rounded_md()
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |this, _event, _window, cx| {
                                            this.state = ViewState::Streaming;
                                            this._host_addr = addr;
                                            this.show_panel = false;
                                            this.last_mouse_move = std::time::Instant::now();

                                            let sid = rand::random::<u32>();
                                            this.active_session_id.store(sid, std::sync::atomic::Ordering::Relaxed);
                                            
                                            let s = sender.clone();
                                            let w = this.target_width;
                                            let h = this.target_height;
                                            let f = this.target_fps;
                                            let b = this.target_bitrate_kbps;
                                            cx.background_executor()
                                                .spawn(async move {
                                                    let msg = ControlMessage::StartStream {
                                                        width: w,
                                                        height: h,
                                                        fps: f,
                                                        bitrate_kbps: b,
                                                        session_id: sid,
                                                    };
                                                    let _ = s.send_control(&msg, addr).await;
                                                })
                                                .detach();
                                        }),
                                    )
                                    .child("Connect"),
                            ),
                    );
                }
                list.into_any_element()
            }
            ViewState::Streaming => {
                let video_surface = if let Some(frame) = &self.current_frame {
                    let cv_pixel_buffer = unsafe {
                        core_foundation::base::CFRetain(
                            frame.cv_pixel_buffer as *const std::ffi::c_void,
                        );
                        core_video::pixel_buffer::CVPixelBuffer::wrap_under_create_rule(
                            frame.cv_pixel_buffer as *mut std::ffi::c_void as _,
                        )
                    };
                    gpui::surface(cv_pixel_buffer)
                        .object_fit(gpui::ObjectFit::Contain)
                        .w_full()
                        .h_full()
                        .into_any_element()
                } else {
                    div()
                        .w_full()
                        .h_full()
                        .bg(rgba(0x000000FF))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .text_color(rgb(0xffffff))
                                .child("Waiting for stream..."),
                        )
                        .into_any_element()
                };

                let mut layout = div()
                    .key_context("RemotePlayView")
                    .track_focus(&self.focus_handle)
                    .on_action(cx.listener(|this, _action: &ToggleStats, _window, cx| {
                        this.show_stats = !this.show_stats;
                        cx.notify();
                    }))
                    .on_action(cx.listener(|this, _action: &TogglePanel, _window, cx| {
                        this.show_panel = !this.show_panel;
                        cx.notify();
                    }))
                    .on_mouse_move(cx.listener(|this, _event, _window, cx| {
                        this.last_mouse_move = std::time::Instant::now();
                        cx.notify();
                    }))
                    .w_full()
                    .h_full()
                    .bg(rgb(0x000000))
                    .child(video_surface);

                let show_mini = self.last_mouse_move.elapsed() < std::time::Duration::from_secs(3);

                if show_mini && !self.show_panel {
                    layout = layout.child(
                        div()
                            .absolute()
                            .top_4()
                            .right_4()
                            .bg(rgba(0x333333AA))
                            .p_2()
                            .rounded_full()
                            .cursor_pointer()
                            .id("open_panel")
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _event, _window, cx| {
                                    this.show_panel = true;
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .w_6()
                                    .h_6()
                                    .flex()
                                    .flex_col()
                                    .justify_around()
                                    .py(px(2.0))
                                    .child(div().w_full().h(px(2.0)).bg(rgb(0xffffff)).rounded_sm())
                                    .child(div().w_full().h(px(2.0)).bg(rgb(0xffffff)).rounded_sm())
                                    .child(div().w_full().h(px(2.0)).bg(rgb(0xffffff)).rounded_sm()),
                            ),
                    );
                }

                if self.show_panel {
                    let hs = self.host_stats.read().unwrap().clone();
                    let sender_4k = self._udp_sender.clone();
                    let sender_2k = self._udp_sender.clone();
                    let sender_1080 = self._udp_sender.clone();
                    let sender_30 = self._udp_sender.clone();
                    let sender_60 = self._udp_sender.clone();
                    let sender_stop = self._udp_sender.clone();
                    let sender_5m = self._udp_sender.clone();
                    let sender_10m = self._udp_sender.clone();
                    let sender_20m = self._udp_sender.clone();
                    let addr = self._host_addr;

                    let mut panel = div()
                        .absolute()
                        .top_12()
                        .right_4()
                        .w_64()
                        .bg(rgba(0x1e1e1eEE))
                        .rounded_md()
                        .p_4()
                        .border_1()
                        .border_color(rgba(0x444444FF))
                        .flex()
                        .flex_col();

                    panel = panel.child(
                        div()
                            .flex()
                            .justify_between()
                            .mb_4()
                            .child(
                                div()
                                    .text_color(rgb(0xffffff))
                                    .font_weight(FontWeight::BOLD)
                                    .child("Control Panel"),
                            )
                            .child(
                                div()
                                    .id("close_panel")
                                    .text_color(rgb(0xaaaaaa))
                                    .cursor_pointer()
                                    .child("✕")
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(|this, _event, _window, cx| {
                                            this.show_panel = false;
                                            cx.notify();
                                        }),
                                    ),
                            ),
                    );

                    panel = panel.child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xaaaaaa))
                            .mb_1()
                            .child("Resolution"),
                    );
                    panel = panel.child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .mb_3()
                            .child(
                                div()
                                    .id("res_4k")
                                    .bg(if self.target_width == 3840 { rgb(0x007acc) } else { rgb(0x333333) })
                                    .p_1()
                                    .rounded_sm()
                                    .text_sm()
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |_this, _event, _window, cx| {
                                            _this.target_width = 3840;
                                            _this.target_height = 2160;
                                            let sid = rand::random::<u32>();
                                            _this.active_session_id.store(sid, std::sync::atomic::Ordering::Relaxed);
                                            let s = sender_4k.clone();
                                            let w = _this.target_width;
                                            let h = _this.target_height;
                                            let f = _this.target_fps;
                                            let b = _this.target_bitrate_kbps;
                                            cx.background_executor()
                                                .spawn(async move {
                                                    let _ = s.send_control(
                                                        &ControlMessage::StartStream {
                                                            width: w,
                                                            height: h,
                                                            fps: f,
                                                            bitrate_kbps: b,
                                                            session_id: sid,
                                                        },
                                                        addr,
                                                    )
                                                    .await;
                                                })
                                                .detach();
                                        }),
                                    )
                                    .child("4K"),
                            )
                            .child(
                                div()
                                    .id("res_2k")
                                    .bg(if self.target_width == 2560 { rgb(0x007acc) } else { rgb(0x333333) })
                                    .p_1()
                                    .rounded_sm()
                                    .text_sm()
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |_this, _event, _window, cx| {
                                            _this.target_width = 2560;
                                            _this.target_height = 1440;
                                            let sid = rand::random::<u32>();
                                            _this.active_session_id.store(sid, std::sync::atomic::Ordering::Relaxed);
                                            let s = sender_2k.clone();
                                            let w = _this.target_width;
                                            let h = _this.target_height;
                                            let f = _this.target_fps;
                                            let b = _this.target_bitrate_kbps;
                                            cx.background_executor()
                                                .spawn(async move {
                                                    let _ = s
                                                        .send_control(
                                                            &ControlMessage::StartStream {
                                                                width: w,
                                                                height: h,
                                                                fps: f,
                                                                bitrate_kbps: b,
                                                                session_id: sid,
                                                                },
                                                            addr,
                                                        )
                                                        .await;
                                                })
                                                .detach();
                                        }),
                                    )
                                    .child("2K"),
                            )
                            .child(
                                div()
                                    .id("res_1080p")
                                    .bg(if self.target_width == 1920 { rgb(0x007acc) } else { rgb(0x333333) })
                                    .p_1()
                                    .rounded_sm()
                                    .text_sm()
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |_this, _event, _window, cx| {
                                            _this.target_width = 1920;
                                            _this.target_height = 1080;
                                            let sid = rand::random::<u32>();
                                            _this.active_session_id.store(sid, std::sync::atomic::Ordering::Relaxed);
                                            let s = sender_1080.clone();
                                            let w = _this.target_width;
                                            let h = _this.target_height;
                                            let f = _this.target_fps;
                                            let b = _this.target_bitrate_kbps;
                                            cx.background_executor()
                                                .spawn(async move {
                                                    let _ = s
                                                        .send_control(
                                                            &ControlMessage::StartStream {
                                                                width: w,
                                                                height: h,
                                                                fps: f,
                                                                bitrate_kbps: b,
                                                                session_id: sid,
                                                                },
                                                            addr,
                                                        )
                                                        .await;
                                                })
                                                .detach();
                                        }),
                                    )
                                    .child("1080p"),
                            ),
                    );

                    panel = panel.child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xaaaaaa))
                            .mb_1()
                            .child("Framerate"),
                    );
                    panel = panel.child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .mb_4()
                            .child(
                                div()
                                    .id("fps_30")
                                    .bg(if self.target_fps == 30 { rgb(0x007acc) } else { rgb(0x333333) })
                                    .p_1()
                                    .rounded_sm()
                                    .text_sm()
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |_this, _event, _window, cx| {
                                            _this.target_fps = 30;
                                            let sid = rand::random::<u32>();
                                            _this.active_session_id.store(sid, std::sync::atomic::Ordering::Relaxed);
                                            let s = sender_30.clone();
                                            let w = _this.target_width;
                                            let h = _this.target_height;
                                            let f = _this.target_fps;
                                            let b = _this.target_bitrate_kbps;
                                            cx.background_executor()
                                                .spawn(async move {
                                                    let _ = s
                                                        .send_control(
                                                            &ControlMessage::StartStream {
                                                                width: w,
                                                                height: h,
                                                                fps: f,
                                                                bitrate_kbps: b,
                                                                session_id: sid,
                                                                },
                                                            addr,
                                                        )
                                                        .await;
                                                })
                                                .detach();
                                        }),
                                    )
                                    .child("30 FPS"),
                            )
                            .child(
                                div()
                                    .id("fps_60")
                                    .bg(if self.target_fps == 60 { rgb(0x007acc) } else { rgb(0x333333) })
                                    .p_1()
                                    .rounded_sm()
                                    .text_sm()
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |_this, _event, _window, cx| {
                                            _this.target_fps = 60;
                                            let sid = rand::random::<u32>();
                                            _this.active_session_id.store(sid, std::sync::atomic::Ordering::Relaxed);
                                            let s = sender_60.clone();
                                            let w = _this.target_width;
                                            let h = _this.target_height;
                                            let f = _this.target_fps;
                                            let b = _this.target_bitrate_kbps;
                                            cx.background_executor()
                                                .spawn(async move {
                                                    let _ = s
                                                        .send_control(
                                                            &ControlMessage::StartStream {
                                                                width: w,
                                                                height: h,
                                                                fps: f,
                                                                bitrate_kbps: b,
                                                                session_id: sid,
                                                                },
                                                            addr,
                                                        )
                                                        .await;
                                                })
                                                .detach();
                                        }),
                                    )
                                    .child("60 FPS"),
                            ),
                    );

                    panel = panel.child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xaaaaaa))
                            .mb_1()
                            .child("Bitrate"),
                    );
                    panel = panel.child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .mb_4()
                            .child(
                                div()
                                    .id("bitrate_5m")
                                    .bg(if self.target_bitrate_kbps == 5000 { rgb(0x007acc) } else { rgb(0x333333) })
                                    .p_1()
                                    .rounded_sm()
                                    .text_sm()
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |_this, _event, _window, cx| {
                                            _this.target_bitrate_kbps = 5000;
                                            let sid = rand::random::<u32>();
                                            _this.active_session_id.store(sid, std::sync::atomic::Ordering::Relaxed);
                                            let s = sender_5m.clone();
                                            let w = _this.target_width;
                                            let h = _this.target_height;
                                            let f = _this.target_fps;
                                            let b = _this.target_bitrate_kbps;
                                            cx.background_executor()
                                                .spawn(async move {
                                                    let _ = s
                                                        .send_control(
                                                            &ControlMessage::StartStream {
                                                                width: w,
                                                                height: h,
                                                                fps: f,
                                                                bitrate_kbps: b,
                                                                session_id: sid,
                                                                },
                                                            addr,
                                                        )
                                                        .await;
                                                })
                                                .detach();
                                        }),
                                    )
                                    .child("5 Mbps"),
                            )
                            .child(
                                div()
                                    .id("bitrate_10m")
                                    .bg(if self.target_bitrate_kbps == 10000 { rgb(0x007acc) } else { rgb(0x333333) })
                                    .p_1()
                                    .rounded_sm()
                                    .text_sm()
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |_this, _event, _window, cx| {
                                            _this.target_bitrate_kbps = 10000;
                                            let sid = rand::random::<u32>();
                                            _this.active_session_id.store(sid, std::sync::atomic::Ordering::Relaxed);
                                            let s = sender_10m.clone();
                                            let w = _this.target_width;
                                            let h = _this.target_height;
                                            let f = _this.target_fps;
                                            let b = _this.target_bitrate_kbps;
                                            cx.background_executor()
                                                .spawn(async move {
                                                    let _ = s
                                                        .send_control(
                                                            &ControlMessage::StartStream {
                                                                width: w,
                                                                height: h,
                                                                fps: f,
                                                                bitrate_kbps: b,
                                                                session_id: sid,
                                                                },
                                                            addr,
                                                        )
                                                        .await;
                                                })
                                                .detach();
                                        }),
                                    )
                                    .child("10 Mbps"),
                            )
                            .child(
                                div()
                                    .id("bitrate_20m")
                                    .bg(if self.target_bitrate_kbps == 20000 { rgb(0x007acc) } else { rgb(0x333333) })
                                    .p_1()
                                    .rounded_sm()
                                    .text_sm()
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |_this, _event, _window, cx| {
                                            _this.target_bitrate_kbps = 20000;
                                            let sid = rand::random::<u32>();
                                            _this.active_session_id.store(sid, std::sync::atomic::Ordering::Relaxed);
                                            let s = sender_20m.clone();
                                            let w = _this.target_width;
                                            let h = _this.target_height;
                                            let f = _this.target_fps;
                                            let b = _this.target_bitrate_kbps;
                                            cx.background_executor()
                                                .spawn(async move {
                                                    let _ = s
                                                        .send_control(
                                                            &ControlMessage::StartStream {
                                                                width: w,
                                                                height: h,
                                                                fps: f,
                                                                bitrate_kbps: b,
                                                                session_id: sid,
                                                                },
                                                            addr,
                                                        )
                                                        .await;
                                                })
                                                .detach();
                                        }),
                                    )
                                    .child("20 Mbps"),
                            ),
                    );

                    panel = panel.child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x00ff00))
                            .font_family("Courier")
                            .child(format!(
                                "Host : {:.1}fps | {:.1} Mbps | L: {:.1}ms | J: {:.1}ms",
                                hs.fps, hs.bitrate_kbps as f32 / 1000.0, hs.latency, hs.jitter
                            )),
                    );
                    panel = panel.child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x00ffff))
                            .font_family("Courier")
                            .child(format!(
                                "Client: {:.1}fps | L: {:.1}ms | J: {:.1}ms",
                                self.client_fps, self.client_latency_ms, self.client_jitter_ms
                            )),
                    );
                    panel = panel.child(
                        div()
                            .text_xs()
                            .text_color(rgb(0xffff00))
                            .font_family("Courier")
                            .child(format!(
                                "Global: L: {:.1}ms | J: {:.1}ms",
                                self.global_latency_ms, self.global_jitter_ms
                            )),
                    );

                    panel = panel.child(
                        div()
                            .id("disconnect_btn")
                            .mt_4()
                            .bg(rgb(0xcc0000))
                            .p_2()
                            .rounded_sm()
                            .text_center()
                            .text_sm()
                            .text_color(rgb(0xffffff))
                            .cursor_pointer()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _event, _window, cx| {
                                    let s = sender_stop.clone();
                                    cx.background_executor()
                                        .spawn(async move {
                                            let _ = s
                                                .send_control(&ControlMessage::StopStream, addr)
                                                .await;
                                        })
                                        .detach();
                                    this.state = ViewState::HostList;
                                    this.current_frame = None;
                                    cx.notify();
                                }),
                            )
                            .child("Disconnect"),
                    );

                    layout = layout.child(panel);
                }

                if self.show_stats {
                    let hs = self.host_stats.read().unwrap().clone();
                    layout = layout.child(
                        div()
                            .absolute()
                            .top_4()
                            .left_4()
                            .p_4()
                            .bg(rgba(0x1e1e2eCC))
                            .rounded_lg()
                            .flex()
                            .gap_8()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .child("Host (Capture/Encode)")
                                            .text_color(rgb(0x89b4fa))
                                            .font_weight(FontWeight::BOLD),
                                    )
                                    .child(
                                        div()
                                            .child(format!("FPS: {:.1}", hs.fps))
                                            .text_color(rgb(0xa6e3a1)),
                                    )
                                    .child(
                                        div()
                                            .child(format!("Bitrate: {:.1} Mbps", hs.bitrate_kbps as f32 / 1000.0))
                                            .text_color(rgb(0xa6e3a1)),
                                    )
                                    .child(
                                        div()
                                            .child(format!("Latency: {:.1} ms", hs.latency))
                                            .text_color(rgb(0xf9e2af)),
                                    )
                                    .child(
                                        div()
                                            .child(format!("Jitter: {:.1} ms", hs.jitter))
                                            .text_color(rgb(0xf38ba8)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .child("Client (Network/Render)")
                                            .text_color(rgb(0xcba6f7))
                                            .font_weight(FontWeight::BOLD),
                                    )
                                    .child(
                                        div()
                                            .child(format!("FPS: {:.1}", self.client_fps))
                                            .text_color(rgb(0xa6e3a1)),
                                    )
                                    .child(
                                        div()
                                            .child(format!(
                                                "Latency: {:.1} ms",
                                                self.client_latency_ms
                                            ))
                                            .text_color(rgb(0xf9e2af)),
                                    )
                                    .child(
                                        div()
                                            .child(format!(
                                                "Jitter: {:.1} ms",
                                                self.client_jitter_ms
                                            ))
                                            .text_color(rgb(0xf38ba8)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .child("Global (End-to-End)")
                                            .text_color(rgb(0xf38ba8))
                                            .font_weight(FontWeight::BOLD),
                                    )
                                    .child(div().child("FPS: N/A").text_color(rgb(0x6c7086))) // Global FPS is effectively Client FPS
                                    .child(
                                        div()
                                            .child(format!(
                                                "Latency: {:.1} ms",
                                                self.global_latency_ms
                                            ))
                                            .text_color(rgb(0xf9e2af)),
                                    )
                                    .child(
                                        div()
                                            .child(format!(
                                                "Jitter: {:.1} ms",
                                                self.global_jitter_ms
                                            ))
                                            .text_color(rgb(0xf38ba8)),
                                    ),
                            ),
                    );
                }

                layout.into_any_element()
            }
        }
    }
}
