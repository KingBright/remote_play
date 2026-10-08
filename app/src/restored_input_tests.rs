//! Opt-in full restored GPUI event-routing regression. Uses a local packet recorder,
//! never a real host, native screen capture, device identity, or OS input injection.
use super::{
    DrawerTab, GlobalTheme, RestoredDashboard, UnifiedRuntimeConfig, component, remote_play_themes,
    start_unified_runtime,
};
use crate::desktop::OriginalGuiSession;
use gpui::{
    KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseButton, TestAppContext, WindowAppearance,
    point, px, size,
};
use protocol::{ControlMessage, InputEvent, session::SessionCommand};
use remote_core::{
    net::{MultiplexedPacket, UdpMultiplexer},
    workspace_session::WorkspaceConnection,
};
use std::{
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Duration,
};

#[gpui::test]
#[ignore = "local protocol recorder; run explicitly without any real user session key"]
fn restored_gpui_preserves_press_release_keyboard_and_overlay_isolation(cx: &mut TestAppContext) {
    assert!(
        remote_core::session_crypto::load_session_psk().is_none(),
        "Do not inherit a user's network key"
    );
    remote_core::init_crypto_provider();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let _entered = rt.enter();
    let output = Arc::new(Mutex::new(Vec::new()));
    let events_out = output.clone();
    let temp = tempfile::tempdir().unwrap();
    let (runtime, session, recorder, recovery) = rt.block_on(async {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let addr = mux.local_addr().unwrap();
        let (sender, rx) = mux.split();
        let recorder = tokio::spawn(async move {
            while let Ok(packet) = rx.recv().await {
                match packet {
                    MultiplexedPacket::Control(ControlMessage::Session(cmd), from) => {
                        if let SessionCommand::Open { connection_id, .. } = *cmd {
                            sender
                                .send_control(
                                    &ControlMessage::Session(Box::new(SessionCommand::Opened {
                                        connection_id,
                                        version: protocol::session::SESSION_VERSION,
                                        max_subscriptions: 8,
                                        files: false,
                                        clipboard: false,
                                        window_capture: false,
                                    })),
                                    from,
                                )
                                .await
                                .unwrap();
                        } else {
                            events_out.lock().unwrap().push(*cmd);
                        }
                    }
                    MultiplexedPacket::Control(ControlMessage::Ping { client_send_ts }, from) => {
                        sender
                            .send_control(
                                &ControlMessage::Pong {
                                    client_send_ts,
                                    host_recv_ts: client_send_ts,
                                    host_send_ts: client_send_ts,
                                },
                                from,
                            )
                            .await
                            .unwrap();
                    }
                    _ => {}
                }
            }
        });
        let (conn, events) = WorkspaceConnection::connect(addr, temp.path().join("receive"))
            .await
            .unwrap();
        let mut cfg = UnifiedRuntimeConfig::app_defaults();
        cfg.mesh_dir = temp.path().join("network");
        cfg.enable_discovery = false;
        cfg.enable_passive_host = false;
        cfg.enable_client_receiver = false;
        cfg.enable_viewer_media = false;
        cfg.enable_workspace_viewer = false;
        cfg.enable_session_timeout_monitor = false;
        cfg.enable_p2p = false;
        cfg.relay_endpoint = None;
        cfg.enable_clipboard_sync = false;
        cfg.enable_file_transfer = false;
        cfg.enable_talkback = false;
        let runtime = Arc::new(start_unified_runtime(cfg).await.unwrap());
        let mut session = OriginalGuiSession::new(
            "local-test-only".into(),
            "Input packet recorder".into(),
            "Loopback".into(),
            conn,
            events,
        );
        session.media =
            Some(client::ClientMediaRuntime::start_video_only(session.stats.clone()).unwrap());
        let recovery = session.stats.clone();
        // This fixture supplies a displayed-frame precondition rather than real
        // compressed video. Wait for the decoder task to publish its startup
        // state before simulating a successful decode; otherwise it races the
        // fake displayed frame and correctly prevents all input.
        tokio::time::timeout(Duration::from_secs(1), async {
            while !recovery
                .video_decoder_needs_keyframe
                .load(std::sync::atomic::Ordering::Acquire)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("local decoder task did not initialize");
        recovery
            .video_decoder_needs_keyframe
            .store(false, std::sync::atomic::Ordering::Release);
        (runtime, session, recorder, recovery)
    });
    cx.update(|cx| {
        component::init(cx);
        cx.set_global(GlobalTheme::new_with_themes(
            WindowAppearance::Dark,
            remote_play_themes(),
        ));
    });
    let (view, cx) = cx.add_window_view(move |window, cx| {
        let mut view =
            RestoredDashboard::new(runtime, None, Arc::new(AtomicBool::new(false)), window, cx);
        view.owner.attach_recorder_session(session);
        view.pause_when_inactive = false;
        view.drawer_open = false;
        view.toolbar_revealed = false;
        view.input_locked = false;
        view
    });
    cx.simulate_resize(size(px(1000.), px(700.)));
    cx.run_until_parked();
    // Initial setup can issue source discovery; clear that before the event trace.
    std::thread::sleep(Duration::from_millis(60));
    output.lock().unwrap().clear();
    let pos = point(px(500.), px(350.));
    cx.simulate_mouse_down(pos, MouseButton::Left, Modifiers::none());
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    cx.simulate_mouse_move(
        point(px(550.), px(360.)),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    cx.simulate_mouse_up(
        point(px(550.), px(360.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_event(KeyDownEvent {
        keystroke: Keystroke::parse("a").unwrap(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse("a").unwrap(),
    });
    std::thread::sleep(Duration::from_millis(150));
    let trace = output.lock().unwrap().clone();
    println!("RESTORED_GPUI_PROTOCOL_TRACE {trace:?}");
    let filtered: Vec<_> = trace
        .iter()
        .filter_map(|command| match command {
            SessionCommand::Input { event, .. } | SessionCommand::SourceInput { event, .. } => {
                Some(event.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        filtered.contains(&InputEvent::MouseDown(0)),
        "GPUI swallowed the press"
    );
    assert!(
        filtered.contains(&InputEvent::MouseUp(0)),
        "GPUI swallowed the release"
    );
    assert!(
        filtered.contains(&InputEvent::Key {
            key_code: 0,
            pressed: true,
            modifiers: 0
        }),
        "Keyboard down did not traverse the production root handler"
    );
    assert!(
        filtered.contains(&InputEvent::Key {
            key_code: 0,
            pressed: false,
            modifiers: 0
        }),
        "Keyboard up was swallowed"
    );
    assert!(
        !trace
            .iter()
            .any(|c| matches!(c, SessionCommand::ReleaseInput { .. })),
        "Render cycle released held input before the up event"
    );
    // Local device drawer must intercept its own click and all keyboard input.
    view.update(cx, |v, cx| {
        v.drawer_open = true;
        v.active_tab = DrawerTab::Devices;
        v.owner.release_input();
        cx.notify();
    });
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(60));
    output.lock().unwrap().clear();
    cx.simulate_click(point(px(120.), px(300.)), Modifiers::none());
    cx.simulate_event(KeyDownEvent {
        keystroke: Keystroke::parse("b").unwrap(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse("b").unwrap(),
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !output.lock().unwrap().iter().any(|c| matches!(
            c,
            SessionCommand::Input { .. } | SessionCommand::SourceInput { .. }
        )),
        "Local overlay leaked input into the remote device"
    );
    view.update(cx, |v, cx| {
        v.drawer_open = false;
        v.show_apps_menu = true;
        v.owner.release_input();
        cx.notify();
    });
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(60));
    output.lock().unwrap().clear();
    cx.simulate_event(KeyDownEvent {
        keystroke: Keystroke::parse("down").unwrap(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse("down").unwrap(),
    });
    cx.simulate_click(pos, Modifiers::none());
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !output.lock().unwrap().iter().any(|c| matches!(
            c,
            SessionCommand::Input { .. } | SessionCommand::SourceInput { .. }
        )),
        "Dismissing the apps menu leaked a remote click or key"
    );
    // Explicit locking leaves picture navigation available but disables remote input.
    view.update(cx, |v, cx| {
        v.drawer_open = false;
        v.set_input_locked(true);
        cx.notify();
    });
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(50));
    output.lock().unwrap().clear();
    cx.simulate_click(pos, Modifiers::none());
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !output.lock().unwrap().iter().any(|c| matches!(
            c,
            SessionCommand::Input { .. } | SessionCommand::SourceInput { .. }
        )),
        "Locked view emitted input"
    );
    // Recovery must still cancel an active drag even with Enable control on.
    // Do not make the test green by weakening production readiness checks.
    view.update(cx, |v, cx| {
        v.drawer_open = false;
        v.show_apps_menu = false;
        v.set_input_locked(false);
        cx.notify();
    });
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(60));
    output.lock().unwrap().clear();
    cx.simulate_mouse_down(pos, MouseButton::Left, Modifiers::none());
    std::thread::sleep(Duration::from_millis(60));
    assert!(
        output.lock().unwrap().iter().any(|c| matches!(
            c,
            SessionCommand::Input {
                event: InputEvent::MouseDown(0),
                ..
            } | SessionCommand::SourceInput {
                event: InputEvent::MouseDown(0),
                ..
            }
        )),
        "recovery fixture drag never began"
    );
    recovery
        .video_decoder_needs_keyframe
        .store(true, std::sync::atomic::Ordering::Release);
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(60));
    assert!(
        output
            .lock()
            .unwrap()
            .iter()
            .any(|c| matches!(c, SessionCommand::ReleaseInput { .. })),
        "reference recovery did not release held input"
    );
    output.lock().unwrap().clear();
    cx.simulate_mouse_up(pos, MouseButton::Left, Modifiers::none());
    cx.simulate_event(KeyDownEvent {
        keystroke: Keystroke::parse("c").unwrap(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse("c").unwrap(),
    });
    std::thread::sleep(Duration::from_millis(60));
    assert!(
        !output.lock().unwrap().iter().any(|c| matches!(
            c,
            SessionCommand::Input { .. } | SessionCommand::SourceInput { .. }
        )),
        "recovering video accepted remote input"
    );
    recovery
        .video_decoder_needs_keyframe
        .store(false, std::sync::atomic::Ordering::Release);
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    cx.simulate_click(pos, Modifiers::none());
    std::thread::sleep(Duration::from_millis(60));
    assert!(
        !output.lock().unwrap().iter().any(|c| matches!(
            c,
            SessionCommand::Input { .. } | SessionCommand::SourceInput { .. }
        )),
        "decoder readiness without a newly displayed frame resumed input"
    );
    view.update(cx, |v, _| v.owner.close_all());
    recorder.abort();
}
