//! Local, same-user control plane. The service owns network registration and
//! the canonical profile lock; frontends own only outgoing sessions and windows.
//! No shell execution or media copies cross this endpoint; GUI activation uses the installed executable.
use crate::{UnifiedRuntimeConfig, UnifiedRuntimeHandle, UnifiedServiceOwner};
use client::{MeshPairingBackend, MeshPairingControl, MeshPairingMessageKind, MeshPairingSnapshot};
use remote_core::{discovery::DiscoveryPeerSnapshot, mesh::AppPrivateMeshConfigStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::{self, Read, Write}, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, sync::{watch, Semaphore}};

type Error = Box<dyn std::error::Error + Send + Sync>;
const SOCKET: &str = "background.sock";
pub const ROLE_MARKER: &str = "background-service.enabled";
const LIMIT: usize = 256 * 1024;
const SCHEMA: u32 = 2;
const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Request { Delivery { generation:String, executable_sha256:String, intent:host::directed_delivery::LocalIntent }, Snapshot, CreateGroup, JoinGroup(String), Reload(String), InspectConnection { connection_id:u32 } }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) enum OwnerMode { Standalone, Background }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    owner_mode: OwnerMode,
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

pub(crate) use crate::private_ipc::private_directory;
#[cfg(unix)]
pub(crate) use crate::private_ipc::private_socket;

