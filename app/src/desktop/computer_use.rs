//! Owner-local observe -> bounded action adapter, not a network authorization API.
//! Only a trusted owner may mint a receipt after checking its existing session.
//! The owner holds its session lock across current-state validation and dispatch.
//! No focus, permission, source grant or presentation requirement is relaxed here.
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

const MAX_OBSERVATION_AGE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    Observe,
    Control,
}

/// Normalized source coordinates, never desktop pixels or a default target.
/// Deliberately excludes held keys, dragging, modifiers and arbitrary batches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Click { x: u16, y: u16, button: u8 },
    KeyPress { key_code: u16 },
    Scroll { x: u16, y: u16, delta_x: i32, delta_y: i32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Move { x: u16, y: u16 },
    Button { button: u8, pressed: bool },
    Key { key_code: u16, pressed: bool },
    Scroll { delta_x: i32, delta_y: i32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rejection {
    NoSession,
    StaleSession,
    Unpaired,
    FrameUnavailable,
    InputNotAllowed,
    InputBusy,
    Expired,
    Consumed,
    InvalidAction,
    /// Only local queue admission is known, never remote execution or release.
    QueueRejected { release_queued: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Queued {
    pub events: usize,
}

/// Must retain the exact observed host input grant, including its epoch.
/// A failed send may already have updated local held state; always attempt an
/// exact-grant release. Never substitute the replacement source's grant.
pub(crate) trait InputPort {
    fn send(&mut self, event: Event) -> bool;
    fn release(&mut self) -> bool;
}

/// Process-local, non-serializable, single-use input receipt. Not an auth token.
/// Frame ownership preserves a snapshot only; it does not keep a channel alive.
pub(crate) struct Observation<C, Fence, Frame> {
    connection: Weak<C>,
    fence: Fence,
    frame: Arc<Frame>,
    access: Access,
    observed_at: Instant,
    consumed: bool,
}

impl<C, Fence: PartialEq, Frame> Observation<C, Fence, Frame> {
    pub(super) fn new(
        connection: &Arc<C>, fence: Fence, frame: Arc<Frame>,
        access: Access, observed_at: Instant,
    ) -> Self {
        Self {
            connection: Arc::downgrade(connection), fence, frame, access,
            observed_at, consumed: false,
        }
    }

    pub(crate) fn frame(&self) -> &Arc<Frame> { &self.frame }
    pub(super) fn fence(&self) -> &Fence { &self.fence }

    pub(super) fn invalidate(&mut self) { self.consumed = true; }

    /// Shared by dispatch and snapshot export. The owner keeps its session lock
    /// through validation and byte selection; a retained frame is not authority.
    pub(super) fn validate_current(
        &mut self, connection: &Arc<C>, current: &Fence, now: Instant,
    ) -> Result<(), Rejection> {
        if self.consumed { return Err(Rejection::Consumed); }
        if self.fence != *current || !self.connection.upgrade()
            .is_some_and(|old| Arc::ptr_eq(&old, connection)) {
            self.consumed = true;
            return Err(Rejection::StaleSession);
        }
        if now.checked_duration_since(self.observed_at)
            .is_none_or(|age| age > MAX_OBSERVATION_AGE) {
            self.consumed = true;
            return Err(Rejection::Expired);
        }
        Ok(())
    }

    pub(super) fn act(
        &mut self, connection: &Arc<C>, current: &Fence, now: Instant,
        input_allowed: bool, action: Action, port: &mut impl InputPort,
    ) -> Result<Queued, Rejection> {
        self.validate_current(connection, current, now)?;
        if self.access != Access::Control || !input_allowed {
            self.consumed = true;
            return Err(Rejection::InputNotAllowed);
        }
        let events = match action {
            Action::Click { x, y, button } if button <= 2 => vec![
                Event::Move { x, y }, Event::Button { button, pressed: true },
                Event::Button { button, pressed: false },
            ],
            Action::KeyPress { key_code } => vec![
                Event::Key { key_code, pressed: true },
                Event::Key { key_code, pressed: false },
            ],
            Action::Scroll { x, y, delta_x, delta_y }
                if delta_x.unsigned_abs() <= 1200 && delta_y.unsigned_abs() <= 1200 => vec![
                    Event::Move { x, y }, Event::Scroll { delta_x, delta_y },
                ],
            _ => return Err(Rejection::InvalidAction),
        };
        // Consume before the first effect; queue failure must not be retried
        // with this receipt, because some events may already have been accepted.
        self.consumed = true;
        let count = events.len();
        for event in events {
            if !port.send(event) {
                return Err(Rejection::QueueRejected { release_queued: port.release() });
            }
        }
        Ok(Queued { events: count })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Recorder { sent: Vec<Event>, calls: usize, fail_at: Option<usize>, releases: usize, release_ok: bool }
    impl InputPort for Recorder {
        fn send(&mut self, event: Event) -> bool {
            let call = self.calls; self.calls += 1;
            if self.fail_at == Some(call) { return false; }
            self.sent.push(event); true
        }
        fn release(&mut self) -> bool { self.releases += 1; self.release_ok }
    }
    fn observation(c: &Arc<u32>, access: Access, now: Instant) -> Observation<u32, [u64; 7], Vec<u8>> {
        Observation::new(c, [1; 7], Arc::new(vec![7, 8, 9]), access, now)
    }
    fn key() -> Action { Action::KeyPress { key_code: 12 } }
    #[test]
    fn observes_real_snapshot_without_retaining_the_connection() {
        let c = Arc::new(42); let o = observation(&c, Access::Observe, Instant::now());
        assert_eq!(o.frame().as_ref(), &[7, 8, 9]); assert_eq!(Arc::strong_count(&c), 1);
    }
    #[test]
    fn exact_session_click_dispatches_ordered_down_and_up_once() {
        let c = Arc::new(42); let now = Instant::now(); let mut o = observation(&c, Access::Control, now);
        let mut port = Recorder::default();
        let action = Action::Click { x: 123, y: 456, button: 0 };
        assert_eq!(o.act(&c, &[1; 7], now, true, action, &mut port), Ok(Queued { events: 3 }));
        assert_eq!(port.sent, [Event::Move { x: 123, y: 456 },
            Event::Button { button: 0, pressed: true }, Event::Button { button: 0, pressed: false }]);
        assert_eq!(o.act(&c, &[1; 7], now, true, action, &mut port), Err(Rejection::Consumed));
        assert_eq!(port.calls, 3);
    }
    #[test]
    fn replacement_allocation_with_same_connection_number_gets_no_input() {
        let c = Arc::new(42); let replacement = Arc::new(42); let now = Instant::now();
        let mut o = observation(&c, Access::Control, now); let mut port = Recorder::default();
        assert_eq!(o.act(&replacement, &[1; 7], now, true, key(), &mut port), Err(Rejection::StaleSession));
        assert!(port.sent.is_empty()); assert_eq!(port.releases, 0);
    }
    #[test]
    fn every_fence_component_rejects_before_sending() {
        let c = Arc::new(42); let now = Instant::now();
        // Production fence includes device, network, service/selection epochs,
        // subscription/source generation, recovery epoch and exact input grant.
        for i in 0..7 {
            let mut changed = [1; 7]; changed[i] = 2;
            let mut o = observation(&c, Access::Control, now); let mut port = Recorder::default();
            assert_eq!(o.act(&c, &changed, now, true, key(), &mut port), Err(Rejection::StaleSession));
            assert_eq!(port.calls, 0);
        }
    }
    #[test]
    fn rejection_cannot_revive_when_selection_returns() {
        let c = Arc::new(42); let now = Instant::now(); let mut o = observation(&c, Access::Control, now);
        let mut port = Recorder::default();
        assert_eq!(o.act(&c, &[2; 7], now, true, key(), &mut port), Err(Rejection::StaleSession));
        assert_eq!(o.act(&c, &[1; 7], now, true, key(), &mut port), Err(Rejection::Consumed));
        assert_eq!(port.calls, 0);
    }
    #[test]
    fn view_only_and_revoked_input_never_dispatch() {
        let c = Arc::new(42); let now = Instant::now();
        for (access, allowed) in [(Access::Observe, true), (Access::Control, false)] {
            let mut o = observation(&c, access, now); let mut port = Recorder::default();
            assert_eq!(o.act(&c, &[1; 7], now, allowed, key(), &mut port), Err(Rejection::InputNotAllowed));
            assert_eq!(port.calls, 0);
        }
    }
    #[test]
    fn expired_or_future_receipt_never_dispatches() {
        let c = Arc::new(42); let now = Instant::now();
        for observed in [now - Duration::from_secs(31), now + Duration::from_secs(1)] {
            let mut o = observation(&c, Access::Control, observed); let mut port = Recorder::default();
            assert_eq!(o.act(&c, &[1; 7], now, true, key(), &mut port), Err(Rejection::Expired));
            assert_eq!(port.calls, 0);
        }
    }
    #[test]
    fn partial_queue_failure_attempts_exact_release_and_forbids_retry() {
        let c = Arc::new(42); let now = Instant::now();
        for fail_at in 0..2 {
            for release_ok in [false, true] {
                let mut o = observation(&c, Access::Control, now);
                let mut port = Recorder { fail_at: Some(fail_at), release_ok, ..Recorder::default() };
                assert_eq!(o.act(&c, &[1; 7], now, true, key(), &mut port),
                    Err(Rejection::QueueRejected { release_queued: release_ok }));
                assert_eq!(port.releases, 1);
                assert_eq!(o.act(&c, &[1; 7], now, true, key(), &mut port), Err(Rejection::Consumed));
            }
        }
    }
    #[test]
    fn invalid_button_and_unbounded_scroll_have_no_effects() {
        let c = Arc::new(42); let now = Instant::now();
        for action in [Action::Click { x: 0, y: 0, button: 3 },
            Action::Scroll { x: 0, y: 0, delta_x: i32::MIN, delta_y: 0 }] {
            let mut o = observation(&c, Access::Control, now); let mut port = Recorder::default();
            assert_eq!(o.act(&c, &[1; 7], now, true, action, &mut port), Err(Rejection::InvalidAction));
            assert_eq!(port.calls, 0); assert_eq!(port.releases, 0);
        }
    }
    #[test]
    fn read_only_export_validation_neither_grants_control_nor_consumes_receipt() {
        let c = Arc::new(42); let now = Instant::now();
        let mut o = observation(&c, Access::Observe, now);
        assert_eq!(o.validate_current(&c, &[1; 7], now), Ok(()));
        assert_eq!(o.validate_current(&c, &[1; 7], now), Ok(()));
        let mut port = Recorder::default();
        assert_eq!(o.act(&c, &[1; 7], now, true, key(), &mut port), Err(Rejection::InputNotAllowed));
        assert!(port.sent.is_empty());
    }

    #[test]
    fn export_validation_rejects_expiry_and_changed_generation() {
        let c = Arc::new(42); let now = Instant::now();
        let mut expired = observation(&c, Access::Observe, now - Duration::from_secs(31));
        assert_eq!(expired.validate_current(&c, &[1; 7], now), Err(Rejection::Expired));
        let mut stale = observation(&c, Access::Observe, now);
        assert_eq!(stale.validate_current(&c, &[2; 7], now), Err(Rejection::StaleSession));
        assert_eq!(stale.validate_current(&c, &[1; 7], now), Err(Rejection::Consumed));
    }

    #[test]
    fn scroll_keeps_normalized_position_and_bounded_delta() {
        let c = Arc::new(42); let now = Instant::now(); let mut o = observation(&c, Access::Control, now);
        let mut port = Recorder::default();
        assert_eq!(o.act(&c, &[1; 7], now, true,
            Action::Scroll { x: u16::MAX, y: 0, delta_x: -3, delta_y: 9 }, &mut port),
            Ok(Queued { events: 2 }));
        assert_eq!(port.sent, [Event::Move { x: u16::MAX, y: 0 }, Event::Scroll { delta_x: -3, delta_y: 9 }]);
    }
}
