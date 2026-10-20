//! Safe desktop network DTOs. Never serialize pairing, raw group IDs or packets.
use remote_core::{
    discovery::DiscoveryPeerSnapshot,
    discovery_status::{DiscoveryDiagnostics, DiscoverySnapshot, DiscoveryStamp},
    relay_status::{RelayTunnelDiagnostics, RelayTunnelSnapshot},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, atomic::AtomicU64},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Projection {
    pub observation: Option<DiscoveryStamp>,
    pub routes: Option<usize>,
    pub unique_devices: Option<usize>,
    pub online_devices: Option<usize>,
}

impl Projection {
    pub fn from_snapshot(snapshot: &DiscoveryPeerSnapshot) -> Self {
        let Some(observation) = snapshot.observation() else {
            return Self::default();
        };
        let unique = snapshot
            .peers()
            .iter()
            .map(|p| &p.announcement.device_id)
            .collect::<BTreeSet<_>>()
            .len();
        Self {
            observation: Some(observation),
            routes: Some(snapshot.len()),
            unique_devices: Some(unique),
            online_devices: Some(unique),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkFailure {
    P2pStart,
    RelayStart,
    Reload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayGroups {
    pub runtime_generation: u64,
    pub control_sha256: String,
    pub discovery_sha256: String,
}

impl RelayGroups {
    pub fn new(generation: u64, control: &str, discovery: &str) -> Self {
        // Group IDs are derived capabilities, so expose only their digests.
        // These are observations of the configs handed to the actual tunnels.
        Self {
            runtime_generation: generation,
            control_sha256: format!("{:x}", Sha256::digest(control.as_bytes())),
            discovery_sha256: format!("{:x}", Sha256::digest(discovery.as_bytes())),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSnapshot {
    pub runtime_generation: Option<u64>,
    pub reload_requested: Option<u64>,
    pub reload_applied: Option<u64>,
    pub last_failure: Option<NetworkFailure>,
    pub relay_groups: Option<RelayGroups>,
    pub relay_control: Option<RelayTunnelSnapshot>,
    pub relay_discovery: Option<RelayTunnelSnapshot>,
    pub discovery: Option<DiscoverySnapshot>,
    pub owner_snapshot: Projection,
    pub owner_projection: Projection,
    pub ipc_snapshot: Option<Projection>,
    pub gui_projection: Option<Projection>,
}

#[derive(Default)]
struct State {
    generation: u64,
    reload_requested: u64,
    reload_applied: u64,
    last_failure: Option<NetworkFailure>,
    groups: Option<RelayGroups>,
    control: Option<Arc<RelayTunnelDiagnostics>>,
    relay_discovery: Option<Arc<RelayTunnelDiagnostics>>,
    discovery: Option<Arc<DiscoveryDiagnostics>>,
}

pub(crate) struct NetworkDiagnostics {
    state: Mutex<State>,
    sequence: Arc<AtomicU64>,
}

impl Default for NetworkDiagnostics {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                generation: 1,
                ..State::default()
            }),
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl NetworkDiagnostics {
    pub fn generation(&self) -> u64 {
        self.state.lock().unwrap().generation
    }
    pub fn begin_reload(&self) {
        let mut state = self.state.lock().unwrap();
        state.generation += 1;
        state.reload_requested += 1;
        state.last_failure = None;
        state.discovery = None;
    }
    pub fn reload_finished(&self) {
        let mut state = self.state.lock().unwrap();
        if state.last_failure.is_none() {
            state.reload_applied = state.reload_requested;
        }
    }
    pub fn failure(&self, failure: NetworkFailure) {
        self.state.lock().unwrap().last_failure = Some(failure);
    }
    pub fn discovery(&self) -> Arc<DiscoveryDiagnostics> {
        let mut state = self.state.lock().unwrap();
        let diagnostics = Arc::new(DiscoveryDiagnostics::new(
            state.generation,
            self.sequence.clone(),
        ));
        state.discovery = Some(diagnostics.clone());
        diagnostics
    }
    pub fn clear_relays(&self) {
        let mut state = self.state.lock().unwrap();
        state.groups = None;
        state.control = None;
        state.relay_discovery = None;
    }
    pub fn attach_relays(
        &self,
        groups: RelayGroups,
        control: Option<Arc<RelayTunnelDiagnostics>>,
        discovery: Option<Arc<RelayTunnelDiagnostics>>,
    ) {
        let mut state = self.state.lock().unwrap();
        state.groups = Some(groups);
        state.control = control;
        state.relay_discovery = discovery;
    }
    pub fn snapshot(&self) -> NetworkSnapshot {
        let state = self.state.lock().unwrap();
        NetworkSnapshot {
            runtime_generation: Some(state.generation),
            reload_requested: Some(state.reload_requested),
            reload_applied: Some(state.reload_applied),
            last_failure: state.last_failure,
            relay_groups: state.groups.clone(),
            relay_control: state.control.as_ref().map(|d| d.snapshot()),
            relay_discovery: state.relay_discovery.as_ref().map(|d| d.snapshot()),
            discovery: state.discovery.as_ref().map(|d| d.snapshot()),
            ..NetworkSnapshot::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_is_null_and_group_hashes_follow_derived_secret_and_channel() {
        let state = NetworkDiagnostics::default();
        let json = serde_json::to_value(state.snapshot()).unwrap();
        assert!(json["relay_control"].is_null());
        assert!(json["discovery"].is_null());
        assert!(json["owner_snapshot"]["routes"].is_null());
        let derive = remote_core::relay::derive_relay_group_id;
        let control = derive(
            "synthetic-network",
            "synthetic-secret-a",
            "control-device-v1",
        )
        .unwrap();
        let discovery = derive("synthetic-network", "synthetic-secret-a", "discovery").unwrap();
        let other = derive(
            "synthetic-network",
            "synthetic-secret-b",
            "control-device-v1",
        )
        .unwrap();
        let groups = RelayGroups::new(1, &control, &discovery);
        assert_ne!(groups.control_sha256, groups.discovery_sha256);
        assert_ne!(
            groups.control_sha256,
            RelayGroups::new(1, &other, &discovery).control_sha256
        );
        state.attach_relays(groups, None, None);
        let serialized = serde_json::to_string(&state.snapshot()).unwrap();
        for forbidden in [
            &control,
            &discovery,
            "synthetic-secret-a",
            "synthetic-network",
            "invite_code",
            "pairing",
        ] {
            assert!(!serialized.contains(forbidden));
        }
    }

    #[test]
    fn snapshot_stamp_survives_ipc_and_increases_after_reload() {
        let state = NetworkDiagnostics::default();
        let first = DiscoveryPeerSnapshot::default().observed(&state.discovery());
        let bytes = serde_json::to_vec(&first).unwrap();
        let roundtrip: DiscoveryPeerSnapshot = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(first.observation(), roundtrip.observation());
        state.begin_reload();
        let next = DiscoveryPeerSnapshot::default().observed(&state.discovery());
        assert!(next.observation().unwrap().sequence > first.observation().unwrap().sequence);
        assert_eq!(next.observation().unwrap().runtime_generation, 2);
        state.reload_finished();
        assert_eq!(state.snapshot().reload_applied, Some(1));
    }

    #[tokio::test]
    async fn frontend_projection_keeps_the_ipc_sequence_and_offline_counts() {
        use remote_core::discovery::{
            DiscoveryAnnouncement, DiscoveryCapabilities, DiscoveryPeerCache,
            DiscoveryRouteOverride, DiscoveryScope,
        };
        use std::time::Duration;
        let state = NetworkDiagnostics::default();
        let diagnostics = state.discovery();
        let mut cache = DiscoveryPeerCache::new("synthetic-network", "local");
        let peer = DiscoveryAnnouncement {
            network_name: "synthetic-network".into(),
            device_id: "synthetic-peer".into(),
            display_name: "Synthetic peer".into(),
            control_port: 9000,
            virtual_ip: None,
            capabilities: DiscoveryCapabilities::all_interactive(),
            scope: DiscoveryScope::Lan,
            ttl: Duration::from_secs(8),
        };
        let source = "127.0.0.1:38117".parse().unwrap();
        cache.apply_announcement(peer.clone(), source, 0);
        cache.apply_announcement_with_route_override(
            peer,
            source,
            0,
            DiscoveryRouteOverride {
                source,
                endpoint: "127.0.0.1:9001".parse().unwrap(),
                scope: DiscoveryScope::Relay,
            },
        );
        let initial = cache.snapshot().observed(&diagnostics);
        let (tx, rx) = tokio::sync::watch::channel(initial.clone());
        let mut owner = crate::UnifiedServiceOwner::from_background_snapshot(rx)
            .await
            .unwrap();
        owner.frontend_network = Some(Arc::new(Mutex::new(Some(state.snapshot()))));
        let wait = |expected| {
            let owner = &owner;
            async move {
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if owner
                            .network_snapshot()
                            .gui_projection
                            .as_ref()
                            .unwrap()
                            .observation
                            == expected
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
            }
        };
        wait(initial.observation()).await;
        let snapshot = owner.network_snapshot();
        let ipc = snapshot.ipc_snapshot.unwrap();
        let gui = snapshot.gui_projection.unwrap();
        assert_eq!(ipc.observation, gui.observation);
        assert_eq!(gui.routes, Some(2));
        assert_eq!(gui.unique_devices, Some(1));
        assert_eq!(gui.online_devices, Some(1));
        let empty = DiscoveryPeerSnapshot::default().observed(&diagnostics);
        tx.send(empty.clone()).unwrap();
        wait(empty.observation()).await;
        let gui = owner.network_snapshot().gui_projection.unwrap();
        assert_eq!(gui.routes, Some(0));
        assert_eq!(gui.unique_devices, Some(1));
        assert_eq!(gui.online_devices, Some(0));
        tx.send(DiscoveryPeerSnapshot::default()).unwrap();
        wait(None).await;
        assert!(
            owner
                .network_snapshot()
                .gui_projection
                .unwrap()
                .online_devices
                .is_none()
        );
    }
}
