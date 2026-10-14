//! Managed-worker private-bus acceptance; no desktop bus or permission writes.
use super::*;
struct Registry(Arc<Calls>);
#[zbus::interface(name = "org.freedesktop.host.portal.Registry")]
impl Registry {
    fn register(&self, app_id: String, options: Values, #[zbus(header)] header: Header<'_>) {
        assert_eq!(app_id, "io.github.KingBright.RemoteHosts.DesktopWorker");
        assert!(options.is_empty());
        assert_eq!(self.0.count("create"), 0);
        *self.0.registry_sender.lock().unwrap() = Some(header.sender().unwrap().as_str().into());
        self.0.record("register");
    }
}
async fn registered(fixture: &Fixture, rx: watch::Receiver<bool>) -> Result<PortalCaptureLease> {
    fixture
        .service
        .object_server()
        .at(DESKTOP_PATH, Registry(fixture.calls.clone()))
        .await
        .unwrap();
    registered_on_connection(
        fixture.client.clone(),
        "io.github.KingBright.RemoteHosts.DesktopWorker",
        "x11:2a",
        rx,
        fixture.timeouts(),
    )
    .await
}
#[tokio::test]
async fn worker_registry_same_connection_nonpersistent_chain() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
    let (_tx, rx) = watch::channel(false);
    let mut lease = registered(&fixture, rx).await.unwrap();
    let sender = fixture
        .calls
        .registry_sender
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    assert_eq!(&sender, fixture.client.unique_name().unwrap().as_str());
    assert!(
        fixture
            .calls
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, s)| s == &sender)
    );
    assert_eq!(fixture.calls.events.lock().unwrap()[0], "register");
    assert_eq!(fixture.calls.count("devices"), 1);
    assert!(lease.close().await);
}
#[tokio::test]
async fn worker_rejects_restore_and_clipboard_results() {
    for mode in [1, 2] {
        let fixture = Fixture::new(Behavior::EarlyResponse, 1).await;
        fixture.calls.bad_start.store(mode, Ordering::Relaxed);
        let (_tx, rx) = watch::channel(false);
        assert!(matches!(
            registered(&fixture, rx).await,
            Err(PortalCallError::Response)
        ));
        fixture.calls.wait("session_close").await;
        assert_eq!(fixture.calls.count("open_remote"), 0);
    }
}
#[tokio::test]
async fn worker_owned_frame_budget_and_close_release() {
    let (fixture, capture) = prepared_control_fixture().await;
    let frame = capture
        .observe_owned_frame(Duration::from_secs(1), 8 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!([frame.width, frame.height], [1280, 720]);
    assert!(
        capture
            .check_input_frame(frame.stamp.format_revision, [frame.width, frame.height])
            .is_ok()
    );
    capture
        .inject_input(protocol::InputEvent::KeyDown(0))
        .await
        .unwrap();
    capture
        .inject_input(protocol::InputEvent::MouseDown(0))
        .await
        .unwrap();
    assert!(capture.close_owned().await.is_ok());
    assert!(fixture.held_input_empty());
    assert!(
        capture
            .observe_owned_frame(Duration::from_millis(1), 10)
            .await
            .is_err()
    );
    assert!(capture.close_owned().await.is_err());
}
#[tokio::test]
async fn worker_revision_invalidates_input_without_another_observe() {
    let (_fixture, capture) = prepared_control_fixture().await;
    let frame = capture
        .observe_owned_frame(Duration::from_secs(1), 8 * 1024 * 1024)
        .await
        .unwrap();
    assert!(
        capture
            .inner
            .mailbox
            .set_format_revision(frame.stamp.format_revision + 1)
    );
    assert!(matches!(
        capture.check_input_frame(frame.stamp.format_revision, [frame.width, frame.height]),
        Err(PortalInputError::FrameBinding)
    ));
    assert!(capture.close_owned().await.is_ok());
}
#[tokio::test]
async fn worker_frame_budget_is_enforced() {
    let (_fixture, capture) = prepared_control_fixture().await;
    assert!(matches!(
        capture.observe_owned_frame(Duration::from_secs(1), 1).await,
        Err(PortalCallError::InvalidSource)
    ));
    assert!(capture.close_owned().await.is_ok());
}

#[tokio::test]
async fn worker_owned_close_failure_and_timeout_are_unconfirmed() {
    for mode in [1, 2] {
        let (fixture, capture) = prepared_control_fixture().await;
        capture
            .inject_input(protocol::InputEvent::KeyDown(0))
            .await
            .unwrap();
        capture
            .inject_input(protocol::InputEvent::MouseDown(0))
            .await
            .unwrap();
        fixture.calls.close_mode.store(mode, Ordering::Relaxed);
        let started = std::time::Instant::now();
        assert!(capture.close_owned().await.is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        fixture.calls.wait("session_close").await;
        assert!(fixture.held_input_empty());
        assert!(
            capture
                .inject_input(protocol::InputEvent::KeyDown(0))
                .await
                .is_err()
        );
        assert!(
            capture
                .observe_owned_frame(Duration::from_millis(1), 1024)
                .await
                .is_err()
        );
        assert!(capture.close_owned().await.is_err());
    }
}
