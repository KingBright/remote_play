//! Private-bus target effects; these are not desktop portal approval receipts.
use super::*;
use crate::linux_frame::{PixelFormat, Transform};
use protocol::{InputEvent, input_modifiers};

fn frame(generation: u64) -> OwnedFrame {
    let mut frame = crate::linux_raw_encode::tests::frame(
        PixelFormat::Nv12,
        crate::linux_raw_encode::tests::known_color(),
        1,
    );
    frame.stamp.generation = generation;
    // 2x compositor scale, unrelated to stream origin (-640, 0).
    frame.width = 1280;
    frame.height = 720;
    frame.source_format.width = 1280;
    frame.source_format.height = 720;
    frame.planes = vec![vec![0; 1280 * 720], vec![128; 1280 * 720 / 2]];
    frame
}
pub(super) async fn control(fixture: &Fixture) -> PreparedPortalCapture {
    let (cancel, rx) = watch::channel(false);
    let mut lease = select_with_control(
        fixture.client.clone(),
        "x11:2a",
        PickerSources::Monitor,
        rx,
        fixture.timeouts(),
        true,
    )
    .await
    .unwrap();
    assert!(
        !lease.input_available(),
        "a Start reply is not a validated frame binding"
    );
    *fixture.cancel_keepalive.lock().unwrap() = Some(cancel);
    let (_, fd) = lease.take_pipewire_remote().unwrap();
    drop(fd);
    let mut first = frame(lease.selected.generation);
    if let Some((width, height)) = lease.selected.logical_size {
        first.width = width as u32 * 2;
        first.height = height as u32 * 2;
        first.source_format.width = first.width;
        first.source_format.height = first.height;
        let pixels = first.width as usize * first.height as usize;
        first.planes = vec![vec![0; pixels], vec![128; pixels / 2]];
    }
    lease.prepare_owned_capture(first).unwrap()
}

#[tokio::test]
async fn consented_session_binds_absolute_coordinates_and_changes_target_text() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
    let capture = control(&fixture).await;
    assert!(capture.input_available());
    assert!(capture.source_info().supports_input);
    assert_eq!(fixture.calls.count("rd_create"), 1);
    assert_eq!(fixture.calls.count("devices"), 1);
    assert_eq!(fixture.calls.count("rd_start"), 1);
    for (x, expected_focus) in [(0, 0), (u16::MAX, 1)] {
        capture
            .inject_input(InputEvent::MouseMoveAbsolute { x, y: u16::MAX })
            .await
            .unwrap();
        capture
            .inject_input(InputEvent::MouseDown(0))
            .await
            .unwrap();
        capture.inject_input(InputEvent::MouseUp(0)).await.unwrap();
        capture
            .inject_input(InputEvent::Key {
                key_code: 0,
                pressed: true,
                modifiers: input_modifiers::SHIFT,
            })
            .await
            .unwrap();
        capture
            .inject_input(InputEvent::Key {
                key_code: 0,
                pressed: false,
                modifiers: input_modifiers::SHIFT,
            })
            .await
            .unwrap();
        let target = fixture.calls.target.lock().unwrap();
        assert_eq!(target.focus, expected_focus);
        assert_eq!(target.text[expected_focus], "A");
        assert_eq!(
            target.position,
            Some((77, if x == 0 { 0.0 } else { 639.0 }, 359.0))
        );
    }
    capture
        .inject_input(InputEvent::MouseScroll {
            delta_x: 12,
            delta_y: -24,
        })
        .await
        .unwrap();
    assert_eq!(fixture.calls.target.lock().unwrap().scroll, (12.0, -24.0));
    capture.release_input().await.unwrap();
    assert!(fixture.calls.target.lock().unwrap().keys.is_empty());
    capture.revoke();
}

#[tokio::test]
async fn partial_or_denied_device_grants_never_enable_input() {
    for devices in [0, 1, 2] {
        let fixture = Fixture::with_devices(Behavior::EarlyResponse, 1, devices).await;
        let capture = control(&fixture).await;
        assert!(!capture.source_info().supports_input);
        assert_eq!(
            capture.inject_input(InputEvent::KeyDown(0)).await,
            Err(PortalInputError::DevicePermission)
        );
        assert!(
            fixture
                .calls
                .target
                .lock()
                .unwrap()
                .text
                .iter()
                .all(String::is_empty)
        );
        capture.revoke();
    }
}

#[tokio::test]
async fn view_only_and_window_selection_cannot_fall_back_to_desktop_input() {
    let (fixture, capture) = prepared_fixture().await;
    assert!(!capture.input_available());
    assert_eq!(
        capture.inject_input(InputEvent::MouseDown(0)).await,
        Err(PortalInputError::ViewOnly)
    );
    assert_eq!(fixture.calls.count("rd_create"), 0);
    let (_cancel, rx) = watch::channel(false);
    assert!(matches!(
        select_with_control(
            fixture.client.clone(),
            "x11:2a",
            PickerSources::Window,
            rx,
            fixture.timeouts(),
            true
        )
        .await,
        Err(PortalCallError::Capabilities)
    ));
    assert_eq!(fixture.calls.count("rd_create"), 0);
    capture.revoke();
}

#[tokio::test]
async fn disconnect_releases_held_buttons_modifiers_and_keys_before_session_close() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
    let capture = control(&fixture).await;
    capture
        .inject_input(InputEvent::MouseDown(0))
        .await
        .unwrap();
    capture
        .inject_input(InputEvent::Key {
            key_code: 0,
            pressed: true,
            modifiers: input_modifiers::SHIFT | input_modifiers::CONTROL,
        })
        .await
        .unwrap();
    fixture
        .cancel_keepalive
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .send_replace(true);
    fixture.calls.wait("session_close").await;
    assert!(!capture.input_available());
    let target = fixture.calls.target.lock().unwrap();
    assert!(target.keys.is_empty() && target.buttons.is_empty());
    let events = fixture.calls.events.lock().unwrap();
    assert!(
        events.iter().rposition(|e| e == "key_up").unwrap()
            < events.iter().position(|e| e == "session_close").unwrap()
    );
}

