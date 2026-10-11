//! Local, same-user control plane. The service owns network registration and
//! the canonical profile lock; frontends own only outgoing sessions and windows.
//! No shell execution, launch/stop operations or media copies cross this socket.
use crate::{UnifiedRuntimeConfig, UnifiedRuntimeHandle, UnifiedServiceOwner};
use client::{MeshPairingBackend, MeshPairingControl, MeshPairingMessageKind, MeshPairingSnapshot};
use remote_core::{discovery::DiscoveryPeerSnapshot, mesh::AppPrivateMeshConfigStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::{self, Read, Write}, os::unix::{fs::{FileTypeExt, MetadataExt, PermissionsExt}, net::UnixStream}, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::UnixListener, sync::{watch, Semaphore}};

type Error = Box<dyn std::error::Error + Send + Sync>;
const SOCKET: &str = "background.sock";
pub const ROLE_MARKER: &str = "background-service.enabled";
const LIMIT: usize = 256 * 1024;
const SCHEMA: u32 = 1;
const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Request { Snapshot, CreateGroup, JoinGroup(String), Reload(String), InspectConnection { connection_id:u32 } }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    version: String,
    generation: String,
    executable_sha256: String,
    pid: u32,
    peers: DiscoveryPeerSnapshot,
    pairing: MeshPairingSnapshot,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Response { Snapshot(Snapshot), Rejected(String), ConnectionDiagnostics(Vec<host::lifecycle::ConnectionLifecycleSnapshot>) }

