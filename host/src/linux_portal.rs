//! Control-plane invariants for one explicit local portal selection.
//!
//! Pure ownership rules live here; runtime implements the real D-Bus requests,
//! cancellation, restricted FD handoff and prepared owned-frame resource.
//! No wire command, restore token, global PipeWire connection, or second UI is added.

use protocol::session::CaptureSource;
use remote_core::shared_files::ShareScope;
use std::os::fd::OwnedFd;

pub mod runtime;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PeerOwner {
    connection_id: u32,
    authentication_nonce: [u8; 16],
    share_scope: [u8; 32],
}

impl PeerOwner {
    /// Caller must obtain nonce from the host's verified authentication table.
    /// An address, display name, or a remotely supplied node ID is insufficient.
    pub fn from_authenticated_connection(
        connection_id: u32,
        authentication_nonce: [u8; 16],
        share_scope: ShareScope,
    ) -> Result<Self, PortalError> {
        if connection_id == 0 {
            return Err(PortalError::Owner);
        }
        Ok(Self {
            connection_id,
            authentication_nonce,
            share_scope: share_scope.ok_or(PortalError::Owner)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Creating,
    Selecting,
    Starting,
    Ready,
    Streaming,
    Paused,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Create,
    Select,
    Start,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Monitor,
    Window,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionOptions {
    pub monitor: bool,
    pub window: bool,
    pub multiple: bool,
    pub persist_mode: u32,
    pub embedded_cursor: bool,
}

impl SelectionOptions {
    pub fn from_capabilities(source_types: u32, cursor_modes: u32) -> Result<Self, PortalError> {
        // Portal enums: MONITOR=1, WINDOW=2, VIRTUAL=4, HIDDEN=1, EMBEDDED=2.
        // Cursor metadata requires another implementation; never select it.
        let options = Self {
            monitor: source_types & 1 != 0,
            window: source_types & 2 != 0,
            multiple: false,
            persist_mode: 0,
            embedded_cursor: cursor_modes & 2 != 0,
        };
        if !options.monitor && !options.window || cursor_modes & 3 == 0 {
            return Err(PortalError::Capabilities);
        }
        Ok(options)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalError {
    Owner,
    Capabilities,
    State,
    Handle,
    Response,
    Generation,
    Source,
    SelectionBusy,
    Stale,
}
impl std::fmt::Display for PortalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native Linux portal boundary: {self:?}")
    }
}
impl std::error::Error for PortalError {}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct ClosePlan {
    pub request: Option<String>,
    pub session: Option<String>,
    pub capture_generation: Option<u64>,
}

pub struct PortalSessionState {
    phase: Phase,
    generation: u64,
    session: String,
    request: Option<(Method, String)>,
    selection_options: SelectionOptions,
}

fn valid_handle(path: &str, kind: &str) -> bool {
    let prefix = format!("/org/freedesktop/portal/desktop/{kind}/");
    path.strip_prefix(&prefix).is_some_and(|suffix| {
        let parts: Vec<_> = suffix.split('/').collect();
        parts.len() == 2
            && parts.iter().all(|part| {
                !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
    })
}

impl PortalSessionState {
    /// Reserve a session token/path before Create, so cancellation before its
    /// Response still knows which session must be closed on a late response.
    pub fn new(
        generation: u64,
        session: String,
        options: SelectionOptions,
    ) -> Result<Self, PortalError> {
        if generation == 0 {
            return Err(PortalError::Generation);
        }
        if !valid_handle(&session, "session") {
            return Err(PortalError::Handle);
        }
        if options.multiple || options.persist_mode != 0 || !options.monitor && !options.window {
            return Err(PortalError::Capabilities);
        }
        Ok(Self {
            phase: Phase::Idle,
            generation,
            session,
            request: None,
            selection_options: options,
        })
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Only after the real Response listener has been installed may the
    /// adapter invoke the matching method. Select and Start cannot repeat.
    pub fn begin_request(&mut self, method: Method, path: String) -> Result<(), PortalError> {
        if !valid_handle(&path, "request") {
            return Err(PortalError::Handle);
        }
        if self.request.is_some() {
            return Err(PortalError::State);
        }
        self.phase = match (self.phase, method) {
            (Phase::Idle, Method::Create) => Phase::Creating,
            (Phase::Selecting, Method::Select) => Phase::Selecting,
            (Phase::Starting, Method::Start) => Phase::Starting,
            _ => return Err(PortalError::State),
        };
        self.request = Some((method, path));
        Ok(())
    }

    pub fn complete_request(
        &mut self,
        path: &str,
        response_code: u32,
        returned_session: Option<&str>,
        source_kind: Option<SourceKind>,
    ) -> Result<(), PortalError> {
        let Some((method, pending)) = &self.request else {
            return Err(PortalError::Stale);
        };
        if pending != path {
            return Err(PortalError::Stale);
        }
        if response_code != 0 {
            return Err(PortalError::Response);
        }
        match method {
            Method::Create if returned_session == Some(self.session.as_str()) => {
                self.phase = Phase::Selecting
            }
            Method::Select => self.phase = Phase::Starting,
            Method::Start
                if source_kind.is_some_and(|kind| match kind {
                    SourceKind::Monitor => self.selection_options.monitor,
                    SourceKind::Window => self.selection_options.window,
                }) =>
            {
                self.phase = Phase::Ready
            }
            _ => return Err(PortalError::Source),
        }
        self.request = None;
        Ok(())
    }

    /// Ready means selection succeeded. The live worker must additionally
    /// validate the first negotiated format before committing a replacement.
    pub fn set_streaming(&mut self, generation: u64) -> Result<(), PortalError> {
        if generation != self.generation || !matches!(self.phase, Phase::Ready | Phase::Paused) {
            return Err(PortalError::State);
        }
        self.phase = Phase::Streaming;
        Ok(())
    }
    pub fn pause(&mut self) -> Result<(), PortalError> {
        if self.phase != Phase::Streaming {
            return Err(PortalError::State);
        }
        self.phase = Phase::Paused;
        Ok(())
    }

    /// Timeout, cancellation, failure and Session.Closed all revoke delivery.
    /// Request.Close does not produce Response; do not wait for one afterward.
    pub fn close(&mut self) -> ClosePlan {
        if self.phase == Phase::Closed {
            return ClosePlan::default();
        }
        self.phase = Phase::Closed;
        ClosePlan {
            request: self.request.take().map(|(_, path)| path),
            session: Some(self.session.clone()),
            capture_generation: Some(self.generation),
        }
    }
}

/// Single ownership transfer, never clone/dup or a default-global connection.
pub struct RestrictedRemote {
    generation: u64,
    node_id: u32,
    fd: Option<OwnedFd>,
}
impl RestrictedRemote {
    pub fn new(state: &PortalSessionState, node_id: u32, fd: OwnedFd) -> Result<Self, PortalError> {
        if state.phase != Phase::Ready || node_id == 0 || node_id == u32::MAX {
            return Err(PortalError::State);
        }
        Ok(Self {
            generation: state.generation,
            node_id,
            fd: Some(fd),
        })
    }
    pub fn take_for_connect(
        &mut self,
        state: &PortalSessionState,
    ) -> Result<(u32, OwnedFd), PortalError> {
        if state.generation != self.generation {
            return Err(PortalError::Generation);
        }
        if state.phase != Phase::Ready {
            return Err(PortalError::State);
        }
        Ok((self.node_id, self.fd.take().ok_or(PortalError::State)?))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SelectionTicket {
    owner: PeerOwner,
    source_revision: u32,
    serial: u64,
}

struct Grant<T> {
    owner: PeerOwner,
    source: CaptureSource,
    resource: T,
}

/// The adapter owns actual RAII leases as T. Prepare does not remove the old
/// grant. Commit rechecks authentication, scope and revision after the picker.
pub struct LeaseRegistry<T> {
    active: Option<Grant<T>>,
    pending: Option<SelectionTicket>,
    serial: u64,
}
impl<T> Default for LeaseRegistry<T> {
    fn default() -> Self {
        Self {
            active: None,
            pending: None,
            serial: 0,
        }
    }
}
impl<T> LeaseRegistry<T> {
    pub fn begin_selection(
        &mut self,
        owner: PeerOwner,
        source_revision: u32,
    ) -> Result<SelectionTicket, PortalError> {
        if self.pending.is_some() {
            return Err(PortalError::SelectionBusy);
        }
        self.serial = self.serial.checked_add(1).ok_or(PortalError::Generation)?;
        let ticket = SelectionTicket {
            owner,
            source_revision,
            serial: self.serial,
        };
        self.pending = Some(ticket);
        Ok(ticket)
    }
    pub fn cancel_selection(&mut self, ticket: SelectionTicket) -> bool {
        if self.pending != Some(ticket) {
            return false;
        }
        self.pending = None;
        true
    }
    pub fn commit(
        &mut self,
        ticket: SelectionTicket,
        current_owner: PeerOwner,
        current_revision: u32,
        source: CaptureSource,
        prepared: T,
    ) -> Result<Option<T>, (PortalError, T)> {
        if self.pending != Some(ticket)
            || ticket.owner != current_owner
            || ticket.source_revision != current_revision
        {
            return Err((PortalError::Stale, prepared));
        }
        if !matches!(source, CaptureSource::Display(id) | CaptureSource::Window(id) if id != 0) {
            return Err((PortalError::Source, prepared));
        }
        self.pending = None;
        Ok(self
            .active
            .replace(Grant {
                owner: current_owner,
                source,
                resource: prepared,
            })
            .map(|old| old.resource))
    }
    pub fn source_for(&self, owner: PeerOwner) -> Option<CaptureSource> {
        self.active
            .as_ref()
            .filter(|grant| grant.owner == owner)
            .map(|grant| grant.source)
    }
    pub fn revoke(&mut self) -> Option<T> {
        self.pending = None;
        self.active.take().map(|grant| grant.resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SESSION: &str = "/org/freedesktop/portal/desktop/session/1_4/rp_session";
    const CREATE: &str = "/org/freedesktop/portal/desktop/request/1_4/rp_create";
    const SELECT: &str = "/org/freedesktop/portal/desktop/request/1_4/rp_select";
    const START: &str = "/org/freedesktop/portal/desktop/request/1_4/rp_start";
    fn state() -> PortalSessionState {
        PortalSessionState::new(
            7,
            SESSION.into(),
            SelectionOptions::from_capabilities(3, 3).unwrap(),
        )
        .unwrap()
    }
    fn ready() -> PortalSessionState {
        let mut state = state();
        state.begin_request(Method::Create, CREATE.into()).unwrap();
        state
            .complete_request(CREATE, 0, Some(SESSION), None)
            .unwrap();
        state.begin_request(Method::Select, SELECT.into()).unwrap();
        state.complete_request(SELECT, 0, None, None).unwrap();
        state.begin_request(Method::Start, START.into()).unwrap();
        state
            .complete_request(START, 0, None, Some(SourceKind::Window))
            .unwrap();
        state
    }
    fn owner(connection: u32, nonce: u8, scope: u8) -> PeerOwner {
        PeerOwner::from_authenticated_connection(connection, [nonce; 16], Some([scope; 32]))
            .unwrap()
    }
    #[test]
    fn selection_is_once_and_pause_never_reopens_the_picker() {
        let mut state = ready();
        assert_eq!(state.phase(), Phase::Ready);
        assert_eq!(
            state.begin_request(Method::Start, START.into()),
            Err(PortalError::State)
        );
        state.set_streaming(7).unwrap();
        state.pause().unwrap();
        state.set_streaming(7).unwrap();
        assert_eq!(
            state.begin_request(Method::Select, SELECT.into()),
            Err(PortalError::State)
        );
        assert_eq!(state.set_streaming(8), Err(PortalError::State));
    }
    #[test]
    fn cancellation_does_not_wait_for_response_and_late_response_cannot_revive() {
        let mut state = state();
        state.begin_request(Method::Create, CREATE.into()).unwrap();
        let close = state.close();
        assert_eq!(close.request.as_deref(), Some(CREATE));
        assert_eq!(close.session.as_deref(), Some(SESSION));
        assert_eq!(close.capture_generation, Some(7));
        assert_eq!(state.phase(), Phase::Closed);
        assert_eq!(
            state.complete_request(CREATE, 0, Some(SESSION), None),
            Err(PortalError::Stale)
        );
        assert_eq!(state.close(), ClosePlan::default());
    }
    #[test]
    fn user_rejection_and_foreign_handle_require_explicit_close() {
        let mut state = state();
        state.begin_request(Method::Create, CREATE.into()).unwrap();
        assert_eq!(
            state.complete_request(SELECT, 0, Some(SESSION), None),
            Err(PortalError::Stale)
        );
        assert_eq!(
            state.complete_request(CREATE, 1, None, None),
            Err(PortalError::Response)
        );
        assert_eq!(state.close().request.as_deref(), Some(CREATE));
    }
    #[test]
    fn only_selected_source_kind_can_reach_ready() {
        for granted in [SourceKind::Monitor, SourceKind::Window] {
            let mut state = PortalSessionState::new(
                7,
                SESSION.into(),
                SelectionOptions::from_capabilities(1, 3).unwrap(),
            )
            .unwrap();
            state.begin_request(Method::Create, CREATE.into()).unwrap();
            state
                .complete_request(CREATE, 0, Some(SESSION), None)
                .unwrap();
            state.begin_request(Method::Select, SELECT.into()).unwrap();
            state.complete_request(SELECT, 0, None, None).unwrap();
            state.begin_request(Method::Start, START.into()).unwrap();
            let result = state.complete_request(START, 0, None, Some(granted));
            if granted == SourceKind::Monitor {
                assert_eq!(result, Ok(()));
                assert_eq!(state.phase(), Phase::Ready);
            } else {
                assert_eq!(result, Err(PortalError::Source));
                assert_eq!(state.close().request.as_deref(), Some(START));
            }
        }
    }
    #[test]
    fn no_virtual_persistent_or_metadata_cursor_permission() {
        assert_eq!(
            SelectionOptions::from_capabilities(4, 3),
            Err(PortalError::Capabilities)
        );
        assert_eq!(
            SelectionOptions::from_capabilities(3, 4),
            Err(PortalError::Capabilities)
        );
        let options = SelectionOptions::from_capabilities(7, 3).unwrap();
        assert!(!options.multiple);
        assert_eq!(options.persist_mode, 0);
        assert!(options.embedded_cursor);
        assert!(
            PortalSessionState::new(
                7,
                SESSION.into(),
                SelectionOptions {
                    persist_mode: 1,
                    ..options
                }
            )
            .is_err()
        );
    }
    #[test]
    fn restricted_fd_can_transfer_once_and_untransferred_fd_closes_on_drop() {
        use std::io::Read;
        use std::os::unix::net::UnixStream;
        let (stream, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(std::time::Duration::from_millis(100)))
            .unwrap();
        let state = ready();
        let mut remote = RestrictedRemote::new(&state, 77, stream.into()).unwrap();
        let mut wrong_generation = ready();
        wrong_generation.generation = 8;
        assert!(remote.take_for_connect(&wrong_generation).is_err());
        let (node, fd) = remote.take_for_connect(&state).unwrap();
        assert_eq!(node, 77);
        assert!(remote.take_for_connect(&state).is_err());
        drop(fd);
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        let (stream, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(std::time::Duration::from_millis(100)))
            .unwrap();
        drop(RestrictedRemote::new(&ready(), 77, stream.into()).unwrap());
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }
    #[test]
    fn session_revocation_prevents_even_the_first_fd_handoff() {
        let (stream, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut state = ready();
        let mut remote = RestrictedRemote::new(&state, 77, stream.into()).unwrap();
        state.close();
        assert!(remote.take_for_connect(&state).is_err());
    }
    #[test]
    fn cancel_and_failed_preparation_preserve_the_active_source() {
        let peer = owner(1, 2, 3);
        let mut registry = LeaseRegistry::default();
        let first = registry.begin_selection(peer, 0).unwrap();
        assert_eq!(
            registry.commit(first, peer, 0, CaptureSource::Window(100), "old"),
            Ok(None)
        );
        let next = registry.begin_selection(peer, 1).unwrap();
        assert!(registry.begin_selection(peer, 1).is_err());
        assert!(registry.cancel_selection(next));
        assert_eq!(registry.source_for(peer), Some(CaptureSource::Window(100)));
        assert_eq!(
            registry.commit(next, peer, 1, CaptureSource::Window(200), "new"),
            Err((PortalError::Stale, "new"))
        );
        assert_eq!(registry.revoke(), Some("old"));
    }
    #[test]
    fn owner_nonce_scope_and_revision_are_rechecked_after_selection() {
        let peer = owner(1, 2, 3);
        let mut registry = LeaseRegistry::default();
        let ticket = registry.begin_selection(peer, 5).unwrap();
        for changed in [owner(2, 2, 3), owner(1, 9, 3), owner(1, 2, 9)] {
            assert_eq!(
                registry.commit(ticket, changed, 5, CaptureSource::Display(100), "new"),
                Err((PortalError::Stale, "new"))
            );
            assert!(registry.source_for(changed).is_none());
        }
        assert_eq!(
            registry.commit(ticket, peer, 6, CaptureSource::Display(100), "new"),
            Err((PortalError::Stale, "new"))
        );
        assert_eq!(
            registry.commit(ticket, peer, 5, CaptureSource::MainDisplay, "new"),
            Err((PortalError::Source, "new"))
        );
        assert_eq!(
            registry.commit(ticket, peer, 5, CaptureSource::Display(100), "new"),
            Ok(None)
        );
        assert!(registry.source_for(owner(1, 9, 3)).is_none());
    }
}
