//! Same-user GUI command adapter for directed delivery through existing RH.
//! No remote listener, credentials, shell execution or new media protocol.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};
type Error = Box<dyn std::error::Error + Send + Sync>;
const SOCKET: &str = "gui-receive.sock";
const LIMIT: usize = 4096;
const MAX_LEASE_MS: u64 = 120_000;
const ADMISSION_MS: u64 = 5_000;
#[derive(Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) enum Request {
    Status,
    Receive {
        generation: String,
        request_id: String,
        source_device_id: String,
        deadline_ms: u64,
        lease_ms: u64,
    },
    Stop {
        generation: String,
        request_id: String,
        deadline_ms: u64,
    },
}
pub(crate) struct Command {
    pub request: Request,
    pub reply: oneshot::Sender<Reply>,
}
pub(crate) struct Reply {
    body: serde_json::Value,
    revoke_on_drop: Option<(
        std::sync::Weak<super::original_owner::OriginalOwner>,
        String,
    )>,
}
impl From<serde_json::Value> for Reply {
    fn from(body: serde_json::Value) -> Self {
        Self {
            body,
            revoke_on_drop: None,
        }
    }
}
impl Reply {
    pub fn accepted(
        body: serde_json::Value,
        owner: &std::sync::Arc<super::original_owner::OriginalOwner>,
    ) -> Self {
        let request = body["request_id"]
            .as_str()
            .expect("accepted reply request ID")
            .to_owned();
        Self {
            body,
            revoke_on_drop: Some((std::sync::Arc::downgrade(owner), request)),
        }
    }
    fn written(&mut self) {
        self.revoke_on_drop = None;
    }
}
impl Drop for Reply {
    fn drop(&mut self) {
        if let Some((owner, request)) = self.revoke_on_drop.take() {
            if let Some(owner) = owner.upgrade() {
                owner.cancel_directed_receive(&request);
            }
        }
    }
}

