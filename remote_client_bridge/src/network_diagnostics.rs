//! Public local diagnostics; never hold profile secrets, invitations or routing capabilities.
use remote_core::mesh::{MeshConfig, PublicMeshIdentity};
use remote_core::relay_status::{RelayTunnelDiagnostics, RelayTunnelSnapshot};
use serde::Serialize;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NetworkPhase {
    #[default]
    Starting,
    Discovering,
    Paused,
    Retrying,
    Stopped,
}
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum P2pPhase {
    #[default]
    Idle,
    Resolving,
    Running,
    RetryWait,
    Stopped,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NetworkFailure {
    ProfileUnavailable,
    SupervisorUnavailable,
    ComponentExited,
    ComponentPanicked,
    DiscoveryStopped,
    StartupTimeout,
    P2pUnavailable,
    RelayBindUnavailable,
}

#[derive(Clone, Default)]
struct Progress {
    phase: NetworkPhase,
    runtime_generation: u64,
    reload_requested: u64,
    reload_applied: u64,
    identity: Option<PublicMeshIdentity>,
    discovery_running: bool,
    p2p_state: P2pPhase,
    p2p_error: Option<NetworkFailure>,
    discovered_routes: usize,
    peer_count: usize,
    peer_device_id_sha256: Vec<String>,
    discovery_errors: u64,
    last_failure: Option<NetworkFailure>,
}
#[derive(Default)]
struct State {
    progress: Progress,
    control: Arc<RelayTunnelDiagnostics>,
    discovery: Arc<RelayTunnelDiagnostics>,
}
#[derive(Default)]
pub(crate) struct NetworkDiagnostics {
    state: Mutex<State>,
}
#[derive(Serialize)]
pub(crate) struct NetworkSnapshot {
    #[serde(flatten)]
    progress: PublicProgress,
    relay_control: RelayTunnelSnapshot,
    relay_discovery: RelayTunnelSnapshot,
}
#[derive(Serialize)]
struct PublicProgress {
    phase: NetworkPhase,
    runtime_generation: u64,
    reload_requested: u64,
    reload_applied: u64,
    identity: Option<PublicMeshIdentity>,
    discovery_running: bool,
    p2p_state: P2pPhase,
    p2p_error: Option<NetworkFailure>,
    discovered_routes: usize,
    peer_count: usize,
    peer_device_id_sha256: Vec<String>,
    discovery_errors: u64,
    last_failure: Option<NetworkFailure>,
}
impl NetworkDiagnostics {
    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()));
    }
    pub(crate) fn request_reload(&self) {
        self.update(|s| {
            s.progress.reload_requested = s.progress.reload_requested.saturating_add(1)
        });
    }
    pub(crate) fn requested(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .progress
            .reload_requested
    }
    pub(crate) fn begin(&self, identity: &MeshConfig, requested: u64) {
        self.update(|s| {
            let generation = s.progress.runtime_generation.saturating_add(1);
            let requests = s.progress.reload_requested;
            s.progress = Progress {
                runtime_generation: generation,
                reload_requested: requests,
                reload_applied: requested,
                identity: Some(identity.public_identity()),
                ..Progress::default()
            };
            s.control = Arc::default();
            s.discovery = Arc::default();
        });
    }
    pub(crate) fn attach_relays(
        &self,
        control: Arc<RelayTunnelDiagnostics>,
        discovery: Arc<RelayTunnelDiagnostics>,
    ) {
        self.update(|s| {
            s.control = control;
            s.discovery = discovery;
        });
    }
    pub(crate) fn discovering(&self) {
        self.update(|s| {
            s.progress.phase = NetworkPhase::Discovering;
            s.progress.discovery_running = true;
        });
    }
    pub(crate) fn peers(&self, routes: usize, hashes: Vec<String>, count: usize) {
        self.update(|s| {
            s.progress.discovered_routes = routes;
            s.progress.peer_count = count;
            s.progress.peer_device_id_sha256 = hashes;
        });
    }
    pub(crate) fn discovery_error(&self) {
        self.update(|s| {
            s.progress.discovery_errors = s.progress.discovery_errors.saturating_add(1)
        });
    }
    pub(crate) fn paused(&self) {
        self.update(|s| {
            s.progress.phase = NetworkPhase::Paused;
            s.progress.discovery_running = false;
            s.progress.peer_count = 0;
            s.progress.discovered_routes = 0;
            s.progress.peer_device_id_sha256.clear();
            s.progress.p2p_state = P2pPhase::Stopped;
        });
    }
    pub(crate) fn failed(&self, failure: NetworkFailure) {
        self.update(|s| {
            s.progress.phase = NetworkPhase::Retrying;
            s.progress.last_failure.get_or_insert(failure);
            s.progress.discovery_running = false;
            s.progress.peer_count = 0;
            s.progress.discovered_routes = 0;
            s.progress.peer_device_id_sha256.clear();
        });
    }
    pub(crate) fn stopped(&self) {
        self.update(|s| {
            s.progress.phase = NetworkPhase::Stopped;
            s.progress.discovery_running = false;
        });
    }
    pub(crate) fn p2p(&self, phase: P2pPhase, error: Option<NetworkFailure>) {
        self.update(|s| {
            s.progress.p2p_state = phase;
            s.progress.p2p_error = error;
        });
    }
    pub(crate) fn snapshot(&self) -> NetworkSnapshot {
        let (p, control, discovery) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.progress.clone(), s.control.clone(), s.discovery.clone())
        };
        NetworkSnapshot {
            progress: PublicProgress {
                phase: p.phase,
                runtime_generation: p.runtime_generation,
                reload_requested: p.reload_requested,
                reload_applied: p.reload_applied,
                identity: p.identity,
                discovery_running: p.discovery_running,
                p2p_state: p.p2p_state,
                p2p_error: p.p2p_error,
                discovered_routes: p.discovered_routes,
                peer_count: p.peer_count,
                peer_device_id_sha256: p.peer_device_id_sha256,
                discovery_errors: p.discovery_errors,
                last_failure: p.last_failure,
            },
            relay_control: control.snapshot(),
            relay_discovery: discovery.snapshot(),
        }
    }
}

