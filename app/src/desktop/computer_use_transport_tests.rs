//! Only transport framing, with explicitly synthetic bytes; no media or grant.
use super::*;
use std::os::unix::fs::PermissionsExt;
#[tokio::test]
async fn existing_private_socket_carries_bounded_rgba_after_metadata_and_reports_real_peer_pid() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let mut adapter = Adapter::start(dir.path()).unwrap();
    assert!(!adapter.computer_use.configured());
    let request = Request::ComputerUse {
        generation: adapter.generation.clone(),
        request_id: "44444444-4444-4444-4444-444444444444".into(),
        deadline_ms: crate::unix_now_ms() + 4500,
        call: super::super::computer_use_ipc::Call::Observe {
            actor: super::super::computer_use_ipc::Actor {
                owner: "fixture".into(),
                task_id: "11111111-1111-1111-1111-111111111111".into(),
                executor_device_id: "22222222-2222-2222-2222-222222222222".into(),
            },
            lease_id: "33333333-3333-3333-3333-333333333333".into(),
        },
    };
    let path = dir.path().to_owned();
    let client = tokio::task::spawn_blocking(move || {
        let mut s = crate::private_ipc::connect(&path, SOCKET, Duration::from_secs(2)).unwrap();
        let bytes = serde_json::to_vec(&request).unwrap();
        s.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap();
        s.write_all(&bytes).unwrap();
        let mut head = [0; 4];
        s.read_exact(&mut head).unwrap();
        let mut body = vec![0; u32::from_be_bytes(head) as usize];
        s.read_exact(&mut body).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let mut raw = vec![0; body["bytes"].as_u64().unwrap() as usize];
        s.read_exact(&mut raw).unwrap();
        (body, raw)
    });
    let command = tokio::time::timeout(Duration::from_secs(2), adapter.commands.recv())
        .await
        .unwrap()
        .unwrap();
    adapter
        .admit(&command.request, crate::unix_now_ms())
        .unwrap();
    #[cfg(target_os = "macos")]
    assert_eq!(command.peer_pid, Some(std::process::id() as i32));
    command
        .reply
        .send(Reply::observation(super::super::computer_use_ipc::Output {
            body: serde_json::json!({"ok":true,"bytes":16,"encoding":"rgba8_srgb","size":[2,2]}),
            pixels: Some(std::sync::Arc::new(vec![17; 16])),
        }))
        .unwrap_or_else(|_| panic!("synthetic reader closed"));
    let (body, raw) = client.await.unwrap();
    assert_eq!(body["encoding"], "rgba8_srgb");
    assert_eq!(raw, vec![17; 16]);
}
