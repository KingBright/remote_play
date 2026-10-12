//! Real private D-Bus requests; no actual desktop or stored system permission.
use super::*;

async fn remembered(f: &Fixture, control: bool) -> (watch::Sender<bool>, PortalCaptureLease) {
    f.calls.issue_restores.store(true, Ordering::Relaxed);
    let (keep, rx) = watch::channel(false);
    let lease = select_with_restoration(f.client.clone(), "x11:2a", PickerSources::Monitor,
        rx, f.timeouts(), control, (true, None)).await.unwrap();
    (keep, lease)
}
#[tokio::test]
async fn screencast_uses_application_lifetime_and_rotates_single_use_token() {
    let f = Fixture::new(Behavior::EarlyResponse, 1).await;
    let (_keep, mut first) = remembered(&f, false).await;
    let saved = first.take_restoration().unwrap();
    assert!(first.take_restoration().is_none());
    assert!(!format!("{saved:?}").contains("private-bus-unit-token"));
    let (_keep2, rx) = watch::channel(false);
    let mut second = select_with_restoration(f.client.clone(), "x11:2a", PickerSources::Monitor,
        rx, f.timeouts(), false, (true, Some(saved))).await.unwrap();
    let renewed = second.take_restoration().unwrap();
    assert_eq!(renewed.token, "private-bus-unit-token-2");
    assert_eq!(*f.calls.restore_requests.lock().unwrap(), vec![
        (false, 1, None), (false, 1, Some("private-bus-unit-token-1".into()))]);
    assert!(!second.input_available());
}
#[tokio::test]
async fn combined_control_restoration_only_uses_remote_desktop_options() {
    let f = Fixture::new(Behavior::EarlyResponse, 1).await;
    let (_keep, mut first) = remembered(&f, true).await;
    let saved = first.take_restoration().unwrap();
    let (_keep2, rx) = watch::channel(false);
    let mut second = select_with_restoration(f.client.clone(), "x11:2a", PickerSources::Monitor,
        rx, f.timeouts(), true, (true, Some(saved))).await.unwrap();
    assert!(second.take_restoration().is_some());
    let events = f.calls.restore_requests.lock().unwrap();
    assert_eq!(events.iter().filter(|e| !e.0).cloned().collect::<Vec<_>>(), vec![(false, 0, None);2]);
    assert_eq!(events.iter().filter(|e| e.0).cloned().collect::<Vec<_>>(), vec![
        (true, 1, None), (true, 1, Some("private-bus-unit-token-1".into()))]);
    assert!(!second.input_available(), "a restored Start is not an observed frame");
}
#[tokio::test]
async fn readonly_token_cannot_upgrade_to_input_or_another_source_kind() {
    for (sources, control) in [(PickerSources::Monitor, true), (PickerSources::Window, false)] {
        let f = Fixture::new(Behavior::EarlyResponse, 1).await;
        let (_keep, mut first) = remembered(&f, false).await;
        let saved = first.take_restoration().unwrap();
        let before = f.calls.count("create");
        let (_keep2, rx) = watch::channel(false);
        let result = select_with_restoration(f.client.clone(), "x11:2a", sources,
            rx, f.timeouts(), control, (true, Some(saved))).await;
        assert!(matches!(result, Err(PortalCallError::Capabilities)));
        assert_eq!(f.calls.count("create"), before);
    }
}
#[tokio::test]
async fn revoked_token_does_not_open_another_session() {
    let f = Fixture::new(Behavior::EarlyResponse, 1).await;
    let (keep, mut first) = remembered(&f, false).await;
    let saved = first.take_restoration().unwrap();
    keep.send_replace(true);
    assert!(!saved.is_usable(), "cancellation must not wait for owner-task scheduling");
    tokio::time::timeout(Duration::from_secs(1), async {
        while saved.is_usable() { tokio::task::yield_now().await; }
    }).await.unwrap();
    let (_keep2, rx) = watch::channel(false);
    let result = select_with_restoration(f.client.clone(), "x11:2a", PickerSources::Monitor,
        rx, f.timeouts(), false, (true, Some(saved))).await;
    assert!(matches!(result, Err(PortalCallError::Revoked)));
    assert_eq!(f.calls.count("create"), 1);
}
#[tokio::test]
async fn version_one_remote_desktop_does_not_request_unsupported_persistence() {
    let f = Fixture::new(Behavior::EarlyResponse, 1).await;
    f.calls.remote_version.store(1, Ordering::Relaxed);
    let (_keep, mut first) = remembered(&f, true).await;
    assert!(first.take_restoration().is_none());
    assert!(f.calls.restore_requests.lock().unwrap().iter().all(|e| e.1==0 && e.2.is_none()));
}
#[tokio::test]
async fn partial_keyboard_pointer_grant_is_not_cached_for_control_restoration() {
    for devices in [0,1,2] {
        let f = Fixture::with_devices(Behavior::EarlyResponse, 1, devices).await;
        let (_keep, mut first) = remembered(&f, true).await;
        assert!(first.take_restoration().is_none());
        assert!(!first.input_available());
    }
}
#[tokio::test]
async fn cancellation_consumes_restore_without_replaying_portal_request() {
    let f = Fixture::new(Behavior::EarlyResponse, 1).await;
    let (_keep, mut first) = remembered(&f, false).await;
    let saved = first.take_restoration().unwrap();
    let (_keep2, rx) = watch::channel(true);
    let result = select_with_restoration(f.client.clone(), "x11:2a", PickerSources::Monitor,
        rx, f.timeouts(), false, (true, Some(saved))).await;
    assert!(matches!(result, Err(PortalCallError::Cancelled)));
    assert_eq!(f.calls.count("create"), 1);
}