pub(crate) enum Startup<T, E> {
    Complete(Result<T, E>),
    Cancelled,
    TimedOut,
}
pub(crate) async fn cancellable_startup<T, E>(
    future: impl Future<Output = Result<T, E>>,
    cancel: &mut broadcast::Receiver<()>,
    deadline: Duration,
) -> Startup<T, E> {
    tokio::select! { biased;
        _ = cancel.recv() => Startup::Cancelled,
        result = tokio::time::timeout(deadline, future) => match result { Ok(result) => Startup::Complete(result), Err(_) => Startup::TimedOut },
    }
}
#[derive(Default)]
pub(crate) struct NetworkTasks {
    tasks: Vec<JoinHandle<()>>,
}
impl NetworkTasks {
    pub(crate) fn push(&mut self, task: JoinHandle<()>) {
        self.tasks.push(task);
    }
    pub(crate) async fn finished_failure(&mut self) -> Option<NetworkFailure> {
        let index = self.tasks.iter().position(JoinHandle::is_finished)?;
        let task = self.tasks.swap_remove(index);
        Some(match task.await {
            Err(error) if error.is_panic() => NetworkFailure::ComponentPanicked,
            _ => NetworkFailure::ComponentExited,
        })
    }
    pub(crate) async fn stop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
    }
}
impl Drop for NetworkTasks {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_export_only_hashes_and_safe_states() {
        let mut config = MeshConfig::generate("Synthetic device");
        config.network_name = "private-test-group-name".into();
        config.network_secret =
            remote_core::mesh::MeshSecret::new("credential-canary-never-serialized").unwrap();
        let diagnostics = NetworkDiagnostics::default();
        diagnostics.begin(&config, 0);
        let json = serde_json::to_string(&diagnostics.snapshot()).unwrap();
        let invitation = config.invite_code();
        for forbidden in [
            config.network_name.as_str(),
            config.network_secret.expose_secret(),
            config.node_id.as_str(),
            invitation.as_str(),
        ] {
            assert!(!json.contains(forbidden));
        }
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value["identity"]["network_name_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert_eq!(value["relay_control"]["registrations_sent"], 0);
        assert!(value["relay_control"]["last_pong_age_ms"].is_null());
    }
    #[test]
    fn reload_ack_is_bound_to_the_loaded_identity_generation() {
        let a = MeshConfig::generate("A");
        let b = MeshConfig::generate("B");
        let diagnostics = NetworkDiagnostics::default();
        diagnostics.request_reload();
        diagnostics.begin(&a, 0);
        let pending = diagnostics.snapshot();
        assert_eq!(pending.progress.reload_requested, 1);
        assert_eq!(pending.progress.reload_applied, 0);
        diagnostics.begin(&b, 1);
        let applied = diagnostics.snapshot();
        assert_eq!(
            applied.progress.reload_requested,
            applied.progress.reload_applied
        );
        assert_eq!(applied.progress.identity.unwrap(), b.public_identity());
    }
    #[tokio::test]
    async fn startup_cancel_is_not_lost_before_first_poll() {
        let (cancel, mut rx) = broadcast::channel(2);
        cancel.send(()).unwrap();
        let result = cancellable_startup(
            std::future::pending::<Result<(), ()>>(),
            &mut rx,
            Duration::from_secs(1),
        )
        .await;
        assert!(matches!(result, Startup::Cancelled));
    }
    #[tokio::test]
    async fn pending_dns_style_startup_has_a_bounded_timeout() {
        let (_cancel, mut rx) = broadcast::channel(2);
        let result = cancellable_startup(
            std::future::pending::<Result<(), ()>>(),
            &mut rx,
            Duration::from_millis(10),
        )
        .await;
        assert!(matches!(result, Startup::TimedOut));
    }
    #[tokio::test]
    async fn a_panicked_component_is_observed_and_owned_children_are_joined() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Finished(Arc<AtomicBool>);
        impl Drop for Finished {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let finished = Arc::new(AtomicBool::new(false));
        let child_finished = finished.clone();
        let mut tasks = NetworkTasks::default();
        tasks.push(tokio::spawn(async {
            panic!("synthetic component failure")
        }));
        tasks.push(tokio::spawn(async move {
            let _guard = Finished(child_finished);
            std::future::pending::<()>().await;
        }));
        tokio::task::yield_now().await;
        let failure = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(f) = tasks.finished_failure().await {
                    break f;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(failure, NetworkFailure::ComponentPanicked);
        tasks.stop().await;
        assert!(finished.load(Ordering::SeqCst));
        assert!(tasks.tasks.is_empty());
    }
}