#[tokio::test]
async fn resize_epoch_disables_old_mapping_and_releases_keys_without_stopping_video() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
    let capture = control(&fixture).await;
    capture.inject_input(InputEvent::KeyDown(0)).await.unwrap();
    let mut next = frame(capture.generation());
    next.stamp.format_revision += 1;
    next.stamp.sequence = Some(2);
    capture.publish_owned(next).unwrap();
    assert!(!capture.is_revoked(), "video resize must remain available");
    assert!(!capture.source_info().supports_input);
    assert_eq!(
        capture.inject_input(InputEvent::MouseDown(0)).await,
        Err(PortalInputError::FrameBinding)
    );
    capture.release_input().await.unwrap();
    assert!(fixture.calls.target.lock().unwrap().keys.is_empty());
    capture.revoke();
}

#[tokio::test]
async fn source_switch_does_not_inherit_pressed_keys_or_authorize_the_old_generation() {
    let first_fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
    let first = control(&first_fixture).await;
    first.inject_input(InputEvent::KeyDown(0)).await.unwrap();
    first.revoke();
    first_fixture.calls.wait("session_close").await;
    let second_fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
    let second = control(&second_fixture).await;
    assert_ne!(first.generation(), second.generation());
    assert_eq!(
        first.inject_input(InputEvent::KeyDown(0)).await,
        Err(PortalInputError::SessionClosed)
    );
    assert!(first_fixture.calls.target.lock().unwrap().keys.is_empty());
    assert!(second_fixture.calls.target.lock().unwrap().keys.is_empty());
    second.revoke();
}

#[tokio::test]
async fn unknown_geometry_and_unhandled_raw_rotation_deny_input() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
    let capture = control(&fixture).await;
    let source = SelectedPortalSource {
        generation: capture.generation(),
        kind: SourceKind::Monitor,
        node_id: 77,
        pipewire_serial: None,
        logical_size: None,
        logical_position: Some((-640, 0)),
        embedded_cursor: true,
    };
    let (_cancel, cancel_rx) = watch::channel(false);
    let unknown = input::PortalInput::new(
        fixture.client.clone(),
        fixture.service.unique_name().unwrap().to_string(),
        OwnedObjectPath::try_from(fixture.calls.sessions.lock().unwrap()[0].clone()).unwrap(),
        source,
        3,
        capture.revocation(),
        cancel_rx.clone(),
    );
    unknown.bind_frame(&frame(source.generation));
    assert!(!unknown.available());
    let mut rotated = frame(source.generation);
    rotated.source_format.transform = Transform::Unsupported(1);
    let rotated_input = input::PortalInput::new(
        fixture.client.clone(),
        fixture.service.unique_name().unwrap().to_string(),
        OwnedObjectPath::try_from(fixture.calls.sessions.lock().unwrap()[0].clone()).unwrap(),
        SelectedPortalSource {
            logical_size: Some((360, 640)),
            ..source
        },
        3,
        capture.revocation(),
        cancel_rx,
    );
    rotated_input.bind_frame(&rotated);
    assert!(!rotated_input.available());
    capture.revoke();
}

#[tokio::test]
async fn screencast_v6_requires_stable_serial_for_control_sessions() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
    fixture.calls.version.store(6, Ordering::Relaxed);
    let (_cancel, rx) = watch::channel(false);
    let result = select_with_control(
        fixture.client.clone(),
        "x11:2a",
        PickerSources::Monitor,
        rx,
        fixture.timeouts(),
        true,
    )
    .await;
    assert!(matches!(result, Err(PortalCallError::InvalidSource)));
    assert_eq!(fixture.calls.count("open_remote"), 0);
    fixture.calls.wait("session_close").await;
    let fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
    fixture.calls.version.store(6, Ordering::Relaxed);
    fixture.calls.serial.store(987654321, Ordering::Relaxed);
    let capture = control(&fixture).await;
    let source = capture.inner.lease.lock().unwrap().selected;
    assert_eq!(source.pipewire_serial, Some(987654321));
    assert!(capture.input_available());
    capture.revoke();
}

#[tokio::test]
async fn compositor_oriented_portrait_display_uses_logical_axes_at_double_scale() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
    *fixture.calls.geometry.lock().unwrap() = Some((360, 640));
    let capture = control(&fixture).await;
    assert_eq!(
        (capture.source_info().width, capture.source_info().height),
        (720, 1280)
    );
    capture
        .inject_input(InputEvent::MouseMoveAbsolute {
            x: u16::MAX,
            y: u16::MAX,
        })
        .await
        .unwrap();
    assert_eq!(
        fixture.calls.target.lock().unwrap().position,
        Some((77, 359.0, 639.0))
    );
    capture.revoke();
}

#[tokio::test]
async fn failed_modifier_rpc_is_an_error_releases_observed_down_and_disables_backend() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
    let capture = control(&fixture).await;
    fixture.calls.fail_key_down.store(true, Ordering::Relaxed);
    assert_eq!(
        capture
            .inject_input(InputEvent::Key {
                key_code: 0,
                pressed: true,
                modifiers: input_modifiers::SHIFT
            })
            .await,
        Err(PortalInputError::Transport)
    );
    assert!(!capture.source_info().supports_input);
    assert_eq!(capture.input_status(), Err(PortalInputError::Transport));
    assert!(fixture.held_input_empty());
    assert!(fixture.target_text().iter().all(String::is_empty));
    capture.revoke();
}
