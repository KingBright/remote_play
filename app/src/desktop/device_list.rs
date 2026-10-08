//! Framework-independent device list state and projection for desktop adapters.
use crate::AppDevice;
use remote_core::{discovery::DiscoveryScope, role::RoleState};
use std::net::SocketAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DeviceFilterKind {
    #[default]
    All,
    Lan,
    P2p,
    Relay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceConnectionStatus {
    Connecting,
    Viewing,
    Serving,
    Available,
    Standby,
}

impl DeviceConnectionStatus {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting…",
            Self::Viewing => "Session connected",
            Self::Serving => "Serving",
            Self::Available => "Available",
            Self::Standby => "Standby",
        }
    }

    const fn is_active(self) -> bool {
        matches!(self, Self::Connecting | Self::Viewing | Self::Serving)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceRowViewModel {
    pub device_id: String,
    pub display_name: String,
    pub endpoint: SocketAddr,
    pub scope: DiscoveryScope,
    pub online: bool,
    pub connection: DeviceConnectionStatus,
    pub can_connect: bool,
    pub can_open_files: bool,
    pub can_open_workspace: bool,
}

impl DeviceRowViewModel {
    pub(crate) const fn is_active(&self) -> bool {
        self.connection.is_active()
    }
}

pub(crate) struct DeviceListModel<'a> {
    pub devices: &'a [AppDevice],
    pub role: &'a RoleState,
}
impl DeviceListModel<'_> {
    pub(crate) fn project(&self, filter: DeviceFilterKind) -> DeviceListViewModel {
        DeviceListViewModel::project(self.devices, self.role, filter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DeviceListViewModel {
    pub rows: Vec<DeviceRowViewModel>,
}

impl DeviceListViewModel {
    pub(crate) fn project(
        devices: &[AppDevice],
        role: &RoleState,
        filter: DeviceFilterKind,
    ) -> Self {
        let active = role.session();
        let rows = devices
            .iter()
            .filter(|device| matches_filter(filter, device.scope))
            .map(|device| {
                let connection = match (role, active) {
                    (RoleState::Connecting(_), Some(session))
                        if session.peer.device_id == device.device_id =>
                    {
                        DeviceConnectionStatus::Connecting
                    }
                    (RoleState::Viewing(_), Some(session))
                        if session.peer.device_id == device.device_id =>
                    {
                        DeviceConnectionStatus::Viewing
                    }
                    (RoleState::Serving(_), Some(session))
                        if session.peer.device_id == device.device_id =>
                    {
                        DeviceConnectionStatus::Serving
                    }
                    _ if device.online => DeviceConnectionStatus::Available,
                    _ => DeviceConnectionStatus::Standby,
                };
                let active = connection.is_active();
                DeviceRowViewModel {
                    device_id: device.device_id.clone(),
                    display_name: device.display_name.clone(),
                    endpoint: device.endpoint,
                    scope: device.scope,
                    online: device.online,
                    connection,
                    can_connect: device.is_streamable() && !active,
                    can_open_files: device.online,
                    can_open_workspace: device.online,
                }
            })
            .collect();
        Self { rows }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DeviceListState {
    pub filter: DeviceFilterKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeviceListAction {
    SelectFilter(DeviceFilterKind),
    Connect(String),
    OpenFiles(String),
    OpenWorkspace(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeviceListEffect {
    ConnectStream(String),
    ConnectFiles(String),
    OpenWorkspace(String),
}

impl DeviceListState {
    /// Reduce an adapter action to state or an effect. The adapter executes effects
    /// through the existing OriginalOwner; this module starts no tasks or runtimes.
    pub(crate) fn reduce(&mut self, action: DeviceListAction) -> Option<DeviceListEffect> {
        match action {
            DeviceListAction::SelectFilter(filter) => {
                self.filter = filter;
                None
            }
            DeviceListAction::Connect(device_id) => {
                Some(DeviceListEffect::ConnectStream(device_id))
            }
            DeviceListAction::OpenFiles(device_id) => {
                Some(DeviceListEffect::ConnectFiles(device_id))
            }
            DeviceListAction::OpenWorkspace(device_id) => {
                Some(DeviceListEffect::OpenWorkspace(device_id))
            }
        }
    }
}

fn matches_filter(filter: DeviceFilterKind, scope: DiscoveryScope) -> bool {
    match filter {
        DeviceFilterKind::All => true,
        DeviceFilterKind::Lan => scope == DiscoveryScope::Lan,
        DeviceFilterKind::P2p => scope == DiscoveryScope::P2p,
        DeviceFilterKind::Relay => scope == DiscoveryScope::Relay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remote_core::{discovery::DiscoveryScope, role::RoleSession};

    fn device(id: &str, scope: DiscoveryScope, online: bool) -> AppDevice {
        AppDevice {
            device_id: id.into(),
            display_name: format!("Device {id}"),
            endpoint: "127.0.0.1:9000".parse().unwrap(),
            scope,
            can_stream: true,
            can_view: true,
            online,
            last_seen_ms: 0,
        }
    }

    #[test]
    fn projection_keeps_device_and_connection_status_separate_from_the_adapter() {
        let devices = [
            device("a", DiscoveryScope::Lan, true),
            device("b", DiscoveryScope::Relay, true),
            device("c", DiscoveryScope::Lan, false),
        ];
        let role = RoleState::Connecting(RoleSession::new(devices[0].role_peer(), 7, 0));

        let model = DeviceListViewModel::project(&devices, &role, DeviceFilterKind::Lan);

        assert_eq!(model.rows.len(), 2);
        assert_eq!(model.rows[0].connection, DeviceConnectionStatus::Connecting);
        assert!(!model.rows[0].can_connect);
        assert!(model.rows[0].can_open_files);
        assert_eq!(model.rows[1].connection, DeviceConnectionStatus::Standby);
        assert!(!model.rows[1].can_connect);
        assert!(!model.rows[1].can_open_files);
    }

    #[test]
    fn state_actions_return_effects_without_starting_runtime_work() {
        let mut state = DeviceListState::default();
        assert_eq!(
            state.reduce(DeviceListAction::SelectFilter(DeviceFilterKind::Relay)),
            None
        );
        assert_eq!(state.filter, DeviceFilterKind::Relay);
        assert_eq!(
            state.reduce(DeviceListAction::Connect("peer-1".into())),
            Some(DeviceListEffect::ConnectStream("peer-1".into()))
        );
        assert_eq!(
            state.reduce(DeviceListAction::OpenFiles("peer-2".into())),
            Some(DeviceListEffect::ConnectFiles("peer-2".into()))
        );
        assert_eq!(
            state.reduce(DeviceListAction::OpenWorkspace("peer-3".into())),
            Some(DeviceListEffect::OpenWorkspace("peer-3".into()))
        );
    }

    #[test]
    fn switching_device_moves_active_state_without_rebinding_row_identity() {
        let devices = [
            device("a", DiscoveryScope::Lan, true),
            device("b", DiscoveryScope::Relay, true),
        ];
        let viewing_a = RoleState::Viewing(RoleSession::new(devices[0].role_peer(), 7, 0));
        let connecting_b = RoleState::Connecting(RoleSession::new(devices[1].role_peer(), 8, 1));
        let before = DeviceListModel {
            devices: &devices,
            role: &viewing_a,
        }
        .project(DeviceFilterKind::All);
        let after = DeviceListModel {
            devices: &devices,
            role: &connecting_b,
        }
        .project(DeviceFilterKind::All);
        assert_eq!(before.rows[0].connection.label(), "Session connected");
        assert!(!before.rows[0].can_connect);
        assert!(after.rows[0].can_connect);
        assert!(!after.rows[1].can_connect);
        assert_eq!(after.rows[1].device_id, "b");
        assert_eq!(after.rows[1].endpoint, devices[1].endpoint);
        let filtered = DeviceListModel {
            devices: &devices,
            role: &connecting_b,
        }
        .project(DeviceFilterKind::Lan);
        assert_eq!(filtered.rows.len(), 1);
        assert_eq!(
            filtered.rows[0].connection,
            DeviceConnectionStatus::Available
        );
    }
}
