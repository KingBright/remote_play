//! Reliable, session-scoped desired media state over the existing UDP route.
use protocol::ControlMessage;
use std::{
    net::SocketAddr,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::Notify;

const RETRY: Duration = Duration::from_millis(200);

#[derive(Default)]
pub struct MediaPauseControl {
    state: Mutex<Option<State>>,
    pub(crate) changed: Notify,
}

struct State {
    target: SocketAddr,
    session_id: u32,
    revision: u64,
    paused: bool,
    acknowledged: bool,
    last_sent: Option<Instant>,
}

impl MediaPauseControl {
    pub fn begin_session(&self, target: SocketAddr, session_id: u32) {
        *self.state.lock().unwrap() = Some(State {
            target,
            session_id,
            revision: 0,
            paused: false,
            acknowledged: true,
            last_sent: None,
        });
        self.changed.notify_one();
    }

    pub fn end_session(&self) {
        *self.state.lock().unwrap() = None;
        self.changed.notify_one();
    }

    /// Returns false when there is no session. Repeating a desired state is a no-op.
    pub fn set_paused(&self, paused: bool) -> bool {
        let mut guard = self.state.lock().unwrap();
        let Some(state) = guard.as_mut() else {
            return false;
        };
        if state.paused != paused {
            state.paused = paused;
            state.revision += 1;
            state.acknowledged = false;
            state.last_sent = None;
            self.changed.notify_one();
        }
        true
    }

    pub fn is_paused(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.paused)
    }

    pub fn is_pending(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| !s.acknowledged)
    }

    pub(crate) fn retry_wait(&self) -> Option<Duration> {
        let guard = self.state.lock().unwrap();
        let state = guard.as_ref().filter(|s| !s.acknowledged)?;
        Some(RETRY.saturating_sub(state.last_sent.map_or(RETRY, |t| t.elapsed())))
    }

    pub(crate) fn take_request(&self) -> Option<(ControlMessage, SocketAddr)> {
        let mut guard = self.state.lock().unwrap();
        let state = guard.as_mut().filter(|s| !s.acknowledged)?;
        if state.last_sent.is_some_and(|t| t.elapsed() < RETRY) {
            return None;
        }
        state.last_sent = Some(Instant::now());
        Some((
            ControlMessage::SetMediaPaused {
                session_id: state.session_id,
                revision: state.revision,
                paused: state.paused,
            },
            state.target,
        ))
    }

    pub(crate) fn acknowledge(
        &self,
        source: SocketAddr,
        session_id: u32,
        revision: u64,
        paused: bool,
    ) {
        if let Some(state) = self.state.lock().unwrap().as_mut()
            && state.target == source
            && state.session_id == session_id
            && state.revision == revision
            && state.paused == paused
        {
            state.acknowledged = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_commands_retry_and_stale_or_foreign_acks_cannot_override_resume() {
        let control = MediaPauseControl::default();
        let peer = "127.0.0.1:8123".parse().unwrap();
        assert!(!control.set_paused(true));
        control.begin_session(peer, 7);
        assert!(control.take_request().is_none());
        control.set_paused(true);
        let (request, _) = control.take_request().unwrap();
        assert!(matches!(
            request,
            ControlMessage::SetMediaPaused {
                revision: 1,
                paused: true,
                ..
            }
        ));
        assert!(control.take_request().is_none());
        control.state.lock().unwrap().as_mut().unwrap().last_sent = Some(Instant::now() - RETRY);
        assert!(control.take_request().is_some());
        control.set_paused(false);
        control.acknowledge(peer, 7, 1, true);
        control.acknowledge("127.0.0.1:8124".parse().unwrap(), 7, 2, false);
        control.acknowledge(peer, 8, 2, false);
        assert!(control.is_pending());
        control.acknowledge(peer, 7, 2, false);
        assert!(!control.is_pending());
        assert!(control.retry_wait().is_none());
        control.end_session();
        assert!(control.take_request().is_none());
        assert!(!control.is_paused());
    }
}
