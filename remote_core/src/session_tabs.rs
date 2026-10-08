//! Pure session navigation and connection intent; no renderer, media or executor.
use std::{collections::BTreeMap, net::SocketAddr};

pub const MAX_CONNECTIONS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPeer {
    pub device_id: String,
    pub name: String,
    pub endpoint: SocketAddr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionHealth {
    pub connected: bool,
    pub peer_responsive: bool,
    pub video_failed: bool,
}
impl SessionHealth {
    pub const fn reusable(self) -> bool {
        self.connected && self.peer_responsive && !self.video_failed
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SessionCandidate {
    pub index: usize,
    pub health: SessionHealth,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectingSession {
    pub peer: SessionPeer,
    pub attempt: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenDecision {
    Reuse(usize),
    AlreadyPending,
    Start(u64),
    AtCapacity,
    GenerationExhausted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenPlan {
    pub retire_index: Option<usize>,
    pub decision: OpenDecision,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    Cancelled,
    Selected,
    Background,
}

/// The Owner keeps this beside its native session objects under its existing lock.
#[derive(Default, Debug)]
pub struct SessionTabsState {
    selected: usize,
    pending: BTreeMap<String, u64>,
    intent: Option<String>,
    next_attempt: u64,
    connecting: Option<ConnectingSession>,
}
impl SessionTabsState {
    pub fn selected_index(&self) -> usize {
        self.selected
    }
    pub fn connecting(&self) -> Option<&ConnectingSession> {
        self.connecting.as_ref()
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn can_change_network(&self, sessions: usize) -> bool {
        sessions == 0 && self.pending.is_empty()
    }

    pub fn select(&mut self, index: usize, key: &str, sessions: usize) -> bool {
        if index >= sessions {
            return false;
        }
        self.selected = index;
        self.intent = Some(key.to_owned());
        self.connecting = None;
        true
    }

    /// Return only resource decisions; the Owner drops/reuses/opens actual sessions.
    pub fn plan_open(
        &mut self,
        peer: SessionPeer,
        existing: Option<SessionCandidate>,
        mut sessions: usize,
    ) -> OpenPlan {
        let existing = existing.filter(|candidate| candidate.index < sessions);
        if let Some(candidate) = existing
            && candidate.health.reusable()
        {
            self.select(candidate.index, &peer.device_id, sessions);
            return OpenPlan {
                retire_index: None,
                decision: OpenDecision::Reuse(candidate.index),
            };
        }
        let retire_index = existing.map(|candidate| candidate.index);
        if let Some(index) = retire_index {
            self.close_index(index, sessions);
            sessions -= 1;
        }
        let decision = if let Some(&attempt) = self.pending.get(&peer.device_id) {
            self.intent = Some(peer.device_id.clone());
            self.connecting = Some(ConnectingSession { peer, attempt });
            OpenDecision::AlreadyPending
        } else if sessions + self.pending.len() >= MAX_CONNECTIONS {
            OpenDecision::AtCapacity
        } else if let Some(attempt) = self.next_attempt.checked_add(1) {
            self.next_attempt = attempt;
            self.intent = Some(peer.device_id.clone());
            self.pending.insert(peer.device_id.clone(), attempt);
            self.connecting = Some(ConnectingSession { peer, attempt });
            OpenDecision::Start(attempt)
        } else {
            OpenDecision::GenerationExhausted
        };
        OpenPlan {
            retire_index,
            decision,
        }
    }

    /// Reject cancelled or replaced generations before the Owner attaches any resource.
    pub fn complete(
        &mut self,
        key: &str,
        attempt: u64,
        success: bool,
        sessions: usize,
    ) -> Completion {
        if self.pending.get(key) != Some(&attempt) {
            return Completion::Cancelled;
        }
        self.pending.remove(key);
        if self.intent.as_deref() != Some(key) {
            return Completion::Background;
        }
        self.connecting = None;
        if !success {
            self.selected = sessions;
        }
        Completion::Selected
    }

    pub fn attached(&mut self, completion: Completion, sessions: usize) {
        if sessions == 0 || completion == Completion::Cancelled {
            return;
        }
        if completion == Completion::Selected {
            self.selected = sessions - 1;
        } else if self.selected >= sessions - 1 {
            // Appending a background result must preserve an empty selection,
            // especially after the newest foreground connection has failed.
            self.selected = if sessions == 1 && self.intent.is_none() {
                0
            } else {
                sessions
            };
        }
    }

    /// Preserve the same page when a preceding/other page closes; current chooses its neighbour.
    pub fn close_index(&mut self, index: usize, sessions: usize) -> bool {
        if index >= sessions {
            return false;
        }
        let remaining = sessions - 1;
        self.selected = if self.selected >= sessions {
            remaining // A failed connection's empty selection must not resurrect an unrelated page.
        } else if remaining == 0 {
            0
        } else {
            self.selected
                .saturating_sub(usize::from(self.selected > index))
                .min(remaining - 1)
        };
        true
    }

    /// A pending connect is cancelled without closing the previous available page.
    pub fn disconnect(&mut self, sessions: usize) -> Option<usize> {
        let cancelling = self.connecting.take().is_some();
        if let Some(key) = self.intent.take() {
            self.pending.remove(&key);
        }
        let index = self.selected;
        if !cancelling && self.close_index(index, sessions) {
            Some(index)
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.pending.clear();
        self.intent = None;
        self.connecting = None;
        self.selected = 0;
        // Keep the generation monotonic so an old reply cannot match an open after clear.
    }

    pub fn project<'a>(
        &self,
        sessions: impl IntoIterator<Item = SessionFacts<'a>>,
    ) -> SessionTabsViewModel {
        let tabs: Vec<_> = sessions
            .into_iter()
            .enumerate()
            .map(|(index, facts)| SessionTabViewModel {
                connection_id: facts.connection_id,
                device_id: facts.device_id.to_owned(),
                label: facts.name.to_owned(),
                selected: index == self.selected,
                connection: if !facts.connected {
                    ConnectionStatus::Disconnected
                } else if facts.video_confirmed && facts.has_media {
                    ConnectionStatus::Connected
                } else {
                    ConnectionStatus::Connecting
                },
            })
            .collect();
        let active = self.connecting.is_none() && self.selected < tabs.len();
        SessionTabsViewModel {
            tabs,
            can_disconnect: active || self.connecting.is_some(),
            can_reconnect: active,
        }
    }
}

/// Scalar observations only; decoded frames, surfaces and native handles stay in the Owner.
pub struct SessionFacts<'a> {
    pub connection_id: u32,
    pub device_id: &'a str,
    pub name: &'a str,
    pub connected: bool,
    pub video_confirmed: bool,
    pub has_media: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Disconnected,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTabViewModel {
    pub connection_id: u32,
    pub device_id: String,
    pub label: String,
    pub selected: bool,
    /// Protocol/stream state, never a claim of actual frame presentation.
    pub connection: ConnectionStatus,
}
pub struct SessionTabsViewModel {
    pub tabs: Vec<SessionTabViewModel>,
    pub can_disconnect: bool,
    pub can_reconnect: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTabsAction {
    Select(u32),
    Close(u32),
    Disconnect,
    Reconnect,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTabsEffect {
    Select(u32),
    Close(u32),
    Disconnect,
    Reconnect,
}
impl SessionTabsViewModel {
    pub fn effect(&self, action: SessionTabsAction) -> Option<SessionTabsEffect> {
        match action {
            SessionTabsAction::Select(id)
                if self.tabs.iter().any(|tab| tab.connection_id == id) =>
            {
                Some(SessionTabsEffect::Select(id))
            }
            SessionTabsAction::Close(id) if self.tabs.iter().any(|tab| tab.connection_id == id) => {
                Some(SessionTabsEffect::Close(id))
            }
            SessionTabsAction::Disconnect if self.can_disconnect => {
                Some(SessionTabsEffect::Disconnect)
            }
            SessionTabsAction::Reconnect if self.can_reconnect => {
                Some(SessionTabsEffect::Reconnect)
            }
            _ => None,
        }
    }
}

/// Per-view command receipts fence late async status callbacks; not network identity.
#[derive(Default)]
pub struct SessionCommandState {
    revision: u64,
}
#[derive(Clone, Copy)]
pub struct CommandTicket(u64);
impl SessionCommandState {
    pub fn begin(&mut self) -> CommandTicket {
        self.revision = self.revision.wrapping_add(1);
        CommandTicket(self.revision)
    }
    pub fn is_current(&self, ticket: CommandTicket) -> bool {
        self.revision == ticket.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn peer(key: &str) -> SessionPeer {
        SessionPeer {
            device_id: key.into(),
            name: key.into(),
            endpoint: "127.0.0.1:1".parse().unwrap(),
        }
    }
    fn start(state: &mut SessionTabsState, key: &str, count: usize) -> u64 {
        match state.plan_open(peer(key), None, count).decision {
            OpenDecision::Start(attempt) => attempt,
            other => panic!("{other:?}"),
        }
    }
    fn facts(id: u32, key: &str, connected: bool) -> SessionFacts<'_> {
        SessionFacts {
            connection_id: id,
            device_id: key,
            name: key,
            connected,
            video_confirmed: true,
            has_media: true,
        }
    }

    #[test]
    fn repeated_open_reuses_healthy_connection_and_replaces_unhealthy_one() {
        let mut s = SessionTabsState::default();
        let healthy = SessionHealth {
            connected: true,
            peer_responsive: true,
            video_failed: false,
        };
        let p = s.plan_open(
            peer("a"),
            Some(SessionCandidate {
                index: 1,
                health: healthy,
            }),
            2,
        );
        assert_eq!(
            p,
            OpenPlan {
                retire_index: None,
                decision: OpenDecision::Reuse(1)
            }
        );
        assert_eq!(s.selected_index(), 1);
        assert_eq!(s.pending_count(), 0);
        for health in [
            SessionHealth {
                connected: false,
                ..healthy
            },
            SessionHealth {
                peer_responsive: false,
                ..healthy
            },
            SessionHealth {
                video_failed: true,
                ..healthy
            },
        ] {
            let mut s = SessionTabsState::default();
            let p = s.plan_open(peer("a"), Some(SessionCandidate { index: 0, health }), 1);
            assert_eq!(p.retire_index, Some(0));
            assert!(matches!(p.decision, OpenDecision::Start(_)));
        }
    }
    #[test]
    fn pending_duplicate_and_capacity_do_not_create_more_attempts() {
        let mut s = SessionTabsState::default();
        let a = start(&mut s, "a", 7);
        assert_eq!(
            s.plan_open(peer("a"), None, 7).decision,
            OpenDecision::AlreadyPending
        );
        assert_eq!(
            s.plan_open(peer("b"), None, 7).decision,
            OpenDecision::AtCapacity
        );
        assert_eq!(s.pending_count(), 1);
        assert_eq!(s.connecting().unwrap().attempt, a);
        assert_eq!(s.complete("a", a, true, 7), Completion::Selected);
    }
    #[test]
    fn selecting_a_pending_request_tracks_it_without_restarting_or_clearing_other_attempts() {
        let mut state = SessionTabsState::default();
        let a = start(&mut state, "a", 0);
        let b = start(&mut state, "b", 0);
        assert_eq!(
            state.plan_open(peer("a"), None, 0).decision,
            OpenDecision::AlreadyPending
        );
        assert_eq!(state.pending_count(), 2);
        assert_eq!(state.connecting().unwrap().peer.device_id, "a");
        assert_eq!(state.connecting().unwrap().attempt, a);
        assert_eq!(state.complete("b", b, true, 0), Completion::Background);
        state.attached(Completion::Background, 1);
        assert_eq!(state.connecting().unwrap().attempt, a);
        assert!(!state.project([facts(11, "b", true)]).can_reconnect);
        assert_eq!(state.complete("a", a, true, 1), Completion::Selected);
        state.attached(Completion::Selected, 2);
        assert_eq!(state.selected_index(), 1);
        assert_eq!(state.pending_count(), 0);
    }

    #[test]
    fn switching_and_closing_keep_selection_of_current_other_and_last_pages() {
        let mut s = SessionTabsState::default();
        assert!(s.select(2, "c", 3));
        assert!(!s.select(3, "missing", 3));
        assert_eq!(s.selected_index(), 2);
        assert!(s.close_index(0, 3));
        assert_eq!(s.selected_index(), 1);
        assert!(s.close_index(1, 2));
        assert_eq!(s.selected_index(), 0);
        assert!(s.close_index(0, 1));
        assert_eq!(s.selected_index(), 0);
        assert!(!s.close_index(0, 0));
        s.select(0, "a", 3);
        s.close_index(2, 3);
        assert_eq!(s.selected_index(), 0);
        s.select(1, "b", 3);
        s.close_index(1, 3);
        assert_eq!(s.selected_index(), 1);
    }
    #[test]
    fn cancelling_connect_preserves_old_page_and_late_completion_is_rejected() {
        let mut s = SessionTabsState::default();
        s.select(0, "a", 1);
        let b = start(&mut s, "b", 1);
        assert_eq!(s.disconnect(1), None);
        assert_eq!(s.selected_index(), 0);
        assert_eq!(s.complete("b", b, true, 1), Completion::Cancelled);
        assert_eq!(s.disconnect(1), Some(0));
        assert!(s.can_change_network(0));
    }
    #[test]
    fn reopen_after_cancel_or_clear_uses_new_generation_and_rejects_old_reply() {
        let mut s = SessionTabsState::default();
        let old = start(&mut s, "a", 0);
        s.disconnect(0);
        let new = start(&mut s, "a", 0);
        assert_ne!(old, new);
        assert_eq!(s.complete("a", old, true, 0), Completion::Cancelled);
        assert_eq!(s.connecting().unwrap().attempt, new);
        assert_eq!(s.pending_count(), 1);
        s.clear();
        let third = start(&mut s, "a", 0);
        assert!(third > new);
        assert_eq!(s.complete("a", new, false, 0), Completion::Cancelled);
        assert_eq!(s.complete("a", third, true, 0), Completion::Selected);
        s.attached(Completion::Selected, 1);
        assert_eq!(s.selected_index(), 0);
    }
    #[test]
    fn background_response_cannot_select_over_new_intent_and_failure_hides_old_page() {
        let mut s = SessionTabsState::default();
        s.select(0, "a", 1);
        let b = start(&mut s, "b", 1);
        s.select(0, "a", 1);
        assert_eq!(s.complete("b", b, true, 1), Completion::Background);
        s.attached(Completion::Background, 2);
        assert_eq!(s.selected_index(), 0);
        let c = start(&mut s, "c", 2);
        assert_eq!(s.complete("c", c, false, 2), Completion::Selected);
        assert_eq!(s.selected_index(), 2);
        s.close_index(0, 2);
        assert_eq!(s.selected_index(), 1);
        assert!(!s.project([facts(2, "b", true)]).can_reconnect);
    }
    #[test]
    fn reconnect_actions_and_status_projection_use_scalar_identity_not_frames() {
        let mut s = SessionTabsState::default();
        s.select(1, "b", 2);
        let v = s.project([facts(11, "a", true), facts(22, "b", false)]);
        assert_eq!(v.tabs[1].connection, ConnectionStatus::Disconnected);
        assert!(v.tabs[1].selected);
        assert_eq!(
            v.effect(SessionTabsAction::Select(11)),
            Some(SessionTabsEffect::Select(11))
        );
        assert_eq!(
            v.effect(SessionTabsAction::Close(22)),
            Some(SessionTabsEffect::Close(22))
        );
        assert_eq!(v.effect(SessionTabsAction::Close(99)), None);
        assert_eq!(
            v.effect(SessionTabsAction::Reconnect),
            Some(SessionTabsEffect::Reconnect)
        );
        let reconnect = s.disconnect(2);
        assert_eq!(reconnect, Some(1));
        let a = start(&mut s, "b", 1);
        assert!(!s.project([facts(11, "a", true)]).can_reconnect);
        assert_eq!(s.complete("b", a, true, 1), Completion::Selected);
        s.attached(Completion::Selected, 2);
        assert_eq!(s.selected_index(), 1);
        let waiting = SessionFacts {
            video_confirmed: false,
            ..facts(22, "b", true)
        };
        assert_eq!(
            s.project([waiting]).tabs[0].connection,
            ConnectionStatus::Connecting
        );
    }
    #[test]
    fn late_status_callback_cannot_overwrite_a_new_selection_or_reconnect() {
        let mut state = SessionCommandState::default();
        let old = state.begin();
        assert!(state.is_current(old));
        let selection = state.begin();
        assert!(!state.is_current(old));
        assert!(state.is_current(selection));
        let reconnect = state.begin();
        assert!(!state.is_current(selection));
        assert!(state.is_current(reconnect));
    }
    #[test]
    fn late_background_attach_preserves_empty_selection_after_foreground_failure() {
        for count in [0, 1] {
            let mut state = SessionTabsState::default();
            if count == 1 {
                state.select(0, "existing", 1);
            }
            let old = start(&mut state, "old", count);
            let current = start(&mut state, "current", count);
            assert_eq!(
                state.complete("current", current, false, count),
                Completion::Selected
            );
            assert_eq!(
                state.complete("old", old, true, count),
                Completion::Background
            );
            state.attached(Completion::Background, count + 1);
            assert_eq!(state.selected_index(), count + 1);
            let rows = if count == 0 {
                vec![facts(11, "old", true)]
            } else {
                vec![facts(10, "existing", true), facts(11, "old", true)]
            };
            let view = state.project(rows);
            assert!(!view.can_reconnect);
            assert!(view.tabs.iter().all(|tab| !tab.selected));
        }
    }
}
