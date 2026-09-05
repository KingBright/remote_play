use crate::host_config::configured_hosts;
use crate::mesh_pairing::{MeshPairingControl, MeshPairingMessageKind, MeshPairingSnapshot};
use crate::transfer_center::{
    TransferCancelTarget, TransferDirection, TransferEntrySnapshot, TransferStatus,
};
use crate::video_decode::MacDecodedVideoFrame;
use crate::{
    AudioPlaybackControl, audio_player::AudioPlayerSettings, talkback::TalkbackCaptureSettings,
};
use core_foundation::base::TCFType;
use gpui::{ClipboardItem, *};
use protocol::{AudioControlTarget, ControlMessage};
use remote_core::discovery::DiscoveryPeerSnapshot;
use remote_core::mesh::{EasyTierHealthIssue, EasyTierHealthSnapshot, EasyTierHealthState};
use remote_core::net::{DEFAULT_CONTROL_PORT, UdpSender};
use std::error::Error;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(PartialEq)]
pub enum ViewState {
    HostList,
    Streaming,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TalkbackMode {
    Off,
    AlwaysOn,
    PushToTalk,
}

#[derive(Clone)]
pub struct ClientRuntimeControls {
    pub audio_playback: AudioPlaybackControl,
    pub clipboard: Option<crate::ClipboardRuntimeControl>,
    pub file_transfer: Option<crate::FileTransferRuntimeControl>,
    pub talkback: Option<crate::TalkbackRuntimeControl>,
    pub discovery: Option<tokio::sync::watch::Receiver<DiscoveryPeerSnapshot>>,
    pub mesh_health: Option<tokio::sync::watch::Receiver<EasyTierHealthSnapshot>>,
    pub mesh_pairing: Option<MeshPairingControl>,
}

pub async fn run_client(
    udp_sender: UdpSender,
    shared_frame: Arc<Mutex<Option<MacDecodedVideoFrame>>>,
    active_session_id: Arc<std::sync::atomic::AtomicU32>,
    host_stats: Arc<crate::SharedHostStats>,
    controls: ClientRuntimeControls,
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
            let controls = controls.clone();
            let view = cx.new(|cx| {
                RemotePlayView::new(
                    udp_sender.clone(),
                    active_session_id_clone,
                    host_stats,
                    controls,
                    cx,
                )
            });

            // 60FPS Render loop
            cx.spawn({
                let view = view.clone();
                async move |cx| {
                    loop {
                        gpui::Timer::after(std::time::Duration::from_millis(16)).await;

                        let new_frame = {
                            let mut guard = shared_frame.lock().unwrap();
                            guard.take()
                        };

                        if let Some(frame) = new_frame
                            && view
                                .update(&mut *cx, |view, cx| {
                                    view.process_frame(frame, cx);
                                })
                                .is_err()
                        {
                            break;
                        }
                    }
                }
            })
            .detach();

            // Heartbeat loop
            cx.spawn({
                let view = view.clone();
                let sender = udp_sender.clone();
                async move |cx| {
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

            cx.spawn({
                let view = view.clone();
                async move |cx| {
                    loop {
                        gpui::Timer::after(std::time::Duration::from_millis(250)).await;
                        if view
                            .update(&mut *cx, |_this, cx| {
                                cx.notify();
                            })
                            .is_err()
                        {
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
    host_stats: Arc<crate::SharedHostStats>,
    controls: ClientRuntimeControls,
    current_frame: Option<MacDecodedVideoFrame>,
    show_stats: bool,
    focus_handle: gpui::FocusHandle,
    state: ViewState,
    hosts: Vec<SocketAddr>,
    discovery: Option<tokio::sync::watch::Receiver<DiscoveryPeerSnapshot>>,
    mesh_health: Option<tokio::sync::watch::Receiver<EasyTierHealthSnapshot>>,
    mesh_pairing: Option<MeshPairingControl>,
    mesh_pairing_snapshot: Option<MeshPairingSnapshot>,
    show_panel: bool,
    last_mouse_move: std::time::Instant,

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
    remote_system_muted: bool,
    remote_microphone_muted: bool,
    remote_audio_volume_percent: u8,
    talkback_mode: TalkbackMode,
    talkback_local_mic_muted: bool,
    talkback_push_active: bool,
    talkback_remote_muted: bool,
    talkback_remote_volume_percent: u8,
    telemetry_engine: remote_core::PipelineTelemetryEngine,
}

impl RemotePlayView {
    pub fn new(
        udp_sender: UdpSender,
        active_session_id: Arc<std::sync::atomic::AtomicU32>,
        host_stats: Arc<crate::SharedHostStats>,
        controls: ClientRuntimeControls,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let talkback_mode = if controls.talkback.is_some() {
            TalkbackMode::AlwaysOn
        } else {
            TalkbackMode::Off
        };
        let hosts = configured_hosts();
        let discovery = controls.discovery.clone();
        let mesh_health = controls.mesh_health.clone();
        let mesh_pairing = controls.mesh_pairing.clone();
        let mesh_pairing_snapshot = mesh_pairing.as_ref().map(MeshPairingControl::snapshot);
        let current_session_id = active_session_id.load(std::sync::atomic::Ordering::Relaxed);
        let telemetry_engine = remote_core::PipelineTelemetryEngine::new(current_session_id, 300);

        Self {
            _udp_sender: udp_sender,
            _host_addr: SocketAddr::from(([127, 0, 0, 1], DEFAULT_CONTROL_PORT)),
            active_session_id,
            host_stats,
            controls,
            current_frame: None,
            show_stats: false,
            focus_handle,
            state: ViewState::HostList,
            hosts,
            discovery,
            mesh_health,
            mesh_pairing,
            mesh_pairing_snapshot,
            show_panel: false,
            last_mouse_move: std::time::Instant::now(),
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
            remote_system_muted: false,
            remote_microphone_muted: false,
            remote_audio_volume_percent: 100,
            talkback_mode,
            talkback_local_mic_muted: false,
            talkback_push_active: false,
            talkback_remote_muted: false,
            talkback_remote_volume_percent: 100,
            telemetry_engine,
        }
    }

    fn mesh_health_snapshot(&self) -> Option<EasyTierHealthSnapshot> {
        self.mesh_health.as_ref().map(|rx| rx.borrow().clone())
    }

    fn copy_mesh_invite_code(&mut self, cx: &mut gpui::Context<Self>) {
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

    fn join_mesh_group_from_clipboard(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(control) = &self.mesh_pairing else {
            return;
        };
        let invite_code = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        self.mesh_pairing_snapshot = Some(control.join_from_invite_code(&invite_code));
    }

    pub fn process_frame(&mut self, mut frame: MacDecodedVideoFrame, cx: &mut gpui::Context<Self>) {
        if self.state != ViewState::Streaming {
            return;
        }
        if frame.timing.capture_ts_us > 0 {
            let floor = frame
                .timing
                .decode_done_ts_us
                .max(frame.timing.jitter_exit_ts_us)
                .max(frame.timing.recv_ts_us);
            let elapsed_us = frame.decoded_at.elapsed().as_micros() as u32;
            frame.timing.render_submit_ts_us =
                remote_core::timing::advance_client_stage(floor, elapsed_us);
            frame.timing.render_done_ts_us = frame.timing.render_submit_ts_us;
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

        let timing = frame.timing;
        self.telemetry_engine.record_frame(&timing, 0);

        self.current_frame = Some(frame);
        self.frames_since_update += 1;

        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_fps_update).as_secs_f32();
        if elapsed >= 1.0 {
            self.client_fps = self.frames_since_update as f32 / elapsed;
            self.frames_since_update = 0;
            self.last_fps_update = now;
        }

        self.host_stats.set_latest_timing(timing);
        self.host_stats
            .set_pipeline_report(self.telemetry_engine.generate_report());

        cx.notify();
    }

    fn start_talkback_session(&self, addr: SocketAddr, session_id: u32) {
        if let Some(control) = &self.controls.talkback {
            if self.talkback_mode == TalkbackMode::Off {
                control.stop();
            } else {
                control.set_settings(self.talkback_capture_settings());
                control.start(addr, session_id);
            }
        }
    }

    fn stop_talkback_session(&self) {
        if let Some(control) = &self.controls.talkback {
            control.stop();
        }
    }

    fn sync_talkback_session(&self) {
        let session_id = self
            .active_session_id
            .load(std::sync::atomic::Ordering::Relaxed);
        if session_id == 0 {
            self.stop_talkback_session();
        } else {
            self.start_talkback_session(self._host_addr, session_id);
        }
    }

    fn apply_talkback_settings(&self) {
        if let Some(control) = &self.controls.talkback {
            control.set_settings(self.talkback_capture_settings());
        }
    }

    fn talkback_capture_settings(&self) -> TalkbackCaptureSettings {
        TalkbackCaptureSettings {
            local_mic_muted: self.talkback_local_mic_muted,
            push_to_talk: self.talkback_mode == TalkbackMode::PushToTalk,
            push_to_talk_active: self.talkback_push_active,
        }
    }

    fn apply_audio_playback_settings(&self) {
        self.controls
            .audio_playback
            .set_settings(self.audio_playback_settings());
    }

    fn audio_playback_settings(&self) -> AudioPlayerSettings {
        AudioPlayerSettings {
            remote_system_muted: self.remote_system_muted,
            remote_microphone_muted: self.remote_microphone_muted,
            volume_percent: self.remote_audio_volume_percent,
        }
    }

    fn talkback_playback_control_message(&self) -> Option<ControlMessage> {
        let session_id = self
            .active_session_id
            .load(std::sync::atomic::Ordering::Relaxed);
        (session_id != 0).then_some(ControlMessage::AudioControl {
            session_id,
            target: AudioControlTarget::ViewerTalkbackPlayback,
            muted: self.talkback_remote_muted,
            volume_percent: self.talkback_remote_volume_percent,
        })
    }

    fn send_talkback_playback_control(&self, cx: &mut gpui::Context<Self>) {
        let Some(message) = self.talkback_playback_control_message() else {
            return;
        };
        let sender = self._udp_sender.clone();
        let addr = self._host_addr;
        cx.background_executor()
            .spawn(async move {
                let _ = sender.send_control(&message, addr).await;
            })
            .detach();
    }

    fn request_stream(&self, sender: UdpSender, addr: SocketAddr, cx: &mut gpui::Context<Self>) {
        let sid = rand::random::<u32>();
        self.active_session_id
            .store(sid, std::sync::atomic::Ordering::Relaxed);
        self.start_talkback_session(addr, sid);
        self.send_talkback_playback_control(cx);

        let width = self.target_width;
        let height = self.target_height;
        let fps = self.target_fps;
        let bitrate_kbps = self.target_bitrate_kbps;
        cx.background_executor()
            .spawn(async move {
                let msg = ControlMessage::StartStream {
                    width,
                    height,
                    fps,
                    bitrate_kbps,
                    session_id: sid,
                };
                let _ = sender.send_control(&msg, addr).await;
            })
            .detach();
    }
}

fn host_badge(addr: SocketAddr) -> &'static str {
    if addr.ip().is_loopback() {
        "Local endpoint"
    } else {
        "Configured endpoint"
    }
}

#[derive(Clone)]
struct DeviceRow {
    addr: SocketAddr,
    title: String,
    badge: String,
}

impl RemotePlayView {
    fn device_rows(&self) -> Vec<DeviceRow> {
        let mut rows = Vec::new();
        if let Some(discovery) = &self.discovery {
            for peer in discovery.borrow().peers() {
                if !peer.announcement.capabilities.can_stream || peer.announcement.control_port == 0
                {
                    continue;
                }
                let scope = match peer.scope {
                    remote_core::discovery::DiscoveryScope::Lan => "LAN discovery",
                    remote_core::discovery::DiscoveryScope::Mesh => "Mesh discovery",
                    remote_core::discovery::DiscoveryScope::Relay => "Relay discovery",
                };
                rows.push(DeviceRow {
                    addr: peer.endpoint,
                    title: peer.announcement.display_name.clone(),
                    badge: format!("{scope} · {}", peer.endpoint),
                });
            }
        }

        for host in &self.hosts {
            if rows.iter().any(|row| row.addr == *host) {
                continue;
            }
            rows.push(DeviceRow {
                addr: *host,
                title: host.to_string(),
                badge: host_badge(*host).to_string(),
            });
        }
        rows
    }
}

fn panel_section(label: &'static str) -> Div {
    div()
        .mt_4()
        .mb_2()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(0xb6c2cf))
        .child(label)
}

fn control_fill(active: bool) -> Rgba {
    if active { rgb(0x2563eb) } else { rgb(0x242a33) }
}

fn toggle_fill(is_on: bool) -> Rgba {
    if is_on { rgb(0x2f7d68) } else { rgb(0x57343d) }
}

fn mesh_status_text(snapshot: &EasyTierHealthSnapshot) -> String {
    if snapshot.issue == Some(EasyTierHealthIssue::RequiresAdminPrivileges) {
        return "Mesh needs admin setup".to_string();
    }

    match snapshot.state {
        EasyTierHealthState::Ready => format!(
            "Mesh ready · {}",
            snapshot
                .virtual_ip
                .map(|ip| ip.to_string())
                .unwrap_or_else(|| "virtual IP pending".to_string())
        ),
        EasyTierHealthState::Starting => "Mesh starting".to_string(),
        EasyTierHealthState::Degraded => "Mesh degraded".to_string(),
        EasyTierHealthState::Stopped => "Mesh stopped".to_string(),
    }
}

fn mesh_status_color(snapshot: &EasyTierHealthSnapshot) -> Rgba {
    match snapshot.state {
        EasyTierHealthState::Ready => rgb(0x39a275),
        EasyTierHealthState::Starting => rgb(0xd49a3a),
        EasyTierHealthState::Degraded => rgb(0xd65f5f),
        EasyTierHealthState::Stopped => rgb(0x6f7a86),
    }
}

fn pairing_message_color(kind: MeshPairingMessageKind) -> Rgba {
    match kind {
        MeshPairingMessageKind::Neutral => rgb(0x8d98a7),
        MeshPairingMessageKind::Success => rgb(0x39a275),
        MeshPairingMessageKind::Warning => rgb(0xd49a3a),
        MeshPairingMessageKind::Error => rgb(0xd65f5f),
    }
}

fn compact_device_id(device_id: &str) -> String {
    if device_id.len() <= 8 {
        device_id.to_string()
    } else {
        format!("{}...", &device_id[..8])
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
                    .items_center()
                    .justify_center()
                    .bg(rgb(0x101114))
                    .p_8();

                let mut device_list = div().flex().flex_col().w(px(560.0)).gap_3();
                device_list = device_list.child(
                    div()
                        .flex()
                        .flex_col()
                        .mb_3()
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(FontWeight::BOLD)
                                .text_color(rgb(0xf4f7fb))
                                .child("RemotePlay"),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_sm()
                                .text_color(rgb(0x8d98a7))
                                .child("Devices"),
                        ),
                );

                if let Some(snapshot) = self.mesh_health_snapshot() {
                    let color = mesh_status_color(&snapshot);
                    device_list = device_list.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .mb_2()
                            .px_3()
                            .py_2()
                            .bg(rgb(0x181b20))
                            .border_1()
                            .border_color(rgb(0x2a3038))
                            .rounded_md()
                            .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(color))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0xb6c2cf))
                                    .truncate()
                                    .child(mesh_status_text(&snapshot)),
                            ),
                    );
                }

                if let Some(snapshot) = self.mesh_pairing_snapshot.clone() {
                    let invite_code = snapshot.invite_code.clone();
                    let message_color = pairing_message_color(snapshot.message_kind);
                    let restart_text = if snapshot.restart_required {
                        "Mesh will use this group after restart."
                    } else {
                        ""
                    };
                    let mut pairing_card = div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .mb_3()
                        .p_3()
                        .bg(rgb(0x181b20))
                        .border_1()
                        .border_color(rgb(0x2a3038))
                        .rounded_md()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .overflow_hidden()
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
                                                .text_color(rgb(0x7f8a99))
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
                                        .bg(rgb(0x20252d))
                                        .text_xs()
                                        .text_color(rgb(0xb6c2cf))
                                        .child("RPM2"),
                                ),
                        )
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .bg(rgb(0x101318))
                                .border_1()
                                .border_color(rgb(0x2a3038))
                                .rounded_sm()
                                .text_xs()
                                .text_color(rgb(0xd8dee9))
                                .truncate()
                                .child(invite_code),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    div()
                                        .id("copy_mesh_invite")
                                        .bg(rgb(0x2563eb))
                                        .px_3()
                                        .py_2()
                                        .rounded_sm()
                                        .text_sm()
                                        .text_color(rgb(0xffffff))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _event, _window, cx| {
                                                this.copy_mesh_invite_code(cx);
                                                cx.notify();
                                            }),
                                        )
                                        .child("Copy Code"),
                                )
                                .child(
                                    div()
                                        .id("join_mesh_invite_clipboard")
                                        .bg(rgb(0x242a33))
                                        .px_3()
                                        .py_2()
                                        .rounded_sm()
                                        .text_sm()
                                        .text_color(rgb(0xffffff))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _event, _window, cx| {
                                                this.join_mesh_group_from_clipboard(cx);
                                                cx.notify();
                                            }),
                                        )
                                        .child("Join Clipboard"),
                                )
                                .child(
                                    div()
                                        .id("create_mesh_group")
                                        .bg(rgb(0x242a33))
                                        .px_3()
                                        .py_2()
                                        .rounded_sm()
                                        .text_sm()
                                        .text_color(rgb(0xffffff))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _event, _window, cx| {
                                                this.create_mesh_group();
                                                cx.notify();
                                            }),
                                        )
                                        .child("New Group"),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(message_color)
                                .child(snapshot.message),
                        );
                    if !restart_text.is_empty() {
                        pairing_card = pairing_card.child(
                            div()
                                .text_xs()
                                .text_color(rgb(0xd49a3a))
                                .child(restart_text),
                        );
                    }
                    device_list = device_list.child(pairing_card);
                }

                for row in self.device_rows() {
                    let addr = row.addr;
                    let sender = self._udp_sender.clone();
                    device_list = device_list.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .p_3()
                            .bg(rgb(0x181b20))
                            .border_1()
                            .border_color(rgb(0x2a3038))
                            .rounded_md()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(0xf4f7fb))
                                            .truncate()
                                            .child(row.title),
                                    )
                                    .child(
                                        div()
                                            .mt_1()
                                            .text_xs()
                                            .text_color(rgb(0x7f8a99))
                                            .child(row.badge),
                                    ),
                            )
                            .child(
                                div()
                                    .id(format!("connect-{}", addr))
                                    .bg(rgb(0x2563eb))
                                    .px_3()
                                    .py_2()
                                    .rounded_sm()
                                    .text_sm()
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |this, _event, _window, cx| {
                                            this.state = ViewState::Streaming;
                                            this._host_addr = addr;
                                            this.show_panel = false;
                                            this.last_mouse_move = std::time::Instant::now();
                                            if let Some(control) = &this.controls.clipboard {
                                                control.start(addr);
                                            }
                                            if let Some(control) = &this.controls.file_transfer {
                                                control.start(addr);
                                            }

                                            this.request_stream(sender.clone(), addr, cx);
                                        }),
                                    )
                                    .child("Connect"),
                            ),
                    );
                }
                list = list.child(device_list);
                list.into_any_element()
            }
            ViewState::Streaming => {
                let video_surface = if let Some(frame) = &self.current_frame {
                    let cv_pixel_buffer = unsafe {
                        core_foundation::base::CFRetain(
                            frame.cv_pixel_buffer as *const std::ffi::c_void,
                        );
                        core_video::pixel_buffer::CVPixelBuffer::wrap_under_create_rule(
                            frame.cv_pixel_buffer as _,
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
                                .text_sm()
                                .text_color(rgb(0xd8dee9))
                                .child("Connecting..."),
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
                            .bg(rgba(0x171a20dd))
                            .p_2()
                            .rounded_full()
                            .border_1()
                            .border_color(rgba(0xffffff22))
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
                                    .child(div().w_full().h(px(2.0)).bg(rgb(0xf4f7fb)).rounded_sm())
                                    .child(div().w_full().h(px(2.0)).bg(rgb(0xf4f7fb)).rounded_sm())
                                    .child(
                                        div().w_full().h(px(2.0)).bg(rgb(0xf4f7fb)).rounded_sm(),
                                    ),
                            ),
                    );
                }

                if self.show_panel {
                    let hs = self.host_stats.snapshot();
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
                    let transfer_entries = self
                        .controls
                        .file_transfer
                        .as_ref()
                        .map(|control| control.transfer_snapshot())
                        .unwrap_or_default();

                    let mut panel = div()
                        .absolute()
                        .top_12()
                        .right_4()
                        .w(px(390.0))
                        .max_h(px(640.0))
                        .id("control_panel")
                        .overflow_y_scroll()
                        .scrollbar_width(px(4.0))
                        .bg(rgba(0x15181dee))
                        .rounded_md()
                        .p_4()
                        .border_1()
                        .border_color(rgba(0xffffff24))
                        .flex()
                        .flex_col();

                    panel = panel.child(
                        div()
                            .flex()
                            .items_start()
                            .justify_between()
                            .mb_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .text_color(rgb(0xf4f7fb))
                                            .font_weight(FontWeight::BOLD)
                                            .child("Session"),
                                    )
                                    .child(
                                        div()
                                            .mt_1()
                                            .text_xs()
                                            .text_color(rgb(0x8d98a7))
                                            .truncate()
                                            .child(addr.to_string()),
                                    ),
                            )
                            .child(
                                div()
                                    .id("close_panel")
                                    .text_color(rgb(0x8d98a7))
                                    .cursor_pointer()
                                    .p_1()
                                    .child("x")
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(|this, _event, _window, cx| {
                                            this.show_panel = false;
                                            cx.notify();
                                        }),
                                    ),
                            ),
                    );

                    panel = panel.child(panel_section("Stream"));
                    panel = panel.child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x7f8a99))
                            .mb_1()
                            .child("Size"),
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
                                    .bg(control_fill(self.target_width == 3840))
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
                                            _this
                                                .active_session_id
                                                .store(sid, std::sync::atomic::Ordering::Relaxed);
                                            _this.start_talkback_session(addr, sid);
                                            _this.send_talkback_playback_control(cx);
                                            let s = sender_4k.clone();
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
                                    .child("4K"),
                            )
                            .child(
                                div()
                                    .id("res_2k")
                                    .bg(control_fill(self.target_width == 2560))
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
                                            _this
                                                .active_session_id
                                                .store(sid, std::sync::atomic::Ordering::Relaxed);
                                            _this.start_talkback_session(addr, sid);
                                            _this.send_talkback_playback_control(cx);
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
                                    .bg(control_fill(self.target_width == 1920))
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
                                            _this
                                                .active_session_id
                                                .store(sid, std::sync::atomic::Ordering::Relaxed);
                                            _this.start_talkback_session(addr, sid);
                                            _this.send_talkback_playback_control(cx);
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
                            .text_xs()
                            .text_color(rgb(0x7f8a99))
                            .mb_1()
                            .child("FPS"),
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
                                    .bg(control_fill(self.target_fps == 30))
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
                                            _this
                                                .active_session_id
                                                .store(sid, std::sync::atomic::Ordering::Relaxed);
                                            _this.start_talkback_session(addr, sid);
                                            _this.send_talkback_playback_control(cx);
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
                                    .bg(control_fill(self.target_fps == 60))
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
                                            _this
                                                .active_session_id
                                                .store(sid, std::sync::atomic::Ordering::Relaxed);
                                            _this.start_talkback_session(addr, sid);
                                            _this.send_talkback_playback_control(cx);
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
                            .text_xs()
                            .text_color(rgb(0x7f8a99))
                            .mb_1()
                            .child("Rate"),
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
                                    .bg(control_fill(self.target_bitrate_kbps == 5000))
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
                                            _this
                                                .active_session_id
                                                .store(sid, std::sync::atomic::Ordering::Relaxed);
                                            _this.start_talkback_session(addr, sid);
                                            _this.send_talkback_playback_control(cx);
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
                                    .bg(control_fill(self.target_bitrate_kbps == 10000))
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
                                            _this
                                                .active_session_id
                                                .store(sid, std::sync::atomic::Ordering::Relaxed);
                                            _this.start_talkback_session(addr, sid);
                                            _this.send_talkback_playback_control(cx);
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
                                    .bg(control_fill(self.target_bitrate_kbps == 20000))
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
                                            _this
                                                .active_session_id
                                                .store(sid, std::sync::atomic::Ordering::Relaxed);
                                            _this.start_talkback_session(addr, sid);
                                            _this.send_talkback_playback_control(cx);
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
                            .mt_1()
                            .p_2()
                            .rounded_sm()
                            .bg(rgba(0x20252ddd))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xs().text_color(rgb(0xd8dee9)).child(format!(
                                "Host {:.1} fps / {:.1} Mbps / {:.1} ms",
                                hs.fps,
                                hs.bitrate_kbps as f32 / 1000.0,
                                hs.latency
                            )))
                            .child(div().text_xs().text_color(rgb(0xaab4c2)).child(format!(
                                "Client {:.1} fps / {:.1} ms / jitter {:.1} ms",
                                self.client_fps, self.client_latency_ms, self.client_jitter_ms
                            )))
                            .child(div().text_xs().text_color(rgb(0xaab4c2)).child(format!(
                                "End to end {:.1} ms / jitter {:.1} ms",
                                self.global_latency_ms, self.global_jitter_ms
                            ))),
                    );

                    panel = panel.child(panel_section("Audio"));
                    panel = panel.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .mb_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("remote_system_mute_btn")
                                            .bg(toggle_fill(!self.remote_system_muted))
                                            .flex_1()
                                            .p_2()
                                            .rounded_sm()
                                            .text_center()
                                            .text_sm()
                                            .text_color(rgb(0xffffff))
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                cx.listener(|this, _event, _window, cx| {
                                                    this.remote_system_muted =
                                                        !this.remote_system_muted;
                                                    this.apply_audio_playback_settings();
                                                    cx.notify();
                                                }),
                                            )
                                            .child(if self.remote_system_muted {
                                                "System Muted"
                                            } else {
                                                "System On"
                                            }),
                                    )
                                    .child(
                                        div()
                                            .id("remote_microphone_mute_btn")
                                            .bg(toggle_fill(!self.remote_microphone_muted))
                                            .flex_1()
                                            .p_2()
                                            .rounded_sm()
                                            .text_center()
                                            .text_sm()
                                            .text_color(rgb(0xffffff))
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                cx.listener(|this, _event, _window, cx| {
                                                    this.remote_microphone_muted =
                                                        !this.remote_microphone_muted;
                                                    this.apply_audio_playback_settings();
                                                    cx.notify();
                                                }),
                                            )
                                            .child(if self.remote_microphone_muted {
                                                "Remote Mic Muted"
                                            } else {
                                                "Remote Mic On"
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .id("audio_volume_down_btn")
                                            .bg(rgb(0x242a33))
                                            .p_2()
                                            .rounded_sm()
                                            .text_center()
                                            .text_sm()
                                            .text_color(rgb(0xffffff))
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                cx.listener(|this, _event, _window, cx| {
                                                    this.remote_audio_volume_percent = this
                                                        .remote_audio_volume_percent
                                                        .saturating_sub(10);
                                                    this.apply_audio_playback_settings();
                                                    cx.notify();
                                                }),
                                            )
                                            .child("-"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(0xd8dee9))
                                            .font_family("Courier")
                                            .child(format!(
                                                "Remote {}%",
                                                self.remote_audio_volume_percent
                                            )),
                                    )
                                    .child(
                                        div()
                                            .id("audio_volume_up_btn")
                                            .bg(rgb(0x242a33))
                                            .p_2()
                                            .rounded_sm()
                                            .text_center()
                                            .text_sm()
                                            .text_color(rgb(0xffffff))
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                cx.listener(|this, _event, _window, cx| {
                                                    this.remote_audio_volume_percent =
                                                        (this.remote_audio_volume_percent + 10)
                                                            .min(200);
                                                    this.apply_audio_playback_settings();
                                                    cx.notify();
                                                }),
                                            )
                                            .child("+"),
                                    ),
                            ),
                    );

                    if self.controls.talkback.is_some() {
                        panel = panel.child(panel_section("Talkback"));
                        panel = panel.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .mb_2()
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id("talkback_off_btn")
                                                .bg(control_fill(
                                                    self.talkback_mode == TalkbackMode::Off,
                                                ))
                                                .flex_1()
                                                .p_2()
                                                .rounded_sm()
                                                .text_center()
                                                .text_sm()
                                                .text_color(rgb(0xffffff))
                                                .cursor_pointer()
                                                .on_mouse_down(
                                                    gpui::MouseButton::Left,
                                                    cx.listener(|this, _event, _window, cx| {
                                                        this.talkback_mode = TalkbackMode::Off;
                                                        this.talkback_push_active = false;
                                                        this.sync_talkback_session();
                                                        cx.notify();
                                                    }),
                                                )
                                                .child("Off"),
                                        )
                                        .child(
                                            div()
                                                .id("talkback_always_btn")
                                                .bg(control_fill(
                                                    self.talkback_mode == TalkbackMode::AlwaysOn,
                                                ))
                                                .flex_1()
                                                .p_2()
                                                .rounded_sm()
                                                .text_center()
                                                .text_sm()
                                                .text_color(rgb(0xffffff))
                                                .cursor_pointer()
                                                .on_mouse_down(
                                                    gpui::MouseButton::Left,
                                                    cx.listener(|this, _event, _window, cx| {
                                                        this.talkback_mode = TalkbackMode::AlwaysOn;
                                                        this.talkback_push_active = false;
                                                        this.sync_talkback_session();
                                                        cx.notify();
                                                    }),
                                                )
                                                .child("Always"),
                                        )
                                        .child(
                                            div()
                                                .id("talkback_ptt_btn")
                                                .bg(control_fill(
                                                    self.talkback_mode == TalkbackMode::PushToTalk,
                                                ))
                                                .flex_1()
                                                .p_2()
                                                .rounded_sm()
                                                .text_center()
                                                .text_sm()
                                                .text_color(rgb(0xffffff))
                                                .cursor_pointer()
                                                .on_mouse_down(
                                                    gpui::MouseButton::Left,
                                                    cx.listener(|this, _event, _window, cx| {
                                                        this.talkback_mode =
                                                            TalkbackMode::PushToTalk;
                                                        this.talkback_push_active = false;
                                                        this.sync_talkback_session();
                                                        cx.notify();
                                                    }),
                                                )
                                                .child("PTT"),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id("talkback_local_mic_mute_btn")
                                                .bg(toggle_fill(!self.talkback_local_mic_muted))
                                                .flex_1()
                                                .p_2()
                                                .rounded_sm()
                                                .text_center()
                                                .text_sm()
                                                .text_color(rgb(0xffffff))
                                                .cursor_pointer()
                                                .on_mouse_down(
                                                    gpui::MouseButton::Left,
                                                    cx.listener(|this, _event, _window, cx| {
                                                        this.talkback_local_mic_muted =
                                                            !this.talkback_local_mic_muted;
                                                        this.apply_talkback_settings();
                                                        cx.notify();
                                                    }),
                                                )
                                                .child(if self.talkback_local_mic_muted {
                                                    "Local Mic Muted"
                                                } else {
                                                    "Local Mic On"
                                                }),
                                        )
                                        .child(
                                            div()
                                                .id("talkback_hold_btn")
                                                .bg(
                                                    if self.talkback_mode
                                                        == TalkbackMode::PushToTalk
                                                        && self.talkback_push_active
                                                    {
                                                        control_fill(true)
                                                    } else {
                                                        rgb(0x242a33)
                                                    },
                                                )
                                                .flex_1()
                                                .p_2()
                                                .rounded_sm()
                                                .text_center()
                                                .text_sm()
                                                .text_color(rgb(0xffffff))
                                                .cursor_pointer()
                                                .on_mouse_down(
                                                    gpui::MouseButton::Left,
                                                    cx.listener(|this, _event, _window, cx| {
                                                        if this.talkback_mode
                                                            == TalkbackMode::PushToTalk
                                                        {
                                                            this.talkback_push_active = true;
                                                            this.apply_talkback_settings();
                                                            cx.notify();
                                                        }
                                                    }),
                                                )
                                                .on_mouse_up(
                                                    gpui::MouseButton::Left,
                                                    cx.listener(|this, _event, _window, cx| {
                                                        if this.talkback_push_active {
                                                            this.talkback_push_active = false;
                                                            this.apply_talkback_settings();
                                                            cx.notify();
                                                        }
                                                    }),
                                                )
                                                .on_mouse_up_out(
                                                    gpui::MouseButton::Left,
                                                    cx.listener(|this, _event, _window, cx| {
                                                        if this.talkback_push_active {
                                                            this.talkback_push_active = false;
                                                            this.apply_talkback_settings();
                                                            cx.notify();
                                                        }
                                                    }),
                                                )
                                                .child("Hold"),
                                        ),
                                ),
                        );
                        panel = panel.child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .mb_2()
                                .items_center()
                                .child(
                                    div()
                                        .id("talkback_remote_mute_btn")
                                        .bg(toggle_fill(!self.talkback_remote_muted))
                                        .flex_1()
                                        .p_2()
                                        .rounded_sm()
                                        .text_center()
                                        .text_sm()
                                        .text_color(rgb(0xffffff))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _event, _window, cx| {
                                                this.talkback_remote_muted =
                                                    !this.talkback_remote_muted;
                                                this.send_talkback_playback_control(cx);
                                                cx.notify();
                                            }),
                                        )
                                        .child(if self.talkback_remote_muted {
                                            "Peer Muted"
                                        } else {
                                            "Peer On"
                                        }),
                                )
                                .child(
                                    div()
                                        .id("talkback_remote_volume_down_btn")
                                        .bg(rgb(0x242a33))
                                        .p_2()
                                        .rounded_sm()
                                        .text_center()
                                        .text_sm()
                                        .text_color(rgb(0xffffff))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _event, _window, cx| {
                                                this.talkback_remote_volume_percent = this
                                                    .talkback_remote_volume_percent
                                                    .saturating_sub(10);
                                                this.send_talkback_playback_control(cx);
                                                cx.notify();
                                            }),
                                        )
                                        .child("-"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(0xd8dee9))
                                        .font_family("Courier")
                                        .child(format!(
                                            "Peer {}%",
                                            self.talkback_remote_volume_percent
                                        )),
                                )
                                .child(
                                    div()
                                        .id("talkback_remote_volume_up_btn")
                                        .bg(rgb(0x242a33))
                                        .p_2()
                                        .rounded_sm()
                                        .text_center()
                                        .text_sm()
                                        .text_color(rgb(0xffffff))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _event, _window, cx| {
                                                this.talkback_remote_volume_percent =
                                                    (this.talkback_remote_volume_percent + 10)
                                                        .min(200);
                                                this.send_talkback_playback_control(cx);
                                                cx.notify();
                                            }),
                                        )
                                        .child("+"),
                                ),
                        );
                    }

                    if !transfer_entries.is_empty() {
                        panel = panel.child(panel_section("Transfers"));

                        for (index, entry) in transfer_entries.into_iter().take(4).enumerate() {
                            panel = panel.child(render_transfer_entry(
                                entry,
                                index,
                                self.controls.file_transfer.clone(),
                                cx,
                            ));
                        }
                    }

                    if let Some(file_control) = self.controls.file_transfer.clone() {
                        let receive_dir = file_control.receive_dir();
                        let receive_dir_label = compact_path(&receive_dir, 42);
                        let allow_overwrite = file_control.allow_overwrite();
                        let receive_control_folder = file_control.clone();
                        let receive_control_overwrite = file_control.clone();
                        let file_control_file = file_control.clone();
                        let file_control_folder = file_control;
                        panel = panel.child(panel_section("Receive"));
                        panel = panel.child(
                            div()
                                .mb_2()
                                .p_2()
                                .rounded_sm()
                                .bg(rgba(0x20252ddd))
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(0xd8dee9))
                                        .font_family("Courier")
                                        .child(receive_dir_label),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id("choose_receive_folder_btn")
                                                .bg(rgb(0x242a33))
                                                .p_2()
                                                .rounded_sm()
                                                .text_center()
                                                .text_sm()
                                                .text_color(rgb(0xffffff))
                                                .cursor_pointer()
                                                .on_mouse_down(
                                                    gpui::MouseButton::Left,
                                                    cx.listener(
                                                        move |_this, _event, _window, _cx| {
                                                            let control =
                                                                receive_control_folder.clone();
                                                            std::thread::spawn(move || {
                                                                if let Some(path) =
                                                                    rfd::FileDialog::new()
                                                                        .pick_folder()
                                                                {
                                                                    control.set_receive_dir(path);
                                                                }
                                                            });
                                                        },
                                                    ),
                                                )
                                                .child("Folder"),
                                        )
                                        .child(
                                            div()
                                                .id("overwrite_toggle_btn")
                                                .bg(control_fill(allow_overwrite))
                                                .p_2()
                                                .rounded_sm()
                                                .text_center()
                                                .text_sm()
                                                .text_color(rgb(0xffffff))
                                                .cursor_pointer()
                                                .on_mouse_down(
                                                    gpui::MouseButton::Left,
                                                    cx.listener(
                                                        move |_this, _event, _window, cx| {
                                                            receive_control_overwrite
                                                                .set_allow_overwrite(
                                                                    !allow_overwrite,
                                                                );
                                                            cx.notify();
                                                        },
                                                    ),
                                                )
                                                .child(if allow_overwrite {
                                                    "Overwrite On"
                                                } else {
                                                    "Overwrite Off"
                                                }),
                                        ),
                                ),
                        );
                        panel = panel.child(panel_section("Send"));
                        panel = panel.child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .mb_2()
                                .child(
                                    div()
                                        .id("send_file_btn")
                                        .bg(rgb(0x2563eb))
                                        .flex_1()
                                        .p_2()
                                        .rounded_sm()
                                        .text_center()
                                        .text_sm()
                                        .text_color(rgb(0xffffff))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(move |_this, _event, _window, _cx| {
                                                let control = file_control_file.clone();
                                                std::thread::spawn(move || {
                                                    if let Some(paths) =
                                                        rfd::FileDialog::new().pick_files()
                                                    {
                                                        if paths.len() == 1 {
                                                            if let Some(path) =
                                                                paths.into_iter().next()
                                                            {
                                                                control.send_file(path, None);
                                                            }
                                                        } else if !paths.is_empty() {
                                                            control.send_file_group(paths);
                                                        }
                                                    }
                                                });
                                            }),
                                        )
                                        .child("Files"),
                                )
                                .child(
                                    div()
                                        .id("send_folder_btn")
                                        .bg(rgb(0x2563eb))
                                        .flex_1()
                                        .p_2()
                                        .rounded_sm()
                                        .text_center()
                                        .text_sm()
                                        .text_color(rgb(0xffffff))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(move |_this, _event, _window, _cx| {
                                                let control = file_control_folder.clone();
                                                std::thread::spawn(move || {
                                                    if let Some(path) =
                                                        rfd::FileDialog::new().pick_folder()
                                                    {
                                                        control.send_file_group(vec![path]);
                                                    }
                                                });
                                            }),
                                        )
                                        .child("Folder"),
                                ),
                        );
                    }

                    panel = panel.child(
                        div()
                            .id("disconnect_btn")
                            .mt_4()
                            .bg(rgb(0x7a2e34))
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
                                    if let Some(control) = &this.controls.clipboard {
                                        control.stop();
                                    }
                                    if let Some(control) = &this.controls.file_transfer {
                                        control.stop();
                                    }
                                    this.stop_talkback_session();
                                    this.active_session_id
                                        .store(0, std::sync::atomic::Ordering::Relaxed);
                                    cx.notify();
                                }),
                            )
                            .child("Disconnect"),
                    );

                    layout = layout.child(panel);
                }

                if self.show_stats {
                    let hs = self.host_stats.snapshot();
                    layout = layout.child(
                        div()
                            .absolute()
                            .top_4()
                            .left_4()
                            .p_4()
                            .bg(rgba(0x1e1e2ecc))
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
                                            .child(format!(
                                                "Bitrate: {:.1} Mbps",
                                                hs.bitrate_kbps as f32 / 1000.0
                                            ))
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

fn render_transfer_entry(
    entry: TransferEntrySnapshot,
    index: usize,
    control: Option<crate::FileTransferRuntimeControl>,
    cx: &mut gpui::Context<RemotePlayView>,
) -> AnyElement {
    let fill_width = 216.0 * entry.progress();
    let direction = match entry.direction {
        TransferDirection::Outgoing => "Send",
        TransferDirection::Incoming => "Receive",
    };
    let status = transfer_status_text(entry.status);
    let status_color = transfer_status_color(entry.status);
    let bytes = format!(
        "{} / {}",
        format_bytes(entry.transferred_bytes),
        format_bytes(entry.total_bytes)
    );
    let cancel_target = entry.cancel_target;
    let mut row = div()
        .id(format!("transfer-entry-{index}"))
        .mb_2()
        .p_2()
        .rounded_sm()
        .bg(rgba(0x2a2a2add))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xffffff))
                        .child(format!("{direction} {}", entry.label)),
                )
                .child(div().text_xs().text_color(status_color).child(status)),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(0xaaaaaa))
                .child(if entry.detail.is_empty() {
                    bytes.clone()
                } else {
                    format!("{} | {}", entry.detail, bytes)
                }),
        )
        .child(
            div()
                .w(px(216.0))
                .h(px(4.0))
                .rounded_sm()
                .bg(rgb(0x444444))
                .child(
                    div()
                        .w(px(fill_width))
                        .h(px(4.0))
                        .rounded_sm()
                        .bg(rgb(0x3ba55d)),
                ),
        );

    if let Some(cancel_target) = cancel_target
        && entry.is_running()
    {
        row = row.child(
            div()
                .id(format!("transfer-cancel-{index}"))
                .mt_1()
                .w(px(72.0))
                .bg(rgb(0x8f2f2f))
                .rounded_sm()
                .p_1()
                .text_center()
                .text_xs()
                .text_color(rgb(0xffffff))
                .cursor_pointer()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |_this, _event, _window, cx| {
                        if let Some(control) = &control {
                            match cancel_target {
                                TransferCancelTarget::Transfer(transfer_id) => {
                                    control.cancel_transfer(transfer_id);
                                }
                                TransferCancelTarget::Group(group_id) => {
                                    control.cancel_group(group_id);
                                }
                            }
                        }
                        cx.notify();
                    }),
                )
                .child("Cancel"),
        );
    }

    row.into_any_element()
}

