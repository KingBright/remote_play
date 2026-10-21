//! Local, same-user control plane. The service owns network registration and
//! the canonical profile lock; frontends own only outgoing sessions and windows.
//! No shell execution or media copies cross this endpoint; GUI activation uses the installed executable.
use crate::{UnifiedRuntimeConfig, UnifiedRuntimeHandle, UnifiedServiceOwner};
use crate::service_contract::{self, Offer, Selection};
use client::{MeshPairingBackend, MeshPairingControl, MeshPairingMessageKind, MeshPairingSnapshot};
use remote_core::{discovery::DiscoveryPeerSnapshot, mesh::AppPrivateMeshConfigStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::{self, Read, Write}, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, sync::{watch, Semaphore}};

mod exit_observer;
mod stop_policy;

type Error = Box<dyn std::error::Error + Send + Sync>;
const SOCKET: &str = "background.sock";
pub const ROLE_MARKER: &str = "background-service.enabled";
const LIMIT: usize = 256 * 1024;
const SCHEMA: u32 = 2;
const TIMEOUT: Duration = Duration::from_secs(2);
const STOP_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Request { Shutdown { generation: String, executable_sha256: String }, Negotiate { generation: String, executable_sha256: String, offer: Offer }, Delivery { generation:String, executable_sha256:String, intent:host::directed_delivery::LocalIntent }, NetworkDiagnostics { generation: String, executable_sha256: String }, Snapshot, CreateGroup, JoinGroup(String), Reload(String), InspectConnection { connection_id:u32 } }
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
enum Response { StopPrepared { generation: String, executable_sha256: String, pid: u32, cleanup_verified: bool }, Negotiated { generation: String, executable_sha256: String, selection: Result<Selection, service_contract::Rejection> }, NetworkDiagnostics(ServiceNetworkSnapshot), Snapshot(Snapshot), Rejected(String), ConnectionDiagnostics(Vec<host::lifecycle::ConnectionLifecycleSnapshot>) }