/// A persistent background role never falls back to a second runtime. An
/// unmarked standalone crash may leave an owned Unix socket, but only verified
/// absence/refusal permits the caller to try the original exclusive profile lock.
pub(crate) fn attachment_required(profile:&Path)->Result<bool,Error>{
    let marker=profile.join(ROLE_MARKER);
    match marker.symlink_metadata(){
        Ok(_)=>{crate::private_ipc::private_directory(profile)?;crate::private_ipc::private_file(&marker)?;return Ok(true);},
        Err(e) if e.kind()==io::ErrorKind::NotFound=>{},
        Err(e)=>return Err(e.into()),
    }
    Ok(!crate::private_ipc::absent(profile,SOCKET)?)
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
pub(crate) struct Service { path: PathBuf, task: tokio::task::JoinHandle<()>, delivery_task:tokio::task::JoinHandle<()>, closing:Arc<Mutex<bool>>, broker:Arc<host::directed_delivery::Broker> }
impl Service {
    pub(crate) fn start(profile: &Path, runtime: Arc<UnifiedRuntimeHandle>, guard: crate::desktop::HeadlessInstanceGuard) -> Result<Self, Error> {
        Self::start_owner(profile,runtime,Arc::new(guard),OwnerMode::Background)
    }
    pub(crate) fn start_for_gui(profile:&Path,runtime:Arc<UnifiedRuntimeHandle>,instance:&crate::desktop::instance::Instance)->Result<Self,Error>{
        let guard=instance.service_lock()?;
        Self::start_owner(profile,runtime,guard,OwnerMode::Standalone)
    }
    fn start_owner(profile:&Path,runtime:Arc<UnifiedRuntimeHandle>,guard:Arc<dyn std::any::Any+Send+Sync>,owner_mode:OwnerMode)->Result<Self,Error>{
        private_directory(profile)?;
        let path=profile.join(SOCKET);
        let mut listener=crate::private_ipc::Listener::bind(profile,SOCKET)?;
        let executable_sha256 = executable_hash()?;
        let generation = format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos());
        let identity=AppPrivateMeshConfigStore::new(profile).load()?.ok_or("Background identity unavailable")?;
        runtime.owner.delivery.set_identity(identity.node_id);
        let incoming=runtime.owner.delivery_incoming.lock().unwrap().take().ok_or("Delivery worker already attached")?;
        let delivery_guard=guard.clone();
        #[cfg(feature="gpui-restoration")]
        let delivery_task={let profile=profile.to_owned();let hash=executable_sha256.clone();tokio::spawn(async move {let _guard=delivery_guard;crate::directed_delivery::receive_worker(profile,hash,owner_mode,incoming).await;})};
        #[cfg(not(feature="gpui-restoration"))]
        let delivery_task=tokio::spawn(async move {let _guard=delivery_guard;let mut incoming=incoming;while let Some(offer)=incoming.recv().await {offer.finish(protocol::session::DeliveryState::Rejected,"Receiver GUI is not compiled");}});
        let limit = Arc::new(Semaphore::new(8));
        let profile=profile.to_owned();
        let registration_lock=Arc::new(tokio::sync::Mutex::new(()));
        let closing=Arc::new(Mutex::new(false));let closed=closing.clone();let broker=runtime.owner.delivery.clone();
        let task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            while let Ok(mut socket) = listener.accept().await {
                while children.try_join_next().is_some() {}
                let Ok(permit) = limit.clone().try_acquire_owned() else { continue; };
                let guard = guard.clone(); let runtime = runtime.clone(); let generation = generation.clone(); let executable_sha256 = executable_sha256.clone();
                let profile=profile.clone();let registration_lock=registration_lock.clone();let closed=closed.clone();
                children.spawn(async move {
                    let _guard = guard;
                    let _permit = permit;
                    let _ = tokio::time::timeout(TIMEOUT, async {
                        let mut header = [0; 4]; socket.read_exact(&mut header).await?;
                        let mut bytes = vec![0; valid_length(header)?]; socket.read_exact(&mut bytes).await?;
                        let request: Request = serde_json::from_slice(&bytes)?;
                        let registration=match &request {
                            Request::Delivery{generation:service,executable_sha256:hash,intent:host::directed_delivery::LocalIntent::Register{generation:gui,..}} if service==&generation&&hash==&executable_sha256=>Some(gui.clone()),
                            _=>None,
                        };
                        let response=if let Some(gui)=registration {
                            let _serial=registration_lock.lock().await;
                            let directory=if owner_mode==OwnerMode::Background {profile.join("gui-state")}else{profile.clone()};
                            #[cfg(feature="gpui-restoration")]
                            let verified=tokio::task::spawn_blocking(move||crate::desktop::directed_receive::exchange(&directory,&crate::desktop::directed_receive::Request::Status)).await
                                .ok().and_then(Result::ok).is_some_and(|status|status["ok"]==true&&status["generation"].as_str()==Some(&gui)&&status["executable_sha256"].as_str()==Some(&executable_sha256));
                            #[cfg(not(feature="gpui-restoration"))]
                            let verified={let _=directory;false};
                            let closing=closed.lock().unwrap();
                            if *closing {Response::Rejected("GUI owner stopped".into())}
                            else if verified {runtime.owner.delivery.verified_gui_generation(&gui);answer(request,&runtime,generation,executable_sha256,owner_mode)}
                            else {Response::Rejected("Sending GUI generation or executable changed".into())}
                        } else {let closing=closed.lock().unwrap();if *closing{Response::Rejected("GUI owner stopped".into())}else{answer(request,&runtime,generation,executable_sha256,owner_mode)}};
                        socket.write_all(&frame(&serde_json::to_vec(&response)?)?).await?;
                        Ok::<(), Error>(())
                    }).await;
                });
            }
        });
        Ok(Self { path, task, delivery_task, closing, broker })
    }
}
impl Drop for Service {
    fn drop(&mut self) { let mut closed=self.closing.lock().unwrap();*closed=true;self.broker.verified_gui_generation("");self.broker.reconcile(|_|false); self.delivery_task.abort(); self.task.abort(); crate::private_ipc::remove_endpoint(&self.path); }
}
fn answer(mut request: Request, runtime: &UnifiedRuntimeHandle, generation: String, executable_sha256: String, owner_mode:OwnerMode) -> Response {
    if let Request::Delivery {generation:expected,executable_sha256:hash,intent} = request {
        if expected!=generation || hash!=executable_sha256 {return Response::Rejected("Background generation changed".into());}
        if let Err(error)=runtime.owner.delivery.local(intent){return Response::Rejected(error);}
        request=Request::Snapshot;
    }
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
        Request::InspectConnection{..}|Request::Delivery{..}=>unreachable!(),
    };
    let peers = runtime.owner.discovery_snapshot_rx().map(|rx| rx.borrow().clone()).unwrap_or_default();
    Response::Snapshot(Snapshot { schema: SCHEMA, owner_mode, version: env!("CARGO_PKG_VERSION").into(), generation, executable_sha256, pid: std::process::id(), peers, pairing })
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
    pub(crate) fn delivery(&self,intent:host::directed_delivery::LocalIntent)->Result<(),Error> {
        let generation=self.cached.lock().unwrap().generation.clone();
        let snapshot=request(&self.profile,&Request::Delivery{generation:generation.clone(),executable_sha256:self.executable_sha256.clone(),intent},&self.executable_sha256)?;
        if snapshot.generation!=generation {return Err("Background generation changed".into());}
        Ok(())
    }
    pub(crate) fn owner_mode(&self)->OwnerMode {self.cached.lock().unwrap().owner_mode}
    pub fn background_pid(&self) -> u32 { self.cached.lock().unwrap().pid }
    pub fn status(&self) -> serde_json::Value {
        let cached = self.cached.lock().unwrap();
        serde_json::json!({"schema": SCHEMA, "role": if cached.owner_mode==OwnerMode::Background {"background-service"}else{"standalone-gui"}, "pid": cached.pid,
            "version": cached.version, "executable_sha256": cached.executable_sha256,
            "discovered_routes": cached.peers.len(), "profile": self.profile})
    }
    pub async fn frontend_runtime(&self) -> Result<UnifiedRuntimeHandle, Error> {
        if self.owner_mode()!=OwnerMode::Background{return Err("Standalone GUI already owns its runtime".into());}
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
pub(crate) fn executable_hash() -> Result<String, Error> {
    Ok(format!("{:x}", Sha256::digest(fs::read(std::env::current_exe()?)?)))
}
fn request(profile: &Path, command: &Request, executable_sha256: &str) -> Result<Snapshot, Error> {
    let mut socket=crate::private_ipc::connect(profile,SOCKET,TIMEOUT)?;
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
    let mut options=fs::OpenOptions::new();options.write(true).create_new(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    match options.open(&marker) {
        Ok(file) => file.sync_all()?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            crate::private_ipc::private_file(&marker)?;
            if marker.metadata()?.len()!=0 {
                return Err("Background launch-role marker is unsafe".into());
            }
        }
        Err(error) => return Err(error.into()),
    }
    tokio::signal::ctrl_c().await?;
    Ok(())
}

#[cfg(all(test,unix))]
mod tests {
    use std::os::unix::{fs::{PermissionsExt,MetadataExt}};
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
    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn standalone_service_reuses_gui_lock_and_cannot_create_a_frontend_runtime() {
        let dir=tempfile::tempdir().unwrap();fs::set_permissions(dir.path(),fs::Permissions::from_mode(0o700)).unwrap();
        let instance=crate::desktop::instance::Instance::acquire(dir.path()).unwrap().unwrap();
        let mut config=UnifiedRuntimeConfig::app_defaults();config.mesh_dir=dir.path().into();config.enable_discovery=false;config.enable_p2p=false;config.relay_endpoint=None;config.enable_passive_host=false;config.enable_client_receiver=false;config.enable_viewer_media=false;
        let runtime=Arc::new(crate::start_unified_runtime(config).await.unwrap());
        let service=Service::start_for_gui(dir.path(),runtime,&instance).unwrap();
        let profile=dir.path().to_owned();let client=tokio::task::spawn_blocking(move||Client::connect(&profile)).await.unwrap().unwrap();
        assert_eq!(client.owner_mode(),OwnerMode::Standalone);assert!(client.frontend_runtime().await.is_err());
        drop(instance);assert!(crate::desktop::claim_headless_runtime(dir.path()).is_err());
        drop(service);tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(crate::desktop::claim_headless_runtime(dir.path()).unwrap().is_some());
    }

    #[tokio::test]
    async fn unmarked_crash_socket_can_reclaim_lock_but_background_marker_never_falls_back(){
        let dir=tempfile::tempdir().unwrap();fs::set_permissions(dir.path(),fs::Permissions::from_mode(0o700)).unwrap();
        let stale=crate::private_ipc::Listener::bind(dir.path(),SOCKET).unwrap();drop(stale);
        assert!(!attachment_required(dir.path()).unwrap());
        let instance=crate::desktop::instance::Instance::acquire(dir.path()).unwrap().unwrap();
        let live=crate::private_ipc::Listener::bind(dir.path(),SOCKET).unwrap();assert!(attachment_required(dir.path()).unwrap());drop(live);drop(instance);
        fs::write(dir.path().join(ROLE_MARKER),"").unwrap();fs::set_permissions(dir.path().join(ROLE_MARKER),fs::Permissions::from_mode(0o600)).unwrap();
        assert!(attachment_required(dir.path()).unwrap());assert!(Client::connect(dir.path()).is_err());
    }

}
