use std::error::Error;
use std::fmt;
use std::net::SocketAddr;

pub const DEFAULT_CONNECTING_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_SESSION_TIMEOUT_MS: u64 = 3_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolePeer {
    pub device_id: String,
    pub display_name: String,
    pub endpoint: SocketAddr,
}

impl RolePeer {
    pub fn new(
        device_id: impl Into<String>,
        display_name: impl Into<String>,
        endpoint: SocketAddr,
    ) -> Self {
        let endpoint_label = endpoint.to_string();
        Self {
            device_id: non_empty_or(device_id.into(), format!("endpoint:{endpoint_label}")),
            display_name: non_empty_or(display_name.into(), endpoint_label),
            endpoint,
        }
    }

    pub fn endpoint_only(endpoint: SocketAddr) -> Self {
        Self::new("", "", endpoint)
    }

    pub fn same_device(&self, other: &Self) -> bool {
        self.device_id == other.device_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSession {
    pub peer: RolePeer,
    pub session_id: u32,
    pub started_at_ms: u64,
    pub last_activity_ms: u64,
}

impl RoleSession {
    pub fn new(peer: RolePeer, session_id: u32, now_ms: u64) -> Self {
        Self {
            peer,
            session_id,
            started_at_ms: now_ms,
            last_activity_ms: now_ms,
        }
    }

    pub fn touch(&mut self, now_ms: u64) {
        self.last_activity_ms = now_ms;
    }

    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.started_at_ms)
    }

    pub fn idle_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.last_activity_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    Idle,
    Connecting,
    Viewing,
    Serving,
}

impl RoleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RoleKind::Idle => "idle",
            RoleKind::Connecting => "connecting",
            RoleKind::Viewing => "viewing",
            RoleKind::Serving => "serving",
        }
    }
}

impl fmt::Display for RoleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RoleState {
    #[default]
    Idle,
    Connecting(RoleSession),
    Viewing(RoleSession),
    Serving(RoleSession),
}

impl RoleState {
    pub fn kind(&self) -> RoleKind {
        match self {
            RoleState::Idle => RoleKind::Idle,
            RoleState::Connecting(_) => RoleKind::Connecting,
            RoleState::Viewing(_) => RoleKind::Viewing,
            RoleState::Serving(_) => RoleKind::Serving,
        }
    }

    pub fn is_idle(&self) -> bool {
        matches!(self, RoleState::Idle)
    }

    pub fn session(&self) -> Option<&RoleSession> {
        match self {
            RoleState::Idle => None,
            RoleState::Connecting(session)
            | RoleState::Viewing(session)
            | RoleState::Serving(session) => Some(session),
        }
    }

    pub fn session_id(&self) -> Option<u32> {
        self.session().map(|session| session.session_id)
    }