/// Deliberately separate from Snapshot: contains no pairing or raw device data.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceNetworkSnapshot {
    generation: String,
    executable_sha256: String,
    pid: u32,
    build_identity: Option<serde_json::Value>,
    network: crate::network_diagnostics::NetworkSnapshot,
}

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
pub(crate) struct Service { path: PathBuf, task: tokio::task::JoinHandle<()>, delivery_task:tokio::task::JoinHandle<()>, closing:Arc<Mutex<bool>>, broker:Arc<host::directed_delivery::Broker>, stopped: Arc<tokio::sync::Notify> }
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
        let stopped = Arc::new(tokio::sync::Notify::new()); let finished = stopped.clone();
        let task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            while let Ok(mut socket) = listener.accept().await {
                while children.try_join_next().is_some() {}
                let Ok(permit) = limit.clone().try_acquire_owned() else { continue; };
                let guard = guard.clone(); let runtime = runtime.clone(); let generation = generation.clone(); let executable_sha256 = executable_sha256.clone();
                let profile=profile.clone();let registration_lock=registration_lock.clone();let closed=closed.clone(); let finished=finished.clone();
                children.spawn(async move {
                    let _guard = guard;
                    let _permit = permit;
                    let _ = tokio::time::timeout(STOP_TIMEOUT, async {
                        let request: Request = tokio::time::timeout(TIMEOUT, async {
                            let mut header = [0; 4]; socket.read_exact(&mut header).await?;
                            let mut bytes = vec![0; valid_length(header)?]; socket.read_exact(&mut bytes).await?;
                            Ok::<Request, Error>(serde_json::from_slice(&bytes)?)
                        }).await??;
                        if let Request::Shutdown { generation: expected, executable_sha256: hash } = &request {
                            let admitted = admit_shutdown(owner_mode, expected, hash, &generation, &executable_sha256, &closed);
                            let response = match admitted {
                                Err(error) => Response::Rejected(error),
                                Ok(()) => match runtime.owner.shutdown_host().await {
                                    Ok(()) => Response::StopPrepared { generation: generation.clone(), executable_sha256: executable_sha256.clone(), pid: std::process::id(), cleanup_verified: true },
                                    Err(error) => Response::Rejected(error),
                                },
                            };
                            let success = matches!(response, Response::StopPrepared { .. });
                            let write = socket.write_all(&frame(&serde_json::to_vec(&response)?)?).await;
                            // An interrupted caller gets an unknown result, never a false success.
                            // A completed cleanup exits normally even if the reply socket was lost.
                            if success { finished.notify_one(); }
                            write?; return Ok::<(), Error>(());
                        }
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
        Ok(Self { path, task, delivery_task, closing, broker, stopped })
    }
}
impl Drop for Service {
    fn drop(&mut self) { let mut closed=self.closing.lock().unwrap();*closed=true;self.broker.verified_gui_generation("");self.broker.reconcile(|_|false); self.delivery_task.abort(); self.task.abort(); crate::private_ipc::remove_endpoint(&self.path); }
}
fn admit_shutdown(mode: OwnerMode, expected: &str, hash: &str, generation: &str, actual_hash: &str, closing: &Mutex<bool>) -> Result<(), String> {
    if mode != OwnerMode::Background || expected != generation || hash != actual_hash {
        return Err("Shutdown requires the exact background owner generation and executable".into());
    }
    let mut state = closing.lock().map_err(|_| "Shutdown state unavailable")?;
    if *state { return Err("Background shutdown already requested; outcome must be observed".into()); }
    *state = true; Ok(())
}

fn answer(mut request: Request, runtime: &UnifiedRuntimeHandle, generation: String, executable_sha256: String, owner_mode:OwnerMode) -> Response {
    if let Request::Negotiate { generation: expected, executable_sha256: hash, offer } = request {
        return answer_negotiation(expected, hash, offer, generation, executable_sha256);
    }
    if let Request::Delivery {generation:expected,executable_sha256:hash,intent} = request {
        if expected!=generation || hash!=executable_sha256 {return Response::Rejected("Background generation changed".into());}
        if let Err(error)=runtime.owner.delivery.local(intent){return Response::Rejected(error);}
        request=Request::Snapshot;
    }
    if let Request::InspectConnection{connection_id}=request {
        return Response::ConnectionDiagnostics(host::lifecycle::inspect_connection(connection_id));
    }
    if let Request::NetworkDiagnostics { generation: expected, executable_sha256: hash } = request {
        if expected != generation || hash != executable_sha256 {
            return Response::Rejected("Background generation or executable changed".into());
        }
        return Response::NetworkDiagnostics(ServiceNetworkSnapshot {
            generation, executable_sha256, pid: std::process::id(),
            build_identity: crate::build_identity::compiled(),
            network: runtime.owner.network_snapshot(),
        });
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
        Request::Shutdown{..}|Request::Negotiate{..}|Request::InspectConnection{..}|Request::Delivery{..}|Request::NetworkDiagnostics{..}=>unreachable!(),
    };
    let peers = runtime.owner.discovery_snapshot_rx().map(|rx| rx.borrow().clone()).unwrap_or_default();
    Response::Snapshot(Snapshot { schema: SCHEMA, owner_mode, version: env!("CARGO_PKG_VERSION").into(), generation, executable_sha256, pid: std::process::id(), peers, pairing })
}

fn answer_negotiation(expected: String, hash: String, offer: Offer, generation: String, executable_sha256: String) -> Response {
    // Compatibility is not process identity. Preserve the current exact
    // executable and service-generation gates even for this read-only API.
    if expected != generation || hash != executable_sha256 {
        return Response::Rejected("Background generation or executable changed".into());
    }
    Response::Negotiated { generation, executable_sha256, selection: service_contract::current_offer().negotiate(&offer) }
}

#[derive(Clone)]
pub struct Client { profile: PathBuf, executable_sha256: String, cached: Arc<Mutex<Snapshot>>, contract: Arc<Mutex<Option<Selection>>>, transition: Arc<Mutex<()>>, network: Arc<Mutex<Option<crate::network_diagnostics::NetworkSnapshot>>> }
impl Client {
    pub fn connect(profile: &Path) -> Result<Self, Error> {
        let executable_sha256 = executable_hash()?;
        let snapshot = request(profile, &Request::Snapshot, &executable_sha256)?;
        let contract = negotiate(profile, &snapshot, &executable_sha256)?;
        let network = request_network(profile, &snapshot.generation, &executable_sha256).ok().map(|s| s.network);
        refresh_secret(profile)?;
        Ok(Self { profile: profile.into(), executable_sha256, cached: Arc::new(Mutex::new(snapshot)), contract: Arc::new(Mutex::new(Some(contract))), transition: Arc::new(Mutex::new(())), network: Arc::new(Mutex::new(network)) })
    }
    fn exchange(&self, command: Request) -> Result<DiscoveryPeerSnapshot, Error> {
        self.transition(|| {
            let snapshot = request(&self.profile, &command, &self.executable_sha256)?;
            let (generation, invite, contract) = {
                let cached = self.cached.lock().unwrap();
                (cached.generation.clone(), cached.pairing.invite_code.clone(), self.contract.lock().unwrap().clone())
            };
            // A restart requires a fresh capability result, never the previous
            // process's selection. Keep I/O outside the short cache locks.
            let contract = if generation != snapshot.generation || contract.is_none() {
                negotiate(&self.profile, &snapshot, &self.executable_sha256)?
            } else { contract.unwrap() };
            if generation != snapshot.generation || invite != snapshot.pairing.invite_code {
                refresh_secret(&self.profile)?;
            }
            *self.network.lock().unwrap() = request_network(
                &self.profile, &snapshot.generation, &self.executable_sha256).ok().map(|s| s.network);
            Ok((snapshot, contract))
        })
    }
    fn transition(&self, operation: impl FnOnce() -> Result<(Snapshot, Selection), Error>) -> Result<DiscoveryPeerSnapshot, Error> {
        // Polling and pairing share one transition. A late old success/failure
        // cannot overwrite or invalidate a newer confirmed owner. Include error
        // invalidation in this lock, not in an asynchronous caller afterwards.
        let _serial = self.transition.lock().unwrap();
        match operation() {
            Ok((snapshot, contract)) => {
                let peers = snapshot.peers.clone();
                let mut cached = self.cached.lock().unwrap();
                *self.contract.lock().unwrap() = Some(contract);
                *cached = snapshot;
                Ok(peers)
            }
            Err(error) => { self.unavailable(); Err(error) }
        }
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
        let diagnostics = request_network(&self.profile, &cached.generation, &self.executable_sha256).ok();
        serde_json::json!({"schema": SCHEMA, "role": if cached.owner_mode==OwnerMode::Background {"background-service"}else{"standalone-gui"}, "pid": cached.pid,
            "version": cached.version, "executable_sha256": cached.executable_sha256,
            "service_contract": *self.contract.lock().unwrap(),
            "network_diagnostics": diagnostics,
            "discovered_routes": cached.peers.len(), "profile": self.profile})
    }
    pub fn shutdown_verified(&self) -> Result<serde_json::Value, Error> {
        let _serial = self.transition.lock().map_err(|_| "Background transition unavailable")?;
        let (generation, pid, mode) = { let cached = self.cached.lock().unwrap(); (cached.generation.clone(), cached.pid, cached.owner_mode) };
        if mode != OwnerMode::Background || generation == "unavailable" {
            return Err("No verified independent background owner".into());
        }
        if !self.contract.lock().unwrap().as_ref().is_some_and(|s| s.capabilities.iter().any(|c| c == service_contract::SHUTDOWN)) {
            return Err("Background does not support verified shutdown; upgrade both owners".into());
        }
        stop_policy::verify(pid, &std::env::current_exe()?)?;
        // Register an exact native process-exit event before requesting any stop.
        let observer = exit_observer::ExitObserver::open(pid)?;
        eprintln!("WEAVELO_STOP_INTENT {}", serde_json::json!({"generation":generation,"pid":pid,"executable_sha256":self.executable_sha256,"success":false}));
        let response = exchange_response_timeout(&self.profile, &Request::Shutdown { generation: generation.clone(), executable_sha256: self.executable_sha256.clone() }, STOP_TIMEOUT)?;
        validate_stop_receipt(response, &generation, &self.executable_sha256, pid)?;
        observer.wait(Duration::from_secs(5))?;
        // A replacement or supervised restart is not the stopped generation.
        for _ in 0..3 {
            if crate::private_ipc::exists(&self.profile, SOCKET)? { return Err("Background endpoint remains or was replaced; full exit unconfirmed".into()); }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(serde_json::json!({"kind":"verified-background-stop", "scope":"background-service-only", "generation":generation, "pid":pid,
            "executable_sha256":self.executable_sha256,"cleanup_verified":true,"process_exit_observed":true,
            "endpoint_absent":true,"persistent_startup_changed":false}))
    }

    pub async fn frontend_runtime(&self) -> Result<UnifiedRuntimeHandle, Error> {
        if self.owner_mode()!=OwnerMode::Background{return Err("Standalone GUI already owns its runtime".into());}
        let (tx, rx) = watch::channel(self.cached.lock().unwrap().peers.clone());
        let mut owner = UnifiedServiceOwner::from_background_snapshot(rx).await?;
        owner.frontend_network = Some(self.network.clone());
        let owner = Arc::new(owner);
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
                    _ => DiscoveryPeerSnapshot::default(),
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
        *self.contract.lock().unwrap() = None;
        *self.network.lock().unwrap() = None;
        // Invalidate once per outage, not on every poll.
        if cached.generation != "unavailable" {
            cached.generation = "unavailable".into();
            remote_core::shared_files::shared_file_catalog().clear();
        }
        cached.pairing.message = "Background service is unavailable; waiting for it to recover.".into();
        cached.pairing.message_kind = MeshPairingMessageKind::Error;
    }
    fn pairing_command(&self, command: Request) -> MeshPairingSnapshot {
        let _ = self.exchange(command);
        self.snapshot()
    }
}
pub(crate) fn executable_hash() -> Result<String, Error> {
    Ok(format!("{:x}", Sha256::digest(fs::read(std::env::current_exe()?)?)))
}
fn validate_stop_receipt(response: Response, generation: &str, hash: &str, pid: u32) -> Result<(), Error> {
    match response {
        Response::StopPrepared { generation: actual, executable_sha256, pid: actual_pid, cleanup_verified: true }
            if actual == generation && executable_sha256 == hash && actual_pid == pid => Ok(()),
        Response::Rejected(reason) => Err(format!("Background stop not confirmed: {reason}").into()),
        _ => Err("Background stop receipt owner/cleanup mismatch".into()),
    }
}
fn exchange_response(profile: &Path, command: &Request) -> Result<Response, Error> {
    exchange_response_timeout(profile, command, TIMEOUT)
}
fn exchange_response_timeout(profile: &Path, command: &Request, timeout: Duration) -> Result<Response, Error> {
    let mut socket=crate::private_ipc::connect(profile,SOCKET,timeout)?;
    socket.write_all(&frame(&serde_json::to_vec(command)?)?)?;
    let mut header = [0; 4]; socket.read_exact(&mut header)?;
    let mut bytes = vec![0; valid_length(header)?]; socket.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice::<Response>(&bytes)?)
}
fn request(profile: &Path, command: &Request, executable_sha256: &str) -> Result<Snapshot, Error> {
    let snapshot = match exchange_response(profile, command)? {
        Response::Snapshot(snapshot) => snapshot,
        Response::Rejected(_) => return Err("Background service rejected the operation".into()),
        Response::StopPrepared{..}|Response::ConnectionDiagnostics(_)|Response::Negotiated{..}|Response::NetworkDiagnostics(_)=>return Err("Expected background Snapshot".into()),
    };
    validate_snapshot(&snapshot, profile, executable_sha256)?;
    Ok(snapshot)
}
fn request_network(profile: &Path, generation: &str, hash: &str) -> Result<ServiceNetworkSnapshot, Error> {
    validate_network_response(exchange_response(profile, &Request::NetworkDiagnostics {
        generation: generation.into(), executable_sha256: hash.into(),
    })?, generation, hash)
}
fn validate_network_response(response: Response, generation: &str, hash: &str) -> Result<ServiceNetworkSnapshot, Error> {
    match response {
        Response::NetworkDiagnostics(snapshot)
            if snapshot.generation == generation && snapshot.executable_sha256 == hash => Ok(snapshot),
        _ => Err("Background network diagnostics unavailable or owner changed".into()),
    }
}
fn validate_snapshot(snapshot: &Snapshot, profile: &Path, executable_sha256: &str) -> Result<(), Error> {
    if snapshot.executable_sha256 != executable_sha256 || snapshot.schema != SCHEMA || snapshot.version != env!("CARGO_PKG_VERSION") || snapshot.pairing.config_dir != profile {
        return Err("Background service executable/protocol/version/profile mismatch; upgrade the service and GUI together".into());
    }
    Ok(())
}
fn negotiate(profile: &Path, snapshot: &Snapshot, executable_sha256: &str) -> Result<Selection, Error> {
    // Keep the legacy trust gate independent of negotiation. In particular, a
    // compatible major/minor does not admit a different or untrusted binary.
    validate_snapshot(snapshot, profile, executable_sha256)?;
    let offer = service_contract::current_offer();
    let response = exchange_response(profile, &Request::Negotiate {
        generation: snapshot.generation.clone(), executable_sha256: executable_sha256.into(), offer: offer.clone(),
    })?;
    validate_negotiation(response, snapshot, &offer)
}
fn validate_negotiation(response: Response, snapshot: &Snapshot, offer: &Offer) -> Result<Selection, Error> {
    let Response::Negotiated { generation, executable_sha256, selection } = response else {
        return Err("Background service did not negotiate the local API".into());
    };
    if generation != snapshot.generation || executable_sha256 != snapshot.executable_sha256 {
        return Err("Background generation or executable changed during negotiation".into());
    }
    // The existing trust policy admits only this exact executable, whose offer
    // is known. Reject fabricated, extra or missing negotiated capabilities.
    let expected = service_contract::current_offer().negotiate(offer);
    if selection != expected { return Err("Invalid service capability selection".into()); }
    selection.map_err(|error| format!("Incompatible service API: {error:?}").into())
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
    let service = Service::start(&config.mesh_dir, runtime.clone(), owner)?;
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
    tokio::select! {
        _ = service.stopped.notified() => {},
        result = tokio::signal::ctrl_c() => { result?; runtime.owner.shutdown_host().await?; },
    }
    // Successful explicit quit returns zero. Failure-only launchd/systemd policy
    // does not relaunch it. No persistent startup or role marker is modified.
    Ok(())
}

#[cfg(test)]
mod service_contract_tests {
    use super::*;

    #[test]
    fn safe_network_response_has_no_pairing_and_requires_exact_owner() {
        let response = || Response::NetworkDiagnostics(ServiceNetworkSnapshot {
            generation: "fixture-generation".into(), executable_sha256: "fixture-hash".into(),
            pid: 42, build_identity: None,
            network: crate::network_diagnostics::NetworkSnapshot::default(),
        });
        let encoded = serde_json::to_string(&response()).unwrap();
        for forbidden in ["pairing", "invite_code", "network_secret", "device_id", "group_id"] {
            assert!(!encoded.contains(forbidden));
        }
        let decoded = serde_json::from_str(&encoded).unwrap();
        assert!(validate_network_response(decoded, "fixture-generation", "fixture-hash").is_ok());
        assert!(validate_network_response(response(), "other-generation", "fixture-hash").is_err());
        assert!(validate_network_response(response(), "fixture-generation", "other-hash").is_err());
        assert!(validate_network_response(Response::Snapshot(snapshot()), "fixture-generation", "fixture-hash").is_err());
    }

    #[test]
    fn full_exit_rejects_normal_restart_and_independent_activation_policies() {
        let good=serde_json::json!({"Label":"fun.hackerlife.weavelo.desktop.host","ProgramArguments":["/fixture/weavelo","--background-service"],"RunAtLoad":true,"KeepAlive":{"SuccessfulExit":false}});
        stop_policy::validate_launchd(&good,"/fixture/weavelo").unwrap();
        for (key,value) in [("KeepAlive",serde_json::json!(true)),("StartInterval",serde_json::json!(30)),
            ("MachServices",serde_json::json!({"fixture":true})),("ProgramArguments",serde_json::json!(["/wrong","--background-service"]))] {
            let mut bad=good.clone();bad[key]=value;assert!(stop_policy::validate_launchd(&bad,"/fixture/weavelo").is_err());
        }
        let good="MainPID=42\nRestart=on-failure\nActiveState=active\nSubState=running\nJob=\nTriggeredBy=\nRestartForceExitStatus=\nExecStopPost=\nOnSuccess=\n";
        stop_policy::validate_systemd(good,42).unwrap();
        for bad in [good.replace("Restart=on-failure","Restart=always"),good.replace("TriggeredBy=","TriggeredBy=other.socket"),
            good.replace("MainPID=42","MainPID=43"),good.replace("RestartForceExitStatus=","RestartForceExitStatus=0"),good.replace("OnSuccess=","OnSuccess=other.service"),good.replace("Job=","Job=45"),good.replace("SubState=running","SubState=stop-sigterm")] {
            assert!(stop_policy::validate_systemd(&bad,42).is_err());
        }
    }

    #[test]
    fn shutdown_requires_exact_background_owner_and_never_accepts_a_replacement() {
        let closing = Mutex::new(false);
        for (mode, generation, hash) in [(OwnerMode::Standalone,"g","h"),
            (OwnerMode::Background,"old","h"),(OwnerMode::Background,"g","wrong")] {
            assert!(admit_shutdown(mode,generation,hash,"g","h",&closing).is_err());
            assert!(!*closing.lock().unwrap());
        }
        admit_shutdown(OwnerMode::Background,"g","h","g","h",&closing).unwrap();
        assert!(admit_shutdown(OwnerMode::Background,"g","h","g","h",&closing).is_err());
        for (generation, hash, pid, clean) in [("old","h",42,true),("g","bad",42,true),
            ("g","h",43,true),("g","h",42,false)] {
            assert!(validate_stop_receipt(Response::StopPrepared { generation:generation.into(),
                executable_sha256:hash.into(),pid,cleanup_verified:clean },"g","h",42).is_err());
        }
        validate_stop_receipt(Response::StopPrepared {generation:"g".into(),executable_sha256:"h".into(),
            pid:42,cleanup_verified:true},"g","h",42).unwrap();
        assert!(validate_stop_receipt(Response::Rejected("cleanup timed out".into()),"g","h",42).is_err());
        let request=Request::Shutdown{generation:"g".into(),executable_sha256:"h".into()};
        let bytes=frame(&serde_json::to_vec(&request).unwrap()).unwrap();
        assert!(matches!(serde_json::from_slice::<Request>(&bytes[4..]).unwrap(),Request::Shutdown{..}));
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_process_observer_requires_actual_owned_child_exit() {
        use exit_observer::ExitObserver;
        let mut child=std::process::Command::new("/bin/sh").args(["-c","read ignored; exit 0"])
            .stdin(std::process::Stdio::piped()).spawn().unwrap();
        let still_running=ExitObserver::open(child.id()).unwrap().wait(Duration::from_millis(10));
        assert!(still_running.is_err());assert!(child.try_wait().unwrap().is_none());
        let observer=ExitObserver::open(child.id()).unwrap();drop(child.stdin.take());
        observer.wait(Duration::from_secs(3)).unwrap();assert!(child.wait().unwrap().success());
        assert!(ExitObserver::open(0).is_err());assert!(ExitObserver::open(std::process::id()).is_err());
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            schema: SCHEMA,
            owner_mode: OwnerMode::Background,
            version: env!("CARGO_PKG_VERSION").into(),
            generation: "service-1".into(),
            executable_sha256: "fixture-trusted-hash".into(),
            pid: 123,
            peers: DiscoveryPeerSnapshot::default(),
            pairing: MeshPairingSnapshot {
                network_name: "fixture".into(),
                device_id: "fixture-device".into(),
                display_name: "fixture".into(),
                invite_code: "fixture-invite".into(),
                config_dir: PathBuf::from("/fixture"),
                message: String::new(),
                message_kind: MeshPairingMessageKind::Neutral,
                restart_required: false,
            },
        }
    }

    // Frozen pre-negotiation DTO: adding even an optional field to the legacy
    // Snapshot is a breaking change for already installed strict readers.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct LegacySnapshot {
        schema: u32,
        owner_mode: OwnerMode,
        version: String,
        generation: String,
        executable_sha256: String,
        pid: u32,
        peers: DiscoveryPeerSnapshot,
        pairing: MeshPairingSnapshot,
    }
    #[derive(Deserialize)]
    enum LegacyResponse {
        Snapshot(LegacySnapshot),
    }

    #[test]
    fn legacy_snapshot_and_requests_keep_their_exact_wire_shape() {
        let encoded = serde_json::to_vec(&Response::Snapshot(snapshot())).unwrap();
        let LegacyResponse::Snapshot(legacy) = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(legacy.schema, 2);
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        let keys: std::collections::BTreeSet<_> = value["Snapshot"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "schema",
                "owner_mode",
                "version",
                "generation",
                "executable_sha256",
                "pid",
                "peers",
                "pairing"
            ]
            .into_iter()
            .collect()
        );
        for (request, legacy_json) in [
            (Request::Snapshot, r#""Snapshot""#),
            (Request::CreateGroup, r#""CreateGroup""#),
            (
                Request::JoinGroup("fixture-invite".into()),
                r#"{"JoinGroup":"fixture-invite"}"#,
            ),
            (Request::Reload("fixture".into()), r#"{"Reload":"fixture"}"#),
            (
                Request::InspectConnection { connection_id: 42 },
                r#"{"InspectConnection":{"connection_id":42}}"#,
            ),
        ] {
            assert_eq!(serde_json::to_string(&request).unwrap(), legacy_json);
            assert!(serde_json::from_str::<Request>(legacy_json).is_ok());
        }
    }

    #[test]
    fn negotiation_request_dispatch_and_response_use_the_real_framed_json_contract() {
        let snapshot = snapshot();
        let mut offer = service_contract::current_offer();
        offer.version.minor = 8;
        offer.capabilities.push("future-optional.v1".into());
        let request = Request::Negotiate {
            generation: snapshot.generation.clone(),
            executable_sha256: snapshot.executable_sha256.clone(),
            offer: offer.clone(),
        };
        let bytes = frame(&serde_json::to_vec(&request).unwrap()).unwrap();
        assert_eq!(
            valid_length(bytes[..4].try_into().unwrap()).unwrap(),
            bytes.len() - 4
        );
        let Request::Negotiate {
            generation,
            executable_sha256,
            offer: decoded,
        } = serde_json::from_slice(&bytes[4..]).unwrap()
        else {
            panic!("wrong request");
        };
        let response = answer_negotiation(
            generation,
            executable_sha256,
            decoded,
            snapshot.generation.clone(),
            snapshot.executable_sha256.clone(),
        );
        let encoded = frame(&serde_json::to_vec(&response).unwrap()).unwrap();
        let response = serde_json::from_slice(&encoded[4..]).unwrap();
        let selected = validate_negotiation(response, &snapshot, &offer).unwrap();
        assert_eq!(selected.version, service_contract::VERSION);
        assert!(
            !selected
                .capabilities
                .iter()
                .any(|c| c == "future-optional.v1")
        );
        assert!(serde_json::from_str::<Request>(r#"{"Negotiate":{"generation":"service-1","executable_sha256":"fixture","offer":{"version":{"major":1,"minor":0},"capabilities":[]},"trusted":true}}"#).is_err());
    }

    #[test]
    fn compatible_protocol_cannot_relax_existing_snapshot_trust_checks() {
        let profile = Path::new("/fixture");
        let hash = "fixture-trusted-hash";
        assert!(validate_snapshot(&snapshot(), profile, hash).is_ok());
        let mut wrong_hash = snapshot();
        wrong_hash.executable_sha256 = "different".into();
        let mut wrong_profile = snapshot();
        wrong_profile.pairing.config_dir = PathBuf::from("/other");
        let mut wrong_version = snapshot();
        wrong_version.version = "different".into();
        let mut wrong_schema = snapshot();
        wrong_schema.schema += 1;
        for changed in [wrong_hash, wrong_profile, wrong_version, wrong_schema] {
            assert!(validate_snapshot(&changed, profile, hash).is_err());
        }
        assert!(validate_snapshot(&snapshot(), profile, "different").is_err());
    }

    #[test]
    fn stale_generation_and_wrong_hash_reject_before_compatibility_is_considered() {
        let current = snapshot();
        let offer = service_contract::current_offer();
        for (generation, hash) in [
            ("service-old", "fixture-trusted-hash"),
            ("service-1", "wrong-hash"),
        ] {
            let response = answer_negotiation(
                generation.into(),
                hash.into(),
                offer.clone(),
                current.generation.clone(),
                current.executable_sha256.clone(),
            );
            assert!(matches!(response, Response::Rejected(_)));
            let response = Response::Negotiated {
                generation: generation.into(),
                executable_sha256: hash.into(),
                selection: offer.negotiate(&offer),
            };
            assert!(validate_negotiation(response, &current, &offer).is_err());
        }
    }

    #[test]
    fn incompatible_or_fabricated_selection_never_becomes_a_client_contract() {
        let current = snapshot();
        let mut incompatible = service_contract::current_offer();
        incompatible.version.major += 1;
        let response = answer_negotiation(
            current.generation.clone(),
            current.executable_sha256.clone(),
            incompatible.clone(),
            current.generation.clone(),
            current.executable_sha256.clone(),
        );
        assert!(matches!(
            &response,
            Response::Negotiated {
                selection: Err(service_contract::Rejection::MajorMismatch),
                ..
            }
        ));
        assert!(validate_negotiation(response, &current, &incompatible).is_err());
        let offer = service_contract::current_offer();
        let mut invented = offer.negotiate(&offer).unwrap();
        invented.capabilities.push("exit-all.v1".into());
        let response = Response::Negotiated {
            generation: current.generation.clone(),
            executable_sha256: current.executable_sha256.clone(),
            selection: Ok(invented),
        };
        assert!(validate_negotiation(response, &current, &offer).is_err());
        assert!(validate_negotiation(Response::Snapshot(snapshot()), &current, &offer).is_err());
    }

    #[test]
    fn slow_old_transition_cannot_overtake_or_invalidate_new_owner() {
        for old_fails in [false, true] {
            let cached = snapshot();
            let offer = service_contract::current_offer();
            let client = Client {
                profile: cached.pairing.config_dir.clone(),
                executable_sha256: cached.executable_sha256.clone(),
                cached: Arc::new(Mutex::new(cached)),
                contract: Arc::new(Mutex::new(Some(offer.negotiate(&offer).unwrap()))),
                transition: Arc::new(Mutex::new(())),
                network: Arc::new(Mutex::new(None)),
            };
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let old = client.clone();
            let first = std::thread::spawn(move || {
                old.transition(|| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    if old_fails {
                        return Err("old owner unavailable".into());
                    }
                    let offer = service_contract::current_offer();
                    Ok((snapshot(), offer.negotiate(&offer).unwrap()))
                })
                .is_ok()
            });
            entered_rx.recv().unwrap();
            let (new_tx, new_rx) = std::sync::mpsc::channel();
            let new = client.clone();
            let second = std::thread::spawn(move || {
                new.transition(|| {
                    new_tx.send(()).unwrap();
                    let mut current = snapshot();
                    current.generation = "service-2".into();
                    let offer = service_contract::current_offer();
                    Ok((current, offer.negotiate(&offer).unwrap()))
                })
                .is_ok()
            });
            assert!(new_rx.recv_timeout(Duration::from_millis(30)).is_err());
            // Status reads remain available while the serialized operation waits.
            assert_eq!(client.status()["service_contract"]["version"]["major"], 1);
            release_tx.send(()).unwrap();
            assert_eq!(first.join().unwrap(), !old_fails);
            assert!(second.join().unwrap());
            assert_eq!(client.cached.lock().unwrap().generation, "service-2");
            assert!(client.contract.lock().unwrap().is_some());
        }
    }

    #[test]
    fn outage_discards_cached_capabilities_until_new_owner_is_negotiated() {
        let cached = snapshot();
        let offer = service_contract::current_offer();
        let client = Client {
            profile: cached.pairing.config_dir.clone(),
            executable_sha256: cached.executable_sha256.clone(),
            cached: Arc::new(Mutex::new(cached)),
            contract: Arc::new(Mutex::new(Some(offer.negotiate(&offer).unwrap()))),
            transition: Arc::new(Mutex::new(())),
            network: Arc::new(Mutex::new(None)),
        };
        client.unavailable();
        assert!(client.contract.lock().unwrap().is_none());
        assert_eq!(client.cached.lock().unwrap().generation, "unavailable");
        assert!(client.status()["service_contract"].is_null());
    }
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
        assert_eq!(client.status()["service_contract"]["version"]["major"], 1);
        assert!(client.contract.lock().unwrap().is_some());
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
