use super::*;
use serde_json::json;
fn fixture() -> (Value, Value, Actor, Call) {
    let actor = Actor {
        owner: "synthetic-owner".into(),
        task_id: "11111111-1111-1111-1111-111111111111".into(),
        executor_device_id: "22222222-2222-2222-2222-222222222222".into(),
    };
    let target = Target {
        peer_device_id: "synthetic-remote-peer".into(),
        source: protocol::session::CaptureSource::Window(42),
        connection_id: 7,
    };
    let route = json!({"profile":"/synthetic/gui","generation":"1-2","gui_executable_sha256":"c".repeat(64),"target":target});
    let root = json!({"enabled":true,"observe":true,"input":false,"capability_version":7,
        "identity":{"owner":actor.owner,"device_id":actor.executor_device_id,"desktop_id":target.desktop_id()},
        "desktop_session":"weavelo","desktop_uid":unsafe {libc::geteuid()},"max_frame_bytes":1024,
        "weavelo":route,"worker_sha256":"a".repeat(64),"agent_sha256":"b".repeat(64)});
    let worker = json!({"enabled":true,"allow_input":false,"app_id":"fun.hackerlife.octendra.desktop-worker",
        "identity":{"owner":actor.owner,"task_id":actor.task_id,"device_id":actor.executor_device_id,"desktop_id":target.desktop_id()},
        "desktop_session":"weavelo","desktop_uid":unsafe {libc::geteuid()},"capability_version":7,
        "max_frame_bytes":1024,"expires_at_ms":crate::unix_now_ms()+30000,"weavelo":route,
        "worker_sha256":"a".repeat(64),"agent_sha256":"b".repeat(64)});
    let call = Call::Open {
        actor: actor.clone(),
        target,
        lease_id: "33333333-3333-3333-3333-333333333333".into(),
        lease_ms: 1000,
        max_bytes: 1024,
    };
    (root, worker, actor, call)
}
fn check(root: &Value, worker: &Value, actor: &Actor, call: &Call) -> Result<(), Error> {
    authorize(
        root,
        worker,
        actor,
        call,
        Path::new("/synthetic/gui"),
        "1-2",
        &"c".repeat(64),
    )
}
#[test]
fn existing_authority_requires_exact_owner_task_executor_remote_source_and_version() {
    let (root, worker, actor, call) = fixture();
    assert!(check(&root, &worker, &actor, &call).is_ok());
    for field in ["owner", "task_id", "device_id", "desktop_id"] {
        let mut other = worker.clone();
        other["identity"][field] = json!("wrong");
        assert!(check(&root, &other, &actor, &call).is_err(), "{field}");
    }
    for field in ["capability_version", "agent_sha256", "worker_sha256"] {
        let mut other = worker.clone();
        other[field] = json!(0);
        assert!(check(&root, &other, &actor, &call).is_err(), "{field}");
    }
    let mut other = root.clone();
    other["weavelo"]["target"]["source"] = json!({"Window":43});
    assert!(check(&other, &worker, &actor, &call).is_err());
    let mut other = root.clone();
    other["identity"]["device_id"] = json!("synthetic-remote-peer");
    assert!(check(&other, &worker, &actor, &call).is_err());
}
#[test]
fn old_local_desktop_grant_and_input_enabled_grants_do_not_authorize_remote_observation() {
    let (root, worker, actor, call) = fixture();
    let mut old = root.clone();
    old.as_object_mut().unwrap().remove("weavelo");
    assert!(check(&old, &worker, &actor, &call).is_err());
    let mut enabled = root.clone();
    enabled["input"] = json!(true);
    assert!(check(&enabled, &worker, &actor, &call).is_err());
    let mut expired = worker.clone();
    expired["expires_at_ms"] = json!(crate::unix_now_ms() - 1);
    assert!(check(&root, &expired, &actor, &call).is_err());
    let mut over = worker.clone();
    over["max_frame_bytes"] = json!(1023);
    assert!(check(&root, &over, &actor, &call).is_err());
}
#[test]
fn absent_locator_is_unavailable_without_trusting_request_paths_or_process_names() {
    let dir = tempfile::tempdir().unwrap();
    let authority = Authority::new(dir.path(), "1-2", &"c".repeat(64));
    assert!(!authority.configured());
    assert!(authority.verify(&fixture().3, None, None).is_err());
}