    pub fn peer(&self) -> Option<&RolePeer> {
        self.session().map(|session| &session.peer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundConflictPolicy {
    Reject,
    StopActiveAndServe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleStateMachineConfig {
    pub connecting_timeout_ms: u64,
    pub session_timeout_ms: u64,
    pub inbound_conflict_policy: InboundConflictPolicy,
}

impl Default for RoleStateMachineConfig {
    fn default() -> Self {
        Self {
            connecting_timeout_ms: DEFAULT_CONNECTING_TIMEOUT_MS,
            session_timeout_ms: DEFAULT_SESSION_TIMEOUT_MS,
            inbound_conflict_policy: InboundConflictPolicy::Reject,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleChangeReason {
    StartViewing,
    ViewingConnected,
    AcceptServing,
    Activity,
    Stop,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleChange {
    pub previous: RoleState,
    pub current: RoleState,
    pub reason: RoleChangeReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleStateError {
    Busy {
        current: RoleKind,
        current_peer_device_id: String,
        current_session_id: u32,
    },
    WrongSession {
        current: RoleKind,
        expected_session_id: u32,
        actual_session_id: u32,
    },
    NoActiveSession,
    NotConnecting {
        current: RoleKind,
    },
}

impl fmt::Display for RoleStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoleStateError::Busy {
                current,
                current_peer_device_id,
                current_session_id,
            } => write!(
                f,
                "role state is busy: {current} with peer {current_peer_device_id} session {current_session_id}"
            ),
            RoleStateError::WrongSession {
                current,
                expected_session_id,
                actual_session_id,
            } => write!(
                f,
                "wrong session for {current}: expected {expected_session_id}, got {actual_session_id}"
            ),
            RoleStateError::NoActiveSession => write!(f, "no active role session"),
            RoleStateError::NotConnecting { current } => {
                write!(f, "role state is {current}, not connecting")
            }
        }
    }
}

impl Error for RoleStateError {}

#[derive(Debug, Clone)]
pub struct RoleStateMachine {
    state: RoleState,
    config: RoleStateMachineConfig,
}

impl RoleStateMachine {
    pub fn new() -> Self {
        Self::with_config(RoleStateMachineConfig::default())
    }

    pub fn with_config(config: RoleStateMachineConfig) -> Self {
        Self {
            state: RoleState::Idle,
            config,
        }
    }

    pub fn state(&self) -> &RoleState {
        &self.state
    }

    pub fn config(&self) -> RoleStateMachineConfig {
        self.config
    }

    pub fn can_start_viewing(&self, peer: &RolePeer) -> bool {
        match &self.state {
            RoleState::Idle => true,
            RoleState::Connecting(session) | RoleState::Viewing(session) => {
                session.peer.same_device(peer)
            }
            RoleState::Serving(_) => false,
        }
    }

    pub fn can_accept_serving(&self, peer: &RolePeer) -> bool {
        match &self.state {
            RoleState::Idle => true,
            RoleState::Serving(session) => session.peer.same_device(peer),
            RoleState::Connecting(_) | RoleState::Viewing(_) => {
                self.config.inbound_conflict_policy == InboundConflictPolicy::StopActiveAndServe
            }
        }
    }

    pub fn start_viewing(
        &mut self,
        peer: RolePeer,
        session_id: u32,
        now_ms: u64,
    ) -> Result<RoleChange, RoleStateError> {
        match &self.state {
            RoleState::Idle => {
                let next = RoleState::Connecting(RoleSession::new(peer, session_id, now_ms));
                Ok(self.replace_state(next, RoleChangeReason::StartViewing))
            }
            RoleState::Connecting(session) | RoleState::Viewing(session)
                if session.peer.same_device(&peer) =>
            {
                let next = RoleState::Connecting(RoleSession::new(peer, session_id, now_ms));
                Ok(self.replace_state(next, RoleChangeReason::StartViewing))
            }
            _ => Err(self.busy_error()),
        }
    }

    pub fn mark_viewing_connected(
        &mut self,
        session_id: u32,
        now_ms: u64,
    ) -> Result<RoleChange, RoleStateError> {
        match &self.state {
            RoleState::Connecting(session) if session.session_id == session_id => {
                let mut session = session.clone();
                session.touch(now_ms);
                Ok(self.replace_state(
                    RoleState::Viewing(session),
                    RoleChangeReason::ViewingConnected,
                ))
            }
            RoleState::Viewing(session) if session.session_id == session_id => {
                self.record_activity(session_id, now_ms)
            }
            RoleState::Connecting(session) | RoleState::Viewing(session) => {
                Err(RoleStateError::WrongSession {
                    current: self.state.kind(),
                    expected_session_id: session.session_id,
                    actual_session_id: session_id,
                })
            }
            _ => Err(RoleStateError::NotConnecting {
                current: self.state.kind(),
            }),
        }
    }

    pub fn accept_serving(
        &mut self,
        peer: RolePeer,
        session_id: u32,
        now_ms: u64,
    ) -> Result<RoleChange, RoleStateError> {
        match &self.state {
            RoleState::Idle => {
                let next = RoleState::Serving(RoleSession::new(peer, session_id, now_ms));
                Ok(self.replace_state(next, RoleChangeReason::AcceptServing))
            }
            RoleState::Serving(session)
                if session.peer.same_device(&peer) && session.session_id == session_id =>
            {
                self.record_activity(session_id, now_ms)
            }
            RoleState::Serving(session) if session.peer.same_device(&peer) => {
                let next = RoleState::Serving(RoleSession::new(peer, session_id, now_ms));
                Ok(self.replace_state(next, RoleChangeReason::AcceptServing))
            }
            RoleState::Connecting(_) | RoleState::Viewing(_)
                if self.config.inbound_conflict_policy
                    == InboundConflictPolicy::StopActiveAndServe =>
            {
                let next = RoleState::Serving(RoleSession::new(peer, session_id, now_ms));
                Ok(self.replace_state(next, RoleChangeReason::AcceptServing))
            }
            _ => Err(self.busy_error()),
        }
    }

    pub fn record_activity(
        &mut self,
        session_id: u32,
        now_ms: u64,
    ) -> Result<RoleChange, RoleStateError> {
        match &self.state {
            RoleState::Idle => Err(RoleStateError::NoActiveSession),
            RoleState::Connecting(session) => {
                if session.session_id != session_id {
                    return Err(wrong_session(self.state.kind(), session, session_id));
                }
                let mut session = session.clone();
                session.touch(now_ms);
                Ok(self.replace_state(RoleState::Connecting(session), RoleChangeReason::Activity))
            }
            RoleState::Viewing(session) => {
                if session.session_id != session_id {
                    return Err(wrong_session(self.state.kind(), session, session_id));
                }
                let mut session = session.clone();
                session.touch(now_ms);
                Ok(self.replace_state(RoleState::Viewing(session), RoleChangeReason::Activity))
            }
            RoleState::Serving(session) => {
                if session.session_id != session_id {
                    return Err(wrong_session(self.state.kind(), session, session_id));
                }
                let mut session = session.clone();
                session.touch(now_ms);
                Ok(self.replace_state(RoleState::Serving(session), RoleChangeReason::Activity))
            }
        }
    }

    pub fn stop_active(&mut self) -> Option<RoleChange> {
        if self.state.is_idle() {
            return None;
        }
        Some(self.replace_state(RoleState::Idle, RoleChangeReason::Stop))
    }

    pub fn stop_session(&mut self, session_id: u32) -> Result<Option<RoleChange>, RoleStateError> {
        let Some(session) = self.state.session() else {
            return Ok(None);
        };
        if session.session_id != session_id {
            return Err(wrong_session(self.state.kind(), session, session_id));
        }
        Ok(self.stop_active())
    }

    pub fn expire_timed_out(&mut self, now_ms: u64) -> Option<RoleChange> {
        let timed_out = match &self.state {
            RoleState::Idle => false,
            RoleState::Connecting(session) => {
                session.age_ms(now_ms) > self.config.connecting_timeout_ms
            }
            RoleState::Viewing(session) | RoleState::Serving(session) => {
                session.idle_ms(now_ms) > self.config.session_timeout_ms
            }
        };

        timed_out.then(|| self.replace_state(RoleState::Idle, RoleChangeReason::Timeout))
    }

    fn replace_state(&mut self, next: RoleState, reason: RoleChangeReason) -> RoleChange {
        let previous = std::mem::replace(&mut self.state, next);
        RoleChange {
            previous,
            current: self.state.clone(),
            reason,
        }
    }

    fn busy_error(&self) -> RoleStateError {
        let Some(session) = self.state.session() else {
            return RoleStateError::NoActiveSession;
        };
        RoleStateError::Busy {
            current: self.state.kind(),
            current_peer_device_id: session.peer.device_id.clone(),
            current_session_id: session.session_id,
        }
    }
}

impl Default for RoleStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

fn wrong_session(
    current: RoleKind,
    session: &RoleSession,
    actual_session_id: u32,
) -> RoleStateError {
    RoleStateError::WrongSession {
        current,
        expected_session_id: session.session_id,
        actual_session_id,
    }
}

fn non_empty_or(value: String, fallback: String) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::DEFAULT_CONTROL_PORT;

    const OTHER_CONTROL_PORT: u16 = DEFAULT_CONTROL_PORT + 1;

    fn peer(device_id: &str, port: u16) -> RolePeer {
        RolePeer::new(
            device_id,
            format!("Device {device_id}"),
            SocketAddr::from(([127, 0, 0, 1], port)),
        )
    }

    fn session_id(state: &RoleState) -> u32 {
        state.session_id().expect("active session")
    }

    #[test]
    fn idle_start_viewing_enters_connecting() {
        let mut machine = RoleStateMachine::new();
        let change = machine
            .start_viewing(peer("peer-a", DEFAULT_CONTROL_PORT), 42, 100)
            .unwrap();

        assert_eq!(change.previous, RoleState::Idle);
        assert_eq!(change.reason, RoleChangeReason::StartViewing);
        assert_eq!(change.current.kind(), RoleKind::Connecting);
        assert_eq!(machine.state().kind(), RoleKind::Connecting);
        assert_eq!(session_id(machine.state()), 42);
    }

    #[test]
    fn same_peer_can_replace_connecting_or_viewing_session() {
        let mut machine = RoleStateMachine::new();
        machine
            .start_viewing(peer("peer-a", DEFAULT_CONTROL_PORT), 1, 100)
            .unwrap();
        machine.mark_viewing_connected(1, 150).unwrap();

        let change = machine
            .start_viewing(peer("peer-a", OTHER_CONTROL_PORT), 2, 200)
            .unwrap();

        assert_eq!(change.previous.kind(), RoleKind::Viewing);
        assert_eq!(change.current.kind(), RoleKind::Connecting);
        assert_eq!(session_id(&change.current), 2);
        assert_eq!(
            change.current.peer().expect("peer").endpoint,
            SocketAddr::from(([127, 0, 0, 1], OTHER_CONTROL_PORT))
        );
    }

    #[test]
    fn viewing_rejects_inbound_serving_by_default() {
        let mut machine = RoleStateMachine::new();
        machine
            .start_viewing(peer("peer-a", DEFAULT_CONTROL_PORT), 1, 100)
            .unwrap();
        machine.mark_viewing_connected(1, 150).unwrap();

        let err = machine
            .accept_serving(peer("peer-b", OTHER_CONTROL_PORT), 9, 200)
            .unwrap_err();

        assert_eq!(
            err,
            RoleStateError::Busy {
                current: RoleKind::Viewing,
                current_peer_device_id: "peer-a".to_string(),
                current_session_id: 1,
            }
        );
        assert_eq!(machine.state().kind(), RoleKind::Viewing);
    }

    #[test]
    fn explicit_policy_can_stop_active_viewing_and_serve() {
        let mut machine = RoleStateMachine::with_config(RoleStateMachineConfig {
            inbound_conflict_policy: InboundConflictPolicy::StopActiveAndServe,
            ..RoleStateMachineConfig::default()
        });
        machine
            .start_viewing(peer("peer-a", DEFAULT_CONTROL_PORT), 1, 100)
            .unwrap();
        machine.mark_viewing_connected(1, 150).unwrap();

        let change = machine
            .accept_serving(peer("peer-b", OTHER_CONTROL_PORT), 2, 200)
            .unwrap();

        assert_eq!(change.previous.kind(), RoleKind::Viewing);
        assert_eq!(change.current.kind(), RoleKind::Serving);
        assert_eq!(session_id(machine.state()), 2);
    }

    #[test]
    fn serving_rejects_start_viewing_until_stopped() {
        let mut machine = RoleStateMachine::new();
        machine
            .accept_serving(peer("peer-a", DEFAULT_CONTROL_PORT), 1, 100)
            .unwrap();

        let err = machine
            .start_viewing(peer("peer-b", OTHER_CONTROL_PORT), 2, 200)
            .unwrap_err();

        assert_eq!(err, machine.busy_error());
        assert_eq!(machine.state().kind(), RoleKind::Serving);

        machine.stop_session(1).unwrap().expect("stop change");
        machine
            .start_viewing(peer("peer-b", OTHER_CONTROL_PORT), 2, 300)
            .unwrap();
        assert_eq!(machine.state().kind(), RoleKind::Connecting);
    }

    #[test]
    fn serving_same_peer_can_refresh_or_replace_session() {
        let mut machine = RoleStateMachine::new();
        machine
            .accept_serving(peer("peer-a", DEFAULT_CONTROL_PORT), 1, 100)
            .unwrap();

        let refresh = machine
            .accept_serving(peer("peer-a", OTHER_CONTROL_PORT), 1, 200)
            .unwrap();
        assert_eq!(refresh.reason, RoleChangeReason::Activity);
        assert_eq!(
            refresh.current.session().expect("serving").last_activity_ms,
            200
        );
        assert_eq!(
            refresh.current.session().expect("serving").peer.endpoint,
            SocketAddr::from(([127, 0, 0, 1], DEFAULT_CONTROL_PORT))
        );

        let replace = machine
            .accept_serving(peer("peer-a", OTHER_CONTROL_PORT), 2, 300)
            .unwrap();
        assert_eq!(replace.reason, RoleChangeReason::AcceptServing);
        assert_eq!(session_id(machine.state()), 2);
        assert_eq!(
            machine.state().peer().expect("peer").endpoint,
            SocketAddr::from(([127, 0, 0, 1], OTHER_CONTROL_PORT))
        );
    }

    #[test]
    fn serving_different_peer_is_busy() {
        let mut machine = RoleStateMachine::new();
        machine
            .accept_serving(peer("peer-a", DEFAULT_CONTROL_PORT), 1, 100)
            .unwrap();

        let err = machine
            .accept_serving(peer("peer-b", OTHER_CONTROL_PORT), 2, 200)
            .unwrap_err();

        assert_eq!(
            err,
            RoleStateError::Busy {
                current: RoleKind::Serving,
                current_peer_device_id: "peer-a".to_string(),
                current_session_id: 1,
            }
        );
    }

    #[test]
    fn mark_viewing_connected_requires_matching_session() {
        let mut machine = RoleStateMachine::new();
        machine
            .start_viewing(peer("peer-a", DEFAULT_CONTROL_PORT), 7, 100)
            .unwrap();

        let err = machine.mark_viewing_connected(8, 150).unwrap_err();

        assert_eq!(
            err,
            RoleStateError::WrongSession {
                current: RoleKind::Connecting,
                expected_session_id: 7,
                actual_session_id: 8,
            }
        );
        assert_eq!(machine.state().kind(), RoleKind::Connecting);

        machine.mark_viewing_connected(7, 160).unwrap();
        assert_eq!(machine.state().kind(), RoleKind::Viewing);
    }

    #[test]
    fn activity_extends_serving_timeout() {
        let mut machine = RoleStateMachine::with_config(RoleStateMachineConfig {
            session_timeout_ms: 100,
            ..RoleStateMachineConfig::default()
        });
        machine
            .accept_serving(peer("peer-a", DEFAULT_CONTROL_PORT), 1, 100)
            .unwrap();
        machine.record_activity(1, 180).unwrap();

        assert!(machine.expire_timed_out(260).is_none());
        let timeout = machine.expire_timed_out(281).expect("timeout change");

        assert_eq!(timeout.reason, RoleChangeReason::Timeout);
        assert_eq!(machine.state(), &RoleState::Idle);
    }

    #[test]
    fn connecting_timeout_returns_to_idle() {
        let mut machine = RoleStateMachine::with_config(RoleStateMachineConfig {
            connecting_timeout_ms: 50,
            ..RoleStateMachineConfig::default()
        });
        machine
            .start_viewing(peer("peer-a", DEFAULT_CONTROL_PORT), 1, 100)
            .unwrap();

        assert!(machine.expire_timed_out(150).is_none());
        let timeout = machine.expire_timed_out(151).expect("timeout change");

        assert_eq!(timeout.previous.kind(), RoleKind::Connecting);
        assert_eq!(machine.state(), &RoleState::Idle);
    }

    #[test]
    fn stop_session_is_idempotent_for_idle_and_strict_for_active_session() {
        let mut machine = RoleStateMachine::new();
        assert!(machine.stop_session(1).unwrap().is_none());
        machine
            .accept_serving(peer("peer-a", DEFAULT_CONTROL_PORT), 7, 100)
            .unwrap();

        let err = machine.stop_session(8).unwrap_err();
        assert_eq!(
            err,
            RoleStateError::WrongSession {
                current: RoleKind::Serving,
                expected_session_id: 7,
                actual_session_id: 8,
            }
        );

        let stop = machine.stop_session(7).unwrap().expect("stop");
        assert_eq!(stop.reason, RoleChangeReason::Stop);
        assert_eq!(machine.state(), &RoleState::Idle);
    }

    #[test]
    fn empty_peer_fields_fall_back_to_endpoint_identity() {
        let endpoint = SocketAddr::from(([10, 0, 0, 2], DEFAULT_CONTROL_PORT));
        let peer = RolePeer::endpoint_only(endpoint);

        assert_eq!(
            peer.device_id,
            format!("endpoint:10.0.0.2:{DEFAULT_CONTROL_PORT}")
        );
        assert_eq!(
            peer.display_name,
            format!("10.0.0.2:{DEFAULT_CONTROL_PORT}")
        );
        assert_eq!(peer.endpoint, endpoint);
    }
}
