//! Opt-in real worker/PID/authority integration. Pixel and host-input endpoints
//! are explicit fixtures; this does not capture a screen or inject OS input.
use super::{
    computer_use_authority::{Authority, Caller},
    computer_use_ipc::{Call, Output},
    directed_receive::{Adapter, Reply, Request},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn digest(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}
fn private_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
async fn exchange(
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::process::ChildStdout,
    operation: &str,
    request: Value,
) -> (Value, Vec<u8>) {
    tokio::time::timeout(Duration::from_secs(12), async {
        let raw = serde_json::to_vec(&json!({"request_id":operation,"request":request})).unwrap();
        input.write_u32(raw.len() as u32).await.unwrap();
        input.write_all(&raw).await.unwrap();
        input.flush().await.unwrap();
        let len = output
            .read_u32()
            .await
            .expect("worker exited before a reply") as usize;
        assert!(len > 0 && len <= 65536);
        let mut raw = vec![0; len];
        output.read_exact(&mut raw).await.unwrap();
        let reply: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(reply["request_id"], operation);
        let length = reply["pixels"]["plane_lengths"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_u64().unwrap() as usize).sum())
            .unwrap_or(0);
        assert!(length <= 16);
        let mut pixels = vec![0; length];
        output.read_exact(&mut pixels).await.unwrap();
        (reply, pixels)
    })
    .await
    .expect("bounded actual worker exchange")
}