pub(crate) struct Adapter {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
    pub generation: String,
    pub commands: mpsc::Receiver<Command>,
    pub lease: Option<Lease>,
    seen: HashSet<String>,
}
pub(crate) struct Lease {
    pub request_id: String,
    pub source: String,
    pub expires: Instant,
    pub owner: std::sync::Weak<super::original_owner::OriginalOwner>,
    pub watchdog: tokio::task::JoinHandle<()>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.cancel_directed_receive(&self.request_id);
        }
        self.watchdog.abort();
    }
}
impl Adapter {
    /// Instance's profile lock must already be held by this GUI.
    pub fn start(directory: &Path) -> Result<Self, Error> {
        crate::background_service::private_directory(directory)?;
        let path = directory.join(SOCKET);
        match path.symlink_metadata() {
            Ok(_) => {
                crate::background_service::private_socket(&path)?;
                match std::os::unix::net::UnixStream::connect(&path) {
                    Ok(_) => return Err("Another GUI receive adapter owns this endpoint".into()),
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                        fs::remove_file(&path)?
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = tokio::net::UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let generation = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        );
        let (tx, commands) = mpsc::channel(8);
        let task = tokio::spawn(async move {
            // Serial, bounded requests. A stalled same-user client holds this
            // adapter for at most two seconds, never the GUI or media runtime.
            while let Ok((mut socket, _)) = listener.accept().await {
                if socket.peer_cred().map(|c| c.uid()).ok() != Some(unsafe { libc::geteuid() }) {
                    continue;
                }
                let _ = tokio::time::timeout(Duration::from_secs(2), async {
                    let mut header = [0; 4];
                    socket.read_exact(&mut header).await?;
                    let size = length(header)?;
                    let mut bytes = vec![0; size];
                    socket.read_exact(&mut bytes).await?;
                    let request: Request = serde_json::from_slice(&bytes)?;
                    let (reply, response) = oneshot::channel();
                    tx.try_send(Command { request, reply })
                        .map_err(|_| "GUI command queue is busy")?;
                    // A reply owns its exact cancellation receipt even while
                    // queued inside oneshot. Timeout after send but before recv
                    // therefore drops the receipt and revokes the lease.
                    let mut response = response.await?;
                    let bytes = serde_json::to_vec(&response.body)?;
                    socket
                        .write_all(&(bytes.len() as u32).to_be_bytes())
                        .await?;
                    socket.write_all(&bytes).await?;
                    response.written(); // Kernel write only; no presentation claim.
                    Ok::<(), Error>(())
                })
                .await;
            }
        });
        Ok(Self {
            path,
            task,
            generation,
            commands,
            lease: None,
            seen: HashSet::new(),
        })
    }
    pub fn admit(&mut self, request: &Request, now_ms: u64) -> Result<(), &'static str> {
        match request {
            Request::Status => Ok(()),
            Request::Receive {
                generation,
                request_id,
                source_device_id,
                deadline_ms,
                lease_ms,
            } => {
                valid_request(
                    &self.generation,
                    generation,
                    request_id,
                    *deadline_ms,
                    now_ms,
                )?;
                if source_device_id.is_empty()
                    || source_device_id.len() > 256
                    || source_device_id.chars().any(char::is_control)
                {
                    return Err("Invalid source device ID");
                }
                if !(1_000..=MAX_LEASE_MS).contains(lease_ms) {
                    return Err("Receive lease must be between 1 and 120 seconds");
                }
                if self.seen.len() >= 256 || !self.seen.insert(request_id.clone()) {
                    return Err("Request replay or generation request limit reached");
                }
                Ok(())
            }
            Request::Stop {
                generation,
                request_id,
                deadline_ms,
            } => valid_request(
                &self.generation,
                generation,
                request_id,
                *deadline_ms,
                now_ms,
            ),
        }
    }
}
fn valid_request(
    expected: &str,
    generation: &str,
    id: &str,
    deadline: u64,
    now: u64,
) -> Result<(), &'static str> {
    if expected != generation {
        return Err("GUI generation changed");
    }
    if id.is_empty()
        || id.len() > 128
        || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err("Invalid request ID");
    }
    if deadline <= now || deadline.saturating_sub(now) > ADMISSION_MS {
        return Err("Request deadline expired or exceeds five seconds");
    }
    Ok(())
}
fn length(header: [u8; 4]) -> Result<usize, Error> {
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > LIMIT {
        return Err("Invalid GUI adapter frame length".into());
    }
    Ok(size)
}
impl Drop for Adapter {
    fn drop(&mut self) {
        self.lease.take();
        self.task.abort();
        let _ = fs::remove_file(&self.path);
    }
}
fn exchange(directory: &Path, request: &Request) -> Result<serde_json::Value, Error> {
    crate::background_service::private_directory(directory)?;
    let path = directory.join(SOCKET);
    crate::background_service::private_socket(&path)?;
    let mut socket = std::os::unix::net::UnixStream::connect(&path)?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    crate::background_service::private_socket(&path)?;
    let bytes = serde_json::to_vec(request)?;
    length((bytes.len() as u32).to_be_bytes())?;
    socket.write_all(&(bytes.len() as u32).to_be_bytes())?;
    socket.write_all(&bytes)?;
    let mut header = [0; 4];
    socket.read_exact(&mut header)?;
    // Status diagnostics can be larger than a command; still strictly bounded.
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > 256 * 1024 {
        return Err("Invalid GUI response length".into());
    }
    let mut bytes = vec![0; size];
    socket.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub(crate) fn command(profile: &Path, args: &[String]) -> Result<serde_json::Value, Error> {
    crate::background_service::private_directory(profile)?;
    let frontend = profile.join("gui-state");
    let attached = frontend.join(SOCKET).symlink_metadata().is_ok();
    let standalone = profile.join(SOCKET).symlink_metadata().is_ok();
    if attached == standalone {
        return Err(
            "Expected exactly one running GUI receive adapter; open the installed GUI first".into(),
        );
    }
    let directory = if attached {
        frontend.as_path()
    } else {
        profile
    };
    let args = if args.len() == 5 && args[0] == "--gui-receive" && args[3] == "--receiver" {
        let identity = remote_core::mesh::AppPrivateMeshConfigStore::new(profile)
            .load()?
            .ok_or("Receiver identity is unavailable")?;
        if identity.node_id != args[4] {
            return Err("This command targets a different receiver device".into());
        }
        &args[..3]
    } else {
        args
    };
    let request = match args {
        [cmd] if cmd == "--gui-status-json" => Request::Status,
        [cmd, source] | [cmd, source, _] if cmd == "--gui-receive" => {
            let seconds: u64 = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(60);
            if !(1..=120).contains(&seconds) { return Err("Receive duration must be 1..120 seconds".into()); }
            let status = exchange(directory, &Request::Status)?;
            let generation = status["generation"].as_str().ok_or("Missing GUI generation")?.to_string();
            let now = crate::unix_now_ms();
            Request::Receive { generation, request_id: format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()),
                source_device_id: source.clone(), deadline_ms: now.saturating_add(ADMISSION_MS), lease_ms: seconds*1000 }
        }
        [cmd, generation, id] if cmd == "--gui-stop-receive" => Request::Stop { generation: generation.clone(), request_id: id.clone(), deadline_ms: crate::unix_now_ms().saturating_add(ADMISSION_MS) },
        _ => return Err("Usage: --gui-status-json | --gui-receive SOURCE_DEVICE_ID [1..120 seconds] [--receiver RECEIVER_DEVICE_ID] | --gui-stop-receive GENERATION REQUEST_ID".into()),
    };
    let result = exchange(directory, &request)?;
    if result["ok"] != true {
        return Err(result["error"]
            .as_str()
            .unwrap_or("GUI rejected request")
            .to_owned()
            .into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_stale_generation_deadline_and_unbounded_ids() {
        assert!(valid_request("a", "b", "request-1", 101, 100).is_err());
        assert!(valid_request("a", "a", "request-1", 100, 100).is_err());
        assert!(valid_request("a", "a", "request-1", 5101, 100).is_err());
        assert!(valid_request("a", "a", "../request", 101, 100).is_err());
        assert!(valid_request("a", "a", "request-1", 5100, 100).is_ok());
    }
    #[test]
    fn rejects_empty_and_oversized_command_frames() {
        assert!(length(0u32.to_be_bytes()).is_err());
        assert!(length(4097u32.to_be_bytes()).is_err());
        assert_eq!(length(4096u32.to_be_bytes()).unwrap(), 4096);
    }
    fn private_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }
    #[tokio::test]
    async fn owns_private_endpoint_rejects_replay_and_removes_on_drop() {
        let dir = private_fixture();
        let mut adapter = Adapter::start(dir.path()).unwrap();
        let receive = Request::Receive {
            generation: adapter.generation.clone(),
            request_id: "request-1".into(),
            source_device_id: "paired-peer".into(),
            deadline_ms: 200,
            lease_ms: 1_000,
        };
        assert!(adapter.admit(&receive, 100).is_ok());
        assert!(adapter.admit(&receive, 100).is_err());
        let invalid = Request::Receive {
            generation: adapter.generation.clone(),
            request_id: "request-2".into(),
            source_device_id: "paired-peer".into(),
            deadline_ms: 200,
            lease_ms: 120_001,
        };
        assert!(adapter.admit(&invalid, 100).is_err());
        assert!(Adapter::start(dir.path()).is_err());
        let socket = dir.path().join(SOCKET);
        assert_eq!(
            socket.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(adapter);
        assert!(!socket.exists());
        let replacement = Adapter::start(dir.path()).unwrap();
        assert!(
            valid_request(
                &replacement.generation,
                match &receive {
                    Request::Receive { generation, .. } => generation,
                    _ => unreachable!(),
                },
                "new-request",
                200,
                100
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn refuses_public_profile_and_symlink_endpoint() {
        let dir = private_fixture();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Adapter::start(dir.path()).is_err());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink("missing-target", dir.path().join(SOCKET)).unwrap();
        assert!(Adapter::start(dir.path()).is_err());
    }
    #[tokio::test]
    async fn private_request_roundtrip_only_enqueues_gui_action() {
        let dir = private_fixture();
        let mut adapter = Adapter::start(dir.path()).unwrap();
        let path = dir.path().to_path_buf();
        let request = tokio::task::spawn_blocking(move || exchange(&path, &Request::Status));
        let command = tokio::time::timeout(Duration::from_secs(1), adapter.commands.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(command.request, Request::Status));
        command
            .reply
            .send(serde_json::json!({"ok":true,"state":"test-gui"}).into())
            .unwrap_or_else(|_| panic!("test reply receiver closed"));
        assert_eq!(request.await.unwrap().unwrap()["state"], "test-gui");
    }
    #[tokio::test]
    async fn unconsumed_accepted_reply_revokes_even_after_successful_oneshot_send() {
        let backend = std::sync::Arc::new(
            crate::UnifiedServiceOwner::start(crate::UnifiedServiceOwnerConfig::default())
                .await
                .unwrap(),
        );
        let owner = super::super::original_owner::OriginalOwner::new(backend);
        let timer = owner
            .begin_directed_receive("ack-race", "source", Duration::from_secs(30))
            .unwrap();
        let (tx, rx) = oneshot::channel();
        let reply = Reply::accepted(
            serde_json::json!({"ok":true,"state":"accepted","request_id":"ack-race"}),
            &owner,
        );
        assert!(tx.send(reply).is_ok());
        assert!(owner.directed_receive_current("ack-race"));
        // Model timeout dropping a response already queued by GUI send.
        drop(rx);
        assert!(!owner.directed_receive_current("ack-race"));
        timer.abort();
        let timer = owner
            .begin_directed_receive("written", "source", Duration::from_secs(30))
            .unwrap();
        let mut written = Reply::accepted(serde_json::json!({"request_id":"written"}), &owner);
        written.written();
        drop(written);
        assert!(owner.directed_receive_current("written"));
        owner.cancel_directed_receive("written");
        timer.abort();
    }
}
