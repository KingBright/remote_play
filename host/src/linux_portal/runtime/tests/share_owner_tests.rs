//! Production application owner, real private D-Bus adapter, mocked native worker.
use super::*;
use crate::service::{LocalPortalShareCommand, LocalPortalShareTarget};
use tokio::sync::mpsc;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum ShareKind { Window, Display, ControlledDisplay }
#[derive(Clone, Debug, PartialEq, Eq)]
struct TargetRow { key: String, label: String }
#[allow(dead_code)]
#[path="../../../../../app/src/desktop/local_share/linux.rs"]
mod application_owner;

struct Actor {
    target: Arc<Mutex<LocalPortalShareTarget>>,
    captures: Arc<Mutex<Vec<PreparedPortalCapture>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Actor { fn drop(&mut self) { self.task.abort(); } }
fn actor() -> (application_owner::Owner, Actor) {
    let target = Arc::new(Mutex::new(LocalPortalShareTarget::fixture_target([1;16])));
    let captures = Arc::new(Mutex::new(Vec::new()));
    let (tx, mut rx) = mpsc::channel(8);
    let current = target.clone();let held = captures.clone();
    let task = tokio::spawn(async move {
        while let Some(command) = rx.recv().await {
            match command {
                LocalPortalShareCommand::Targets { reply } => { let _=reply.send(vec![current.lock().unwrap().clone()]); }
                LocalPortalShareCommand::Commit { target, capture, reply } => {
                    if !target.same_authenticated_target(&current.lock().unwrap()) {
                        capture.revoke();let _=reply.send(Err("viewer changed".into()));continue;
                    }
                    let info=capture.source_info();held.lock().unwrap().push(capture);let _=reply.send(Ok(info));
                }
            }
        }
    });
    (application_owner::Owner::new(Some(tx)), Actor { target, captures, task })
}
// The production owner opens the default session bus. Scope that address only
// inside this serial test process, never a user's running RP process.
#[tokio::test]
#[ignore = "isolated default-session-bus fixture; run alone with --ignored --test-threads=1"]
async fn production_share_owner_restores_stops_and_rejects_changed_authentication() {
    let f = Fixture::new(Behavior::EarlyResponse, 1).await;
    f.calls.issue_restores.store(true, Ordering::Relaxed);
    let old = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
    struct RestoreEnvironment(Option<std::ffi::OsString>);
    impl Drop for RestoreEnvironment { fn drop(&mut self) {
        unsafe { if let Some(v)=&self.0 {std::env::set_var("DBUS_SESSION_BUS_ADDRESS",v)}
                 else {std::env::remove_var("DBUS_SESSION_BUS_ADDRESS")} }
    }}
    let _restore = RestoreEnvironment(old);
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &f._bus.address); }
    let (owner, actor) = actor();
    let rows=owner.refresh(1).await.unwrap();let key=rows[0].key.clone();
    owner.select(2,key.clone(),ShareKind::Display).await.unwrap();
    owner.select(3,key.clone(),ShareKind::Display).await.unwrap();
    assert_eq!(*f.calls.restore_requests.lock().unwrap(),vec![
        (false,1,None),(false,1,Some("private-bus-unit-token-1".into()))]);
    assert_eq!(actor.captures.lock().unwrap().len(),2);
    owner.select_source(8,key.clone(),ShareKind::Display,false).await.unwrap();
    assert_eq!(f.calls.restore_requests.lock().unwrap().last().unwrap(),&(false,1,None));
    assert!(actor.captures.lock().unwrap().iter().all(|capture| !capture.is_revoked()), "fresh picker must keep the previous grant until commit");
    assert!(!owner.active_keys().is_empty());
    owner.stop(&key);
    tokio::time::timeout(Duration::from_secs(1),async {
        while !actor.captures.lock().unwrap().iter().all(PreparedPortalCapture::is_revoked) {
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    owner.select(4,key.clone(),ShareKind::Display).await.unwrap();
    assert_eq!(f.calls.restore_requests.lock().unwrap().last().unwrap(),&(false,1,None));
    // Same address, connection ID and row key, different verified nonce.
    *actor.target.lock().unwrap()=LocalPortalShareTarget::fixture_target([3;16]);
    let before=f.calls.count("create");
    assert!(owner.select(5,key.clone(),ShareKind::Display).await.unwrap_err().contains("Authenticated viewer changed"));
    assert_eq!(f.calls.count("create"),before,"stale row must fail before opening Portal");
    owner.refresh(6).await.unwrap();
    owner.select(7,key.clone(),ShareKind::Display).await.unwrap();
    assert_eq!(f.calls.restore_requests.lock().unwrap().last().unwrap(),&(false,1,None),"fresh identity cannot receive another peer's saved token");
    owner.cancel(7);
    assert!(owner.active_keys().is_empty());
}