fn transfer_status_text(status: TransferStatus) -> &'static str {
    match status {
        TransferStatus::Running => "Running",
        TransferStatus::Completed => "Done",
        TransferStatus::Cancelled => "Cancelled",
        TransferStatus::Error => "Error",
    }
}

fn transfer_status_color(status: TransferStatus) -> Rgba {
    match status {
        TransferStatus::Running => rgb(0x89b4fa),
        TransferStatus::Completed => rgb(0xa6e3a1),
        TransferStatus::Cancelled => rgb(0xf9e2af),
        TransferStatus::Error => rgb(0xf38ba8),
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes_f = bytes as f64;
    if bytes_f >= GIB {
        format!("{:.1} GiB", bytes_f / GIB)
    } else if bytes_f >= MIB {
        format!("{:.1} MiB", bytes_f / MIB)
    } else if bytes_f >= KIB {
        format!("{:.1} KiB", bytes_f / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn compact_path(path: &Path, max_chars: usize) -> String {
    let path = path.display().to_string();
    let char_count = path.chars().count();
    if char_count <= max_chars || max_chars <= 3 {
        return path;
    }

    let tail_len = max_chars.saturating_sub(3);
    let mut tail = path.chars().rev().take(tail_len).collect::<Vec<_>>();
    tail.reverse();
    format!("...{}", tail.into_iter().collect::<String>())
}