async fn exercise(revoke: bool) {
    let worker = PathBuf::from(
        std::env::var_os("WEAVELO_TEST_OCTENDRA_WORKER")
            .expect("run the opt-in computer-use-process mode with a freshly built worker"),
    );
    assert!(worker.is_absolute() && !worker.is_symlink());
    let worker_hash = digest(&worker);
    assert_eq!(
        worker_hash,
        std::env::var("WEAVELO_TEST_OCTENDRA_WORKER_SHA256").unwrap()
    );
    let parent = std::env::current_exe().unwrap();
    let parent_hash = digest(&parent);
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("gui");
    let records = temp.path().join("authority");
    for p in [&profile, &records] {
        fs::create_dir(p).unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let launch_path = records.join("desktop-launch.json");
    private_json(
        &profile.join("octendra-authority.json"),
        &json!({"desktop_launch":launch_path}),
    );
    let mut adapter = Adapter::start(&profile).unwrap();
    assert!(adapter.computer_use.configured());
    let generation = adapter.generation.clone();
    let gui_hash = adapter.executable_sha256.clone();
    let authority = Authority::new(&profile, &generation, &gui_hash);
    let epoch = "33333333-3333-4333-8333-333333333333";
    let identity = json!({"owner":"synthetic-process-test",
        "task_id":"11111111-1111-4111-8111-111111111111",
        "device_id":"22222222-2222-4222-8222-222222222222",
        "desktop_id":"weavelo:7:window:42:synthetic-peer"});
    let target =
        json!({"peer_device_id":"synthetic-peer","source":{"Window":42},"connection_id":7});
    let route = json!({"profile":profile,"generation":generation,"gui_executable_sha256":gui_hash,
        "target":target,"input_platform":"macos"});
    let mut root = json!({"enabled":true,"observe":true,"input":true,"capability_version":7,
        "identity":{"owner":identity["owner"],"device_id":identity["device_id"],"desktop_id":identity["desktop_id"]},
        "desktop_uid":unsafe {libc::geteuid()},"desktop_session":"weavelo",
        "logical_size":[2,2],"logical_position":[0,0],"max_frame_bytes":16,
        "worker_path":worker,"worker_sha256":worker_hash,"agent_sha256":parent_hash,"weavelo":route});
    private_json(&launch_path, &root);
    let approval_path = records.join(format!("desktop-worker-{epoch}.json"));
    private_json(
        &approval_path,
        &json!({"enabled":true,"allow_input":true,"capability_version":7,
        "app_id":"fun.hackerlife.octendra.desktop-worker","desktop_uid":unsafe {libc::geteuid()},
        "desktop_session":"weavelo","identity":identity,"logical_size":[2,2],"logical_position":[0,0],
        "max_frame_bytes":16,"expires_at_ms":crate::unix_now_ms()+30000,
        "worker_sha256":worker_hash,"agent_sha256":parent_hash,"weavelo":route}),
    );
    let mut child = tokio::process::Command::new(&worker)
        .arg("--weavelo-approval")
        .arg(&approval_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let worker_pid = child.id().unwrap() as i32;
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let admitted = Arc::new(AtomicUsize::new(0));
    let actions = Arc::new(AtomicUsize::new(0));
    let checked = admitted.clone();
    let recorded = actions.clone();
    let server = tokio::spawn(async move {
        let mut caller: Option<Caller> = None;
        while let Some(command) = adapter.commands.recv().await {
            adapter
                .admit(&command.request, crate::unix_now_ms())
                .unwrap();
            let (body, pixels) = match command.request {
                Request::Status => (
                    json!({"ok":true,"generation":generation,
                    "executable_sha256":gui_hash,"computer_use":{"protocol":1,"observe":true,"input":true}}),
                    None,
                ),
                Request::ComputerUse { call, .. } => {
                    assert_eq!(
                        command.peer_pid,
                        Some(worker_pid),
                        "kernel peer PID must identify the real child"
                    );
                    let began = std::time::Instant::now();
                    let verified = authority.verify(&call, command.peer_pid, caller.as_ref());
                    eprintln!(
                        "FIXTURE_AUTHORITY_CHECK elapsed_ms={} accepted={} code={}",
                        began.elapsed().as_millis(),
                        verified.is_ok(),
                        verified.as_ref().err().copied().unwrap_or("ok")
                    );
                    match verified {
                        Err(code) => (json!({"ok":false,"code":code}), None),
                        Ok(verified) => {
                            checked.fetch_add(1, Ordering::SeqCst);
                            caller = Some(verified);
                            match call {
                                Call::Open {
                                    target,
                                    lease_id,
                                    lease_ms,
                                    control,
                                    ..
                                } => {
                                    assert!(control);
                                    assert_eq!(
                                        target.desktop_id(),
                                        "weavelo:7:window:42:synthetic-peer"
                                    );
                                    (
                                        json!({"ok":true,"lease_id":lease_id,"lease_ms":lease_ms}),
                                        None,
                                    )
                                }
                                Call::Observe { lease_id, .. } => (
                                    json!({"ok":true,"lease_id":lease_id,"source_revision":7,"frame_sequence":1,
                                    "size":[2,2],"bytes":16,"encoding":"rgba8_srgb"}),
                                    Some(Arc::new(vec![17; 16])),
                                ),
                                Call::Act {
                                    source_revision,
                                    frame_sequence,
                                    action,
                                    ..
                                } => {
                                    assert_eq!((source_revision, frame_sequence), (7, 1));
                                    assert_eq!(
                                        serde_json::to_value(action).unwrap(),
                                        json!({"kind":"key_press","key_code":0})
                                    );
                                    recorded.fetch_add(1, Ordering::SeqCst);
                                    (json!({"ok":true,"effect_confirmed":false}), None)
                                }
                                Call::Close { .. } => (json!({"ok":true}), None),
                                _ => panic!("unexpected fixture operation"),
                            }
                        }
                    }
                }
                _ => panic!("no non-computer-use operation is allowed"),
            };
            let _ = command
                .reply
                .send(Reply::observation(Output { body, pixels }));
        }
    });
    let (opened, _) = exchange(
        &mut input,
        &mut output,
        epoch,
        json!({"operation":"open","identity":identity,"lease_ms":10000}),
    )
    .await;
    assert_eq!(opened["result"]["Ok"]["state"], "opened");
    let binding = opened["result"]["Ok"]["binding"].clone();
    let (observed, pixels) = exchange(
        &mut input,
        &mut output,
        "44444444-4444-4444-8444-444444444444",
        json!({"operation":"observe","binding":binding}),
    )
    .await;
    assert_eq!(pixels, vec![17; 16]);
    let frame = observed["result"]["Ok"]["frame"].clone();
    assert_eq!(
        frame["binding"]["identity"]["device_id"],
        identity["device_id"]
    );
    assert_eq!(frame["source_revision"], 7);
    if revoke {
        root["input"] = json!(false);
        private_json(&launch_path, &root);
    }
    let act = json!({"operation":"act","frame":frame,"sequence":1,
        "actions":[{"kind":"key_down","usage":4},{"kind":"key_up","usage":4}]});
    let (accepted, _) = exchange(
        &mut input,
        &mut output,
        "55555555-5555-4555-8555-555555555555",
        act.clone(),
    )
    .await;
    if revoke {
        assert_eq!(accepted["result"]["Err"], "outcome_unknown");
        assert_eq!(actions.load(Ordering::SeqCst), 0);
    } else {
        assert_eq!(accepted["result"]["Ok"]["state"], "accepted");
        assert_eq!(accepted["result"]["Ok"]["accepted_actions"], 2);
        assert_eq!(accepted["result"]["Ok"]["effect_confirmed"], false);
        let (replayed, _) = exchange(
            &mut input,
            &mut output,
            "66666666-6666-4666-8666-666666666666",
            act,
        )
        .await;
        assert_eq!(replayed["result"]["Err"], "outcome_unknown");
        assert_eq!(actions.load(Ordering::SeqCst), 1);
    }
    let _ = exchange(
        &mut input,
        &mut output,
        "77777777-7777-4777-8777-777777777777",
        json!({"operation":"close","binding":binding}),
    )
    .await;
    drop(input);
    let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    server.abort();
    let _ = server.await;
    assert!(admitted.load(Ordering::SeqCst) >= 2);
    println!(
        "ACTUAL_WORKER_IPC_AUTHORITY verified_calls={} fixture_actions={} revoked={} os_input=false real_screen=false",
        admitted.load(Ordering::SeqCst),
        actions.load(Ordering::SeqCst),
        revoke
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the separately built, hash-pinned Octendra worker; synthetic host endpoints only"]
async fn computer_use_process_actual_child_uses_kernel_pid_parent_pins_and_single_frame_gesture() {
    exercise(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the separately built, hash-pinned Octendra worker; synthetic host endpoints only"]
async fn computer_use_process_actual_child_rejects_revocation_before_fixture_input() {
    exercise(true).await;
}