fn uid() -> u32 { unsafe { libc::geteuid() } }
fn private_directory(path: &Path) -> io::Result<()> {
    let metadata = path.symlink_metadata()?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Background profile must be a private, same-user directory"));
    }
    Ok(())
}
fn private_socket(path: &Path) -> io::Result<()> {
    let metadata = path.symlink_metadata()?;
    if !metadata.file_type().is_socket() || metadata.uid() != uid() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Background endpoint must be a private, same-user socket"));
    }
    Ok(())
}
fn frame(bytes: &[u8]) -> io::Result<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > LIMIT { return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid service frame length")); }
    let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(bytes);
    Ok(framed)
}
fn valid_length(header: [u8; 4]) -> io::Result<usize> {
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > LIMIT { return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid service frame length")); }
    Ok(length)
}

/// Bind only after obtaining the existing canonical runtime lock. Refuse a live
/// endpoint; remove an owned stale socket only when no server is listening.
pub(crate) struct Service { path: PathBuf, task: tokio::task::JoinHandle<()> }
impl Service {
    pub(crate) fn start(profile: &Path, runtime: Arc<UnifiedRuntimeHandle>, guard: crate::desktop::HeadlessInstanceGuard) -> Result<Self, Error> {
        private_directory(profile)?;
        let path = profile.join(SOCKET);
        match path.symlink_metadata() {
            Ok(_) => {
                private_socket(&path)?;
                match UnixStream::connect(&path) {
                    Ok(_) => return Err("A background service already owns this endpoint".into()),
                    Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(&path)?,
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let executable_sha256 = executable_hash()?;
        let generation = format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos());
        let limit = Arc::new(Semaphore::new(8));
        let guard = Arc::new(guard);
        let task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            while let Ok((mut socket, _)) = listener.accept().await {
                while children.try_join_next().is_some() {}
                if socket.peer_cred().map(|c| c.uid()).ok() != Some(uid()) { continue; }
                let Ok(permit) = limit.clone().try_acquire_owned() else { continue; };
                let guard = guard.clone(); let runtime = runtime.clone(); let generation = generation.clone(); let executable_sha256 = executable_sha256.clone();
                children.spawn(async move {
                    let _guard = guard;
                    let _permit = permit;
                    let _ = tokio::time::timeout(TIMEOUT, async {
                        let mut header = [0; 4]; socket.read_exact(&mut header).await?;
                        let mut bytes = vec![0; valid_length(header)?]; socket.read_exact(&mut bytes).await?;
                        let request: Request = serde_json::from_slice(&bytes)?;
                        let response = answer(request, &runtime, generation, executable_sha256);
                        socket.write_all(&frame(&serde_json::to_vec(&response)?)?).await?;
                        Ok::<(), Error>(())
                    }).await;
                });
            }
        });
        Ok(Self { path, task })
    }
}
impl Drop for Service {
    fn drop(&mut self) { self.task.abort(); let _ = fs::remove_file(&self.path); }
}
fn answer(request: Request, runtime: &UnifiedRuntimeHandle, generation: String, executable_sha256: String) -> Response {
    if let Request::InspectConnection{connection_id}=request {
        return Response::ConnectionDiagnostics(host::lifecycle::inspect_connection(connection_id));
    }
    let Some(pairing) = &runtime.mesh_pairing else { return Response::Rejected("Background pairing is unavailable".into()); };
    let changing = !matches!(request, Request::Snapshot);
    if changing && !matches!(runtime.owner.runtime().lock().unwrap().role_state(), remote_core::role::RoleState::Idle) {
        return Response::Rejected("Disconnect background sessions before changing the network".into());
    }
    let pairing = match request {
        Request::Snapshot => pairing.snapshot(),
        Request::CreateGroup => pairing.create_new_group(),
        Request::JoinGroup(code) => pairing.join_from_invite_code(&code),
        Request::Reload(message) => pairing.request_runtime_reload(message),
        Request::InspectConnection{..}=>unreachable!(),
    };
    let peers = runtime.owner.discovery_snapshot_rx().map(|rx| rx.borrow().clone()).unwrap_or_default();
    Response::Snapshot(Snapshot { schema: SCHEMA, version: env!("CARGO_PKG_VERSION").into(), generation, executable_sha256, pid: std::process::id(), peers, pairing })
}

#[derive(Clone)]
pub struct Client { profile: PathBuf, executable_sha256: String, cached: Arc<Mutex<Snapshot>> }
impl Client {
    pub fn connect(profile: &Path) -> Result<Self, Error> {
        let executable_sha256 = executable_hash()?;
        let snapshot = request(profile, &Request::Snapshot, &executable_sha256)?;
        refresh_secret(profile)?;
        Ok(Self { profile: profile.into(), executable_sha256, cached: Arc::new(Mutex::new(snapshot)) })
    }
    fn exchange(&self, command: Request) -> Result<DiscoveryPeerSnapshot, Error> {
        let snapshot = request(&self.profile, &command, &self.executable_sha256)?;
        let mut cached = self.cached.lock().unwrap();
        // A service restart invalidates all GUI session/source/file bindings;
        // it does not launch a new GUI or silently keep stale routed endpoints.
        if cached.generation != snapshot.generation || cached.pairing.invite_code != snapshot.pairing.invite_code {
            refresh_secret(&self.profile)?;
        }
        let peers = snapshot.peers.clone(); *cached = snapshot; Ok(peers)
    }
    pub fn background_pid(&self) -> u32 { self.cached.lock().unwrap().pid }
    pub fn status(&self) -> serde_json::Value {
        let cached = self.cached.lock().unwrap();
        serde_json::json!({"schema": SCHEMA, "role": "background-service", "pid": cached.pid,
            "version": cached.version, "executable_sha256": cached.executable_sha256,
            "discovered_routes": cached.peers.len(), "profile": self.profile})
    }
    pub async fn frontend_runtime(&self) -> Result<UnifiedRuntimeHandle, Error> {
        let (tx, rx) = watch::channel(self.cached.lock().unwrap().peers.clone());
        let owner = Arc::new(UnifiedServiceOwner::from_background_snapshot(rx).await?);
        let epoch = owner.frontend_epoch().unwrap();
        let mut runtime = UnifiedRuntimeHandle::from_workspace_owner(owner);
        runtime.mesh_pairing = Some(MeshPairingControl::from_backend(Arc::new(self.clone())));
        let client = self.clone();
        runtime.media_sink_tasks.push(crate::AbortOnDropTask(tokio::spawn(async move {
            loop {
                let previous = client.cached.lock().unwrap().generation.clone();
                let worker = client.clone();
                let result = tokio::task::spawn_blocking(move || worker.exchange(Request::Snapshot)).await;
                let snapshot = match result {
                    Ok(Ok(snapshot)) => snapshot,
                    _ => { client.unavailable(); DiscoveryPeerSnapshot::default() },
                };
                if previous != client.cached.lock().unwrap().generation { epoch.fetch_add(1, std::sync::atomic::Ordering::Release); }
                if tx.send(snapshot).is_err() { break; }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        })));
        Ok(runtime)
    }
    fn unavailable(&self) {
        let mut cached = self.cached.lock().unwrap();
        // Invalidate once per outage, not on every poll.
        if cached.generation != "unavailable" {
            cached.generation = "unavailable".into();
            remote_core::shared_files::shared_file_catalog().clear();
        }
        cached.pairing.message = "Background service is unavailable; waiting for it to recover.".into();
        cached.pairing.message_kind = MeshPairingMessageKind::Error;
    }
    fn pairing_command(&self, command: Request) -> MeshPairingSnapshot {
        if self.exchange(command).is_err() { self.unavailable(); }
        self.snapshot()
    }
}
fn executable_hash() -> Result<String, Error> {
    Ok(format!("{:x}", Sha256::digest(fs::read(std::env::current_exe()?)?)))
}
fn request(profile: &Path, command: &Request, executable_sha256: &str) -> Result<Snapshot, Error> {
    private_directory(profile)?; let path = profile.join(SOCKET); private_socket(&path)?;
    let mut socket = UnixStream::connect(&path)?;
    socket.set_read_timeout(Some(TIMEOUT))?; socket.set_write_timeout(Some(TIMEOUT))?;
    // The private directory and socket ownership restrict both endpoints to the
    // logged-in user. Neither endpoint accepts a remote TCP address.
    private_socket(&path)?;
    socket.write_all(&frame(&serde_json::to_vec(command)?)?)?;
    let mut header = [0; 4]; socket.read_exact(&mut header)?;
    let mut bytes = vec![0; valid_length(header)?]; socket.read_exact(&mut bytes)?;
    let snapshot = match serde_json::from_slice::<Response>(&bytes)? {
        Response::Snapshot(snapshot) => snapshot,
        Response::Rejected(_) => return Err("Background service rejected the operation".into()),
        Response::ConnectionDiagnostics(_)=>return Err("Expected background Snapshot".into()),
    };
    if snapshot.executable_sha256 != executable_sha256 || snapshot.schema != SCHEMA || snapshot.version != env!("CARGO_PKG_VERSION") || snapshot.pairing.config_dir != profile {
        return Err("Background service executable/protocol/version/profile mismatch; upgrade the service and GUI together".into());
    }
    Ok(snapshot)
}
fn refresh_secret(profile: &Path) -> Result<(), Error> {
    // Read existing membership only. A GUI never generates or saves credentials.
    let config = AppPrivateMeshConfigStore::new(profile).load()?.ok_or("Background profile has no device identity")?;
    remote_core::session_crypto::use_paired_session_secret(config.network_secret.expose_secret());
    Ok(())
}
impl MeshPairingBackend for Client {
    fn snapshot(&self) -> MeshPairingSnapshot { self.cached.lock().unwrap().pairing.clone() }
    fn create_new_group(&self) -> MeshPairingSnapshot { self.pairing_command(Request::CreateGroup) }
    fn join_from_invite_code(&self, code: &str) -> MeshPairingSnapshot { self.pairing_command(Request::JoinGroup(code.into())) }
    fn request_runtime_reload(&self, message: String) -> MeshPairingSnapshot { self.pairing_command(Request::Reload(message)) }
}

/// Explicit service mode owns the same lock as legacy standalone/headless.
/// It never invokes Application::new or GUI activation.
pub async fn run(mut config: UnifiedRuntimeConfig) -> Result<(), Error> {
    private_directory(&config.mesh_dir)?;
    let Some(owner) = crate::desktop::claim_headless_runtime(&config.mesh_dir)? else {
        return Err("Another runtime owns this profile; no second background host was started".into());
    };
    let identity = AppPrivateMeshConfigStore::new(&config.mesh_dir).load_or_generate(&config.display_name)?;
    remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
    config.enable_viewer_media = false; config.enable_workspace_viewer = false; config.enable_client_receiver = false;
    let runtime = Arc::new(crate::start_unified_runtime(config.clone()).await?);
    let _service = Service::start(&config.mesh_dir, runtime, owner)?;
    // Persist the launch role, not credentials. During a crash/restart gap a
    // manual app launch must still attach/fail, never become a standalone host.
    let marker = config.mesh_dir.join(ROLE_MARKER);
    use std::os::unix::fs::OpenOptionsExt;
    match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&marker) {
        Ok(file) => file.sync_all()?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let meta = marker.symlink_metadata()?;
            if !meta.is_file() || meta.uid() != uid() || meta.nlink() != 1 || meta.len() != 0 || meta.permissions().mode() & 0o077 != 0 {
                return Err("Background launch-role marker is unsafe".into());
            }
        }
        Err(error) => return Err(error.into()),
    }
    tokio::signal::ctrl_c().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connection_diagnostic_is_readonly_known_id_and_round_trips(){
        let request:Request=serde_json::from_str(r#"{"InspectConnection":{"connection_id":4294967295}}"#).unwrap();
        assert!(matches!(request,Request::InspectConnection{connection_id:u32::MAX}));
        let records=host::lifecycle::inspect_connection(u32::MAX);assert!(records.is_empty());
        let bytes=serde_json::to_vec(&Response::ConnectionDiagnostics(records)).unwrap();
        assert!(matches!(serde_json::from_slice::<Response>(&bytes).unwrap(),Response::ConnectionDiagnostics(records) if records.is_empty()));
        assert!(serde_json::from_str::<Request>(r#"{"InspectConnection":{"connection_id":1,"all":true}}"#).is_err());
    }
    #[test]
    fn bounded_frames_and_private_endpoints_fail_closed() {
        assert!(valid_length(0u32.to_be_bytes()).is_err());
        assert!(valid_length(((LIMIT+1) as u32).to_be_bytes()).is_err());
        assert!(frame(&vec![0; LIMIT+1]).is_err());
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(dir.path()).is_err());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join(SOCKET); fs::write(&path, "not a socket").unwrap();
        assert!(private_socket(&path).is_err());
        assert!(Client::connect(dir.path()).is_err());
    }
    #[tokio::test(flavor="multi_thread", worker_threads=2)]
    async fn frontend_is_not_a_second_network_owner_and_outlives_only_its_windows() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = UnifiedRuntimeConfig::app_defaults(); config.mesh_dir = dir.path().into();
        config.enable_discovery = false; config.enable_p2p = false; config.relay_endpoint = None;
        config.enable_passive_host = false; config.enable_client_receiver = false; config.enable_viewer_media = false;
        let guard = crate::desktop::claim_headless_runtime(dir.path()).unwrap().unwrap();
        let runtime = Arc::new(crate::start_unified_runtime(config).await.unwrap());
        let service = Service::start(dir.path(), runtime, guard).unwrap();
        let profile = dir.path().to_owned();
        let client = tokio::task::spawn_blocking(move || Client::connect(&profile)).await.unwrap().unwrap();
        let original = ["mesh.conf", "mesh.secret"].map(|name| {
            let meta = dir.path().join(name).metadata().unwrap(); (meta.ino(), meta.len(), meta.mtime(), meta.mtime_nsec())
        });
        let frontend = client.frontend_runtime().await.unwrap();
        assert!(!frontend.owner.owns_discovery()); assert!(!frontend.owner.owns_p2p()); assert!(!frontend.owner.owns_relay());
        assert!(frontend.viewer_frame.is_none());
        assert!(crate::desktop::claim_headless_runtime(dir.path()).unwrap().is_none());
        drop(frontend);
        assert_eq!(original, ["mesh.conf", "mesh.secret"].map(|name| {
            let meta = dir.path().join(name).metadata().unwrap(); (meta.ino(), meta.len(), meta.mtime(), meta.mtime_nsec())
        }));
        let profile = dir.path().to_owned();
        assert!(tokio::task::spawn_blocking(move || request(&profile, &Request::Snapshot, "wrong binary")).await.unwrap().is_err());
        let profile = dir.path().to_owned();
        assert!(tokio::task::spawn_blocking(move || Client::connect(&profile)).await.unwrap().is_ok());
        drop(service);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(crate::desktop::claim_headless_runtime(dir.path()).unwrap().is_some());
    }
}
