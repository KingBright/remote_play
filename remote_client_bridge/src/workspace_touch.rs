//! UI gestures stay local; only translated events with the current grant enter shared input.
use crate::touch_mapper::TouchStateTracker;
use protocol::{InputEvent, TouchAction, realtime_input::InputBinding};
use std::time::Instant;

pub(crate) struct WorkspaceTouch {
    pub revision: u32,
    supported: bool,
    visible: bool,
    surface: u64,
    attached: bool,
    frame_ready: bool,
    binding: Option<InputBinding>,
    last_intent: Option<Instant>,
    tracker: TouchStateTracker,
}

impl WorkspaceTouch {
    pub fn new() -> Self {
        Self {
            revision: 0,
            supported: false,
            visible: true,
            surface: 0,
            attached: false,
            frame_ready: false,
            binding: None,
            last_intent: None,
            tracker: TouchStateTracker::default(),
        }
    }
    pub fn cancel(&mut self) -> Option<InputBinding> {
        self.tracker.cancel_all();
        self.last_intent = None;
        self.binding.take()
    }
    pub fn expire(&mut self, now: Instant) -> Option<InputBinding> {
        if self.last_intent.is_some_and(|last| {
            now.saturating_duration_since(last).as_millis()
                >= protocol::realtime_input::HOLD_TIMEOUT_MS as u128
        }) {
            self.cancel()
        } else {
            None
        }
    }
    pub fn alive(&mut self, token: u64, now: Instant) -> Option<InputBinding> {
        let release = self.expire(now);
        // A transport heartbeat or a late UI callback cannot recreate a cancelled gesture.
        if token == self.surface
            && self.attached
            && self.frame_ready
            && self.visible
            && self.supported
            && self.binding.is_some()
            && self.tracker.has_active_pointers()
        {
            self.last_intent = Some(now);
        }
        release
    }
    /// An old surface's heartbeat cannot attest the replacement gesture.
    pub fn healthy_binding(&self,token:u64)->Option<InputBinding>{
        (token==self.surface&&self.attached&&self.frame_ready&&self.visible&&self.supported&&self.tracker.has_active_pointers()).then_some(self.binding).flatten()
    }
    pub fn invalidate(&mut self) -> Option<InputBinding> {
        self.attached = false;
        self.frame_ready = false;
        self.cancel()
    }
    pub fn support(&mut self, supported: bool) -> Option<InputBinding> {
        self.supported = supported;
        if supported { None } else { self.cancel() }
    }
    pub fn activity(&mut self, visible: bool) -> Option<InputBinding> {
        self.visible = visible;
        if visible { None } else { self.invalidate() }
    }
    pub fn surface(&mut self, token: u64, phase: &str) -> Option<InputBinding> {
        if phase == "begin" {
            if token == 0 || token <= self.surface {
                return None;
            }
            let release = self.cancel();
            self.surface = token;
            self.attached = self.visible;
            self.frame_ready = false;
            return release;
        }
        if token != self.surface || !self.attached {
            return None;
        }
        match phase {
            "ready" => {
                self.frame_ready = self.visible;
                None
            }
            "reset" => {
                self.frame_ready = false;
                self.cancel()
            }
            "end" => self.invalidate(),
            "cancel" => self.cancel(),
            _ => None,
        }
    }
    pub fn touch(
        &mut self,
        token: u64,
        event: InputEvent,
        current: Option<InputBinding>,
        now: Instant,
    ) -> (Option<InputBinding>, Vec<InputEvent>) {
        if token != self.surface || !self.attached {
            return (None, Vec::new());
        }
        if !self.visible || !self.frame_ready || !self.supported || current.is_none() {
            return (self.cancel(), Vec::new());
        }
        let expired = self.expire(now);
        let release = if self.binding != current {
            let release = self.cancel().or(expired);
            self.binding = current;
            release
        } else {
            expired
        };
        let InputEvent::Touch {
            action,
            pointer_id,
            normalized_x,
            normalized_y,
            pressure,
        } = event
        else {
            return (self.cancel().or(release), Vec::new());
        };
        if action == TouchAction::Cancel {
            return (self.cancel().or(release), Vec::new());
        }
        let events = self.tracker.process_touch(
            action,
            pointer_id,
            normalized_x,
            normalized_y,
            pressure,
            now,
        );
        self.last_intent = self.tracker.has_active_pointers().then_some(now);
        (release, events)
    }
    pub fn binding(&self) -> Option<InputBinding> {
        self.binding
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding(epoch: u8) -> InputBinding {
        InputBinding {
            connection_id: 4,
            subscription_id: 256,
            source_revision: 0,
            epoch: [epoch; 16],
        }
    }
    fn event(action: TouchAction, pointer_id: u32, x: f32) -> InputEvent {
        InputEvent::Touch {
            action,
            pointer_id,
            normalized_x: x,
            normalized_y: 0.5,
            pressure: 1.0,
        }
    }
    fn ready() -> WorkspaceTouch {
        let mut touch = WorkspaceTouch::new();
        touch.support(true);
        touch.surface(1, "begin");
        touch.surface(1, "ready");
        touch
    }
    #[test]
    fn current_frame_support_and_grant_are_all_required() {
        let mut touch = WorkspaceTouch::new();
        let now = Instant::now();
        touch.surface(1, "begin");
        for (supported, frame, grant) in [
            (false, true, Some(binding(1))),
            (true, false, Some(binding(1))),
            (true, true, None),
        ] {
            touch.support(supported);
            touch.surface(1, if frame { "ready" } else { "reset" });
            assert!(
                touch
                    .touch(1, event(TouchAction::Down, 7, 0.1), grant, now)
                    .1
                    .is_empty()
            );
        }
        touch.surface(1, "ready");
        assert!(
            !touch
                .touch(1, event(TouchAction::Down, 7, 0.1), Some(binding(1)), now)
                .1
                .is_empty()
        );
    }
    #[test]
    fn drag_is_translated_and_cancel_discards_the_tail() {
        let mut touch = ready();
        let now = Instant::now();
        let grant = Some(binding(1));
        let down = touch
            .touch(1, event(TouchAction::Down, 7, 0.1), grant, now)
            .1;
        let drag = touch
            .touch(1, event(TouchAction::Move, 7, 0.8), grant, now)
            .1;
        assert!(drag.contains(&InputEvent::MouseDown(0)));
        assert!(
            !down
                .iter()
                .chain(&drag)
                .any(|event| matches!(event, InputEvent::Touch { .. }))
        );
        assert_eq!(
            touch
                .touch(1, event(TouchAction::Cancel, 7, 0.8), grant, now)
                .0,
            grant
        );
        assert!(
            touch
                .touch(1, event(TouchAction::Up, 7, 0.8), grant, now)
                .1
                .is_empty()
        );
    }
    #[test]
    fn changed_epoch_cannot_complete_an_old_click_or_drag() {
        let mut touch = ready();
        let now = Instant::now();
        touch.touch(1, event(TouchAction::Down, 7, 0.1), Some(binding(1)), now);
        let (release, events) =
            touch.touch(1, event(TouchAction::Up, 7, 0.1), Some(binding(2)), now);
        assert_eq!(release, Some(binding(1)));
        assert!(events.is_empty());
        assert!(
            !touch
                .touch(1, event(TouchAction::Down, 8, 0.3), Some(binding(2)), now)
                .1
                .is_empty()
        );
    }
    #[test]
    fn hidden_destroyed_and_replaced_surfaces_reject_late_callbacks() {
        let mut touch = ready();
        let now = Instant::now();
        let grant = Some(binding(1));
        touch.touch(1, event(TouchAction::Down, 7, 0.1), grant, now);
        assert_eq!(touch.activity(false), grant);
        touch.activity(true);
        touch.surface(1, "ready");
        assert!(
            touch
                .touch(1, event(TouchAction::Up, 7, 0.1), grant, now)
                .1
                .is_empty()
        );
        touch.surface(2, "begin");
        touch.surface(2, "ready");
        touch.touch(2, event(TouchAction::Down, 8, 0.3), grant, now);
        touch.surface(1, "end");
        touch.surface(1, "begin");
        assert!(
            touch
                .touch(1, event(TouchAction::Up, 7, 0.1), grant, now)
                .1
                .is_empty()
        );
        assert!(
            !touch
                .touch(2, event(TouchAction::Up, 8, 0.3), grant, now)
                .1
                .is_empty()
        );
        touch.surface(2, "end");
        touch.surface(2, "ready");
        assert!(
            touch
                .touch(2, event(TouchAction::Down, 9, 0.3), grant, now)
                .1
                .is_empty()
        );
    }
    #[test]
    fn frozen_ui_expires_and_late_alive_or_gesture_tail_cannot_revive_hold() {
        use std::time::Duration;
        let mut touch = ready();
        let now = Instant::now();
        let grant = Some(binding(1));
        touch.touch(1, event(TouchAction::Down, 7, 0.1), grant, now);
        touch.touch(1, event(TouchAction::Move, 7, 0.8), grant, now);
        assert_eq!(touch.alive(1, now + Duration::from_millis(100)), None);
        assert_eq!(touch.expire(now + Duration::from_millis(399)), None);
        assert_eq!(touch.expire(now + Duration::from_millis(400)), grant);
        assert_eq!(touch.binding(), None);
        assert_eq!(touch.alive(1, now + Duration::from_millis(401)), None);
        assert_eq!(touch.binding(), None);
        assert!(
            touch
                .touch(
                    1,
                    event(TouchAction::Move, 7, 0.9),
                    grant,
                    now + Duration::from_millis(402)
                )
                .1
                .is_empty()
        );
        assert!(
            touch
                .touch(
                    1,
                    event(TouchAction::Up, 7, 0.9),
                    grant,
                    now + Duration::from_millis(403)
                )
                .1
                .is_empty()
        );
        assert!(
            !touch
                .touch(
                    1,
                    event(TouchAction::Down, 8, 0.2),
                    grant,
                    now + Duration::from_millis(404)
                )
                .1
                .is_empty()
        );
        // Even when the watchdog is delayed, a late heartbeat expires before refreshing.
        assert_eq!(touch.alive(1, now + Duration::from_millis(704)), grant);
        assert_eq!(touch.binding(), None);
    }
    #[test]
    fn pointers_and_panes_have_independent_ownership() {
        let mut first = ready();
        let mut second = ready();
        let now = Instant::now();
        let grant = Some(binding(1));
        first.touch(1, event(TouchAction::Down, 7, 0.1), grant, now);
        assert!(
            first
                .touch(1, event(TouchAction::Move, 8, 0.8), grant, now)
                .1
                .is_empty()
        );
        assert!(
            second
                .touch(1, event(TouchAction::Up, 7, 0.1), grant, now)
                .1
                .is_empty()
        );
        assert!(
            !first
                .touch(1, event(TouchAction::Up, 7, 0.1), grant, now)
                .1
                .is_empty()
        );
    }
    #[test]
    fn only_current_live_surface_can_refresh_a_held_binding(){
        let mut touch=ready();let now=Instant::now();let grant=Some(binding(1));
        touch.touch(1,event(TouchAction::Down,7,0.1),grant,now);touch.touch(1,event(TouchAction::Move,7,0.8),grant,now);
        touch.alive(1,now+std::time::Duration::from_millis(100));assert_eq!(touch.healthy_binding(1),grant);assert_eq!(touch.healthy_binding(2),None);
        touch.alive(1,now+std::time::Duration::from_millis(500));assert_eq!(touch.healthy_binding(1),None);
    }

}
