//! Loss is allowed; repeated, stale and unowned transitions are not.
//! The receiver consumes sequence numbers and ownership before native calls.
use crate::InputEvent;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_INPUT_AGE_MS: u64 = 250;
pub const INPUT_REFRESH_INTERVAL_MS: u64 = 100;
pub const HOLD_TIMEOUT_MS: u64 = 300;
pub const INPUT_WATCHDOG_INTERVAL_MS: u64 = 50;
pub const MAX_HELD: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputBinding {
    pub connection_id: u32,
    pub subscription_id: u32,
    pub source_revision: u32,
    pub epoch: [u8; 16],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Press {
    Key(u16),
    Button(u8),
    Modifier(u8),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InputPayload {
    Event(InputEvent),
    /// May release a known press; never synthesizes a missing press.
    Held(Vec<(Press, u64)>),
    Release,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealtimeInput {
    pub binding: InputBinding,
    pub sequence: u64,
    /// Host clock estimated from grant receipt, biased older by network delay.
    pub created_host_ms: u64,
    pub payload: InputPayload,
}

#[derive(Default, Debug)]
pub struct HeldState(BTreeMap<Press, u64>);
impl HeldState {
    pub fn snapshot(&self) -> Vec<(Press, u64)> {
        self.0.iter().map(|(p, s)| (*p, *s)).collect()
    }
    fn modifiers(&self) -> u8 {
        self.0.keys().fold(0, |mask, p| {
            if let Press::Modifier(bit) = p {
                mask | bit
            } else {
                mask
            }
        })
    }
    fn transition(&mut self, press: Press, down: bool, sequence: u64) -> bool {
        if down {
            if self.0.len() >= MAX_HELD || self.0.contains_key(&press) {
                return false;
            }
            self.0.insert(press, sequence);
            true
        } else {
            self.0.remove(&press).is_some()
        }
    }
    pub fn event(&mut self, event: InputEvent, sequence: u64) -> Option<InputEvent> {
        // Scancode and structured-key messages share one physical press identity.
        let event = match event {
            InputEvent::KeyDown(code) if code <= 0x7e => InputEvent::Key {
                key_code: code as u16,
                pressed: true,
                modifiers: 0,
            },
            InputEvent::KeyUp(code) if code <= 0x7e => InputEvent::Key {
                key_code: code as u16,
                pressed: false,
                modifiers: 0,
            },
            other => other,
        };
        let event = if let InputEvent::Key {
            key_code, pressed, ..
        } = event
        {
            let bit = match key_code {
                0x38 | 0x3c => 1,
                0x3b | 0x3e => 2,
                0x3a | 0x3d => 4,
                0x37 | 0x36 => 8,
                _ => 0,
            };
            if bit != 0 {
                if (self.modifiers() & bit != 0) == pressed {
                    return None;
                }
                InputEvent::ModifiersChanged(if pressed {
                    self.modifiers() | bit
                } else {
                    self.modifiers() & !bit
                })
            } else {
                event
            }
        } else {
            event
        };
        let valid = match &event {
            InputEvent::KeyDown(k) if *k <= 0x7e => {
                self.transition(Press::Key(*k as u16), true, sequence)
            }
            InputEvent::KeyUp(k) if *k <= 0x7e => {
                self.transition(Press::Key(*k as u16), false, sequence)
            }
            InputEvent::Key {
                key_code, pressed, ..
            } if *key_code <= 0x7e => self.transition(Press::Key(*key_code), *pressed, sequence),
            InputEvent::KeyDown(_) | InputEvent::KeyUp(_) | InputEvent::Key { .. } => false,
            InputEvent::MouseDown(b) if *b <= 2 => {
                self.transition(Press::Button(*b), true, sequence)
            }
            InputEvent::MouseUp(b) if *b <= 2 => {
                self.transition(Press::Button(*b), false, sequence)
            }
            InputEvent::MouseDown(_) | InputEvent::MouseUp(_) | InputEvent::Touch { .. } => false,
            InputEvent::ModifiersChanged(mask) => {
                // Caps Lock is a toggle, not a releasable momentary press.
                for bit in [1, 2, 4, 8] {
                    if mask & bit != 0 {
                        self.transition(Press::Modifier(bit), true, sequence);
                    } else {
                        self.transition(Press::Modifier(bit), false, sequence);
                    }
                }
                true
            }
            _ => true,
        };
        if !valid {
            return None;
        }
        Some(match event {
            // Key-up never introduces modifiers. Only explicit modifier intent
            // creates a modifier press, and only owned flags reach native APIs.
            InputEvent::Key {
                key_code, pressed, ..
            } => InputEvent::Key {
                key_code,
                pressed,
                modifiers: self.modifiers(),
            },
            InputEvent::ModifiersChanged(_) => InputEvent::ModifiersChanged(self.modifiers()),
            InputEvent::KeyDown(code) | InputEvent::KeyUp(code) => InputEvent::Key {
                key_code: code as u16,
                pressed: matches!(event, InputEvent::KeyDown(_)),
                modifiers: self.modifiers(),
            },
            other => other,
        })
    }
    fn release_missing(&mut self, held: &[(Press, u64)]) -> Vec<InputEvent> {
        let removed: Vec<_> = self
            .0
            .iter()
            .filter(|(p, s)| !held.contains(&(**p, **s)))
            .map(|(p, _)| *p)
            .collect();
        let mut output = Vec::new();
        let mut modifiers_changed = false;
        for press in removed {
            self.0.remove(&press);
            match press {
                Press::Key(key_code) => output.push(InputEvent::Key {
                    key_code,
                    pressed: false,
                    modifiers: self.modifiers(),
                }),
                Press::Button(b) => output.push(InputEvent::MouseUp(b)),
                Press::Modifier(_) => modifiers_changed = true,
            }
        }
        if modifiers_changed {
            output.push(InputEvent::ModifiersChanged(self.modifiers()));
        }
        output
    }
}

#[derive(Debug)]
pub struct InputLease {
    pub binding: InputBinding,
    high_water: u64,
    last_valid_ms: u64,
    held: HeldState,
    last_valid: std::time::Instant,
    last_held_ms: u64,
    last_held: std::time::Instant,
}
impl InputLease {
    pub fn new(binding: InputBinding, now_ms: u64) -> Self {
        Self {
            binding,
            high_water: 0,
            last_valid_ms: now_ms,
            held: HeldState::default(),
            last_valid: std::time::Instant::now(),
            last_held_ms: now_ms,
            last_held: std::time::Instant::now(),
        }
    }
    pub fn held_modifiers(&self) -> u8 {
        self.held.modifiers()
    }
    pub fn expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_valid_ms) >= HOLD_TIMEOUT_MS
            || self.last_valid.elapsed().as_millis() >= HOLD_TIMEOUT_MS as u128
            || (!self.held.0.is_empty()
                && (now_ms.saturating_sub(self.last_held_ms) >= HOLD_TIMEOUT_MS
                    || self.last_held.elapsed().as_millis() >= HOLD_TIMEOUT_MS as u128))
    }
    pub fn accept(&mut self, packet: &RealtimeInput, now_ms: u64) -> Option<Vec<InputEvent>> {
        if packet.binding != self.binding
            || packet.sequence <= self.high_water
            || self.expired(now_ms)
        {
            return None;
        }
        // Consume even a stale/invalid sequence. An unknown native outcome is
        // never retried, and a late lower sequence cannot restore old intent.
        self.high_water = packet.sequence;
        if packet.created_host_ms > now_ms || now_ms - packet.created_host_ms > MAX_INPUT_AGE_MS {
            return None;
        }
        // An explicit chord must not become a different (possibly bare) key
        // when its modifier-down was lost/reordered. Consume the sequence but
        // reject before changing held state. Legacy KeyDown has no such field.
        if let InputPayload::Event(InputEvent::Key { pressed: true, modifiers, .. }) = &packet.payload
            && *modifiers != self.held.modifiers()
        {
            return None;
        }
        let had_held = !self.held.0.is_empty();
        let output = match &packet.payload {
            InputPayload::Event(event) => self
                .held
                .event(event.clone(), packet.sequence)
                .into_iter()
                .collect(),
            InputPayload::Held(held) if held.len() <= MAX_HELD => self.held.release_missing(held),
            InputPayload::Held(_) => return None,
            InputPayload::Release => self.held.release_missing(&[]),
        };
        let received = std::time::Instant::now();
        let created = received.checked_sub(std::time::Duration::from_millis(now_ms - packet.created_host_ms))
            .unwrap_or(self.last_valid);
        self.last_valid_ms = packet.created_host_ms;
        self.last_valid = created;
        // Start a hold budget on the first accepted press. Only explicit held
        // snapshots refresh existing presses: motion, new unrelated events,
        // grants and transport heartbeats cannot keep an old press alive.
        // Account for packet age rather than granting a new timeout at receipt.
        if !self.held.0.is_empty() && (!had_held || matches!(packet.payload, InputPayload::Held(_))) {
            self.last_held_ms = packet.created_host_ms;
            self.last_held = created;
        }
        Some(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn alternate_key_encodings_and_modifier_keys_share_one_press_identity() {
        let mut held = HeldState::default();
        assert!(held.event(InputEvent::KeyDown(9), 1).is_some());
        assert!(
            held.event(
                InputEvent::Key {
                    key_code: 9,
                    pressed: true,
                    modifiers: 0
                },
                2
            )
            .is_none()
        );
        assert!(held.event(InputEvent::ModifiersChanged(1), 3).is_some());
        assert!(held.event(InputEvent::KeyDown(0x38), 4).is_none());
        assert!(held.event(InputEvent::KeyUp(0x38), 5).is_some());
        assert!(held.event(InputEvent::KeyUp(0x38), 6).is_none());
    }
    fn binding(epoch: u8) -> InputBinding {
        InputBinding {
            connection_id: 1,
            subscription_id: 7,
            source_revision: 3,
            epoch: [epoch; 16],
        }
    }
    fn packet(seq: u64, payload: InputPayload) -> RealtimeInput {
        RealtimeInput {
            binding: binding(1),
            sequence: seq,
            created_host_ms: 1000,
            payload,
        }
    }
    #[test]
    fn duplicates_out_of_order_and_unknown_execution_are_consumed() {
        let mut lease = InputLease::new(binding(1), 1000);
        let down = packet(2, InputPayload::Event(InputEvent::MouseDown(0)));
        assert_eq!(
            lease.accept(&down, 1000).unwrap(),
            vec![InputEvent::MouseDown(0)]
        );
        // Simulate an unknown native outcome: callers do not roll back accept.
        assert!(lease.accept(&down, 1001).is_none());
        assert!(
            lease
                .accept(
                    &packet(1, InputPayload::Event(InputEvent::MouseUp(0))),
                    1001
                )
                .is_none()
        );
        assert!(
            lease
                .accept(
                    &packet(3, InputPayload::Event(InputEvent::MouseDown(0))),
                    1001
                )
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn stale_future_and_timed_out_events_never_inject() {
        let mut lease = InputLease::new(binding(1), 1000);
        assert!(
            lease
                .accept(
                    &packet(1, InputPayload::Event(InputEvent::KeyDown(9))),
                    1251
                )
                .is_none()
        );
        assert!(
            lease
                .accept(&packet(2, InputPayload::Event(InputEvent::KeyDown(9))), 999)
                .is_none()
        );
        assert!(lease.expired(1751));
        assert!(
            lease
                .accept(
                    &packet(3, InputPayload::Event(InputEvent::KeyDown(9))),
                    1751
                )
                .is_none()
        );
    }
    #[test]
    fn reconnect_source_switch_and_old_release_cannot_touch_new_epoch() {
        let mut lease = InputLease::new(binding(2), 1000);
        let old = packet(90, InputPayload::Release);
        assert!(lease.accept(&old, 1000).is_none());
        let mut down = packet(1, InputPayload::Event(InputEvent::MouseDown(0)));
        down.binding = binding(2);
        assert_eq!(lease.accept(&down, 1000).unwrap().len(), 1);
        assert!(lease.accept(&old, 1000).is_none());
        let mut switched = down.clone();
        switched.binding.source_revision = 4;
        switched.sequence = 2;
        assert!(lease.accept(&switched, 1000).is_none());
    }
    #[test]
    fn lost_up_snapshot_releases_only_owned_press_and_never_creates_down() {
        let mut lease = InputLease::new(binding(1), 1000);
        lease
            .accept(
                &packet(1, InputPayload::Event(InputEvent::KeyDown(9))),
                1000,
            )
            .unwrap();
        let snapshot = packet(2, InputPayload::Held(vec![(Press::Key(10), 1)]));
        assert_eq!(
            lease.accept(&snapshot, 1000).unwrap(),
            vec![InputEvent::Key {
                key_code: 9,
                pressed: false,
                modifiers: 0
            }]
        );
        assert!(
            lease
                .accept(&packet(3, InputPayload::Release), 1000)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn late_snapshot_cannot_release_a_new_press_or_resurrect_an_old_one() {
        let mut lease = InputLease::new(binding(1), 1000);
        for (s, e) in [
            (1, InputEvent::MouseDown(0)),
            (2, InputEvent::MouseUp(0)),
            (4, InputEvent::MouseDown(0)),
        ] {
            lease
                .accept(&packet(s, InputPayload::Event(e)), 1000)
                .unwrap();
        }
        assert!(
            lease
                .accept(&packet(3, InputPayload::Held(vec![])), 1000)
                .is_none()
        );
        assert!(
            lease
                .accept(
                    &packet(5, InputPayload::Held(vec![(Press::Button(0), 4)])),
                    1000
                )
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn continuous_hold_only_matching_fresh_snapshots_extend_pressed_state() {
        let mut lease = InputLease::new(binding(1), 1000);
        lease.accept(&packet(1, InputPayload::Event(InputEvent::KeyDown(0))), 1000).unwrap();
        lease.accept(&packet(2, InputPayload::Event(InputEvent::MouseDown(0))), 1000).unwrap();
        let held = vec![(Press::Key(0), 1), (Press::Button(0), 2)];
        for (seq, now) in [(3, 1100), (4, 1200), (6, 1400)] {
            let mut refresh = packet(seq, InputPayload::Held(held.clone()));
            refresh.created_host_ms = now;
            assert!(lease.accept(&refresh, now).unwrap().is_empty());
        }
        // A lost 1300ms snapshot is tolerated; duplicate, old epoch and stale
        // packets do not renew, and no snapshot replays a key/button-down.
        let mut duplicate = packet(6, InputPayload::Held(held.clone()));
        duplicate.created_host_ms = 1600;
        assert!(lease.accept(&duplicate, 1600).is_none());
        let mut old_epoch = packet(7, InputPayload::Held(held.clone()));
        old_epoch.binding.epoch = [99; 16]; old_epoch.created_host_ms = 1600;
        assert!(lease.accept(&old_epoch, 1600).is_none());
        assert!(lease.accept(&packet(8, InputPayload::Held(held.clone())), 1600).is_none());
        assert!(!lease.expired(1699));
        assert!(lease.expired(1700));
        let mut late = packet(9, InputPayload::Held(held)); late.created_host_ms = 1700;
        assert!(lease.accept(&late, 1700).is_none());
        let mut fresh = InputLease::new(binding(2), 1700);
        late.binding = binding(2);
        assert!(fresh.accept(&late, 1700).unwrap().is_empty());
        assert!(fresh.held.snapshot().is_empty()); // No automatic restart.
    }

    #[test]
    fn continuous_hold_motion_does_not_renew_and_delayed_snapshot_keeps_its_age() {
        let mut lease = InputLease::new(binding(1), 1000);
        lease.accept(&packet(1, InputPayload::Event(InputEvent::MouseDown(0))), 1000).unwrap();
        let mut motion = packet(2, InputPayload::Event(InputEvent::MouseMove { dx: 1, dy: 0 }));
        motion.created_host_ms = 1200;
        assert!(lease.accept(&motion, 1200).is_some());
        assert!(lease.expired(1300));
        let mut lease = InputLease::new(binding(1), 1000);
        lease.accept(&packet(1, InputPayload::Event(InputEvent::MouseDown(0))), 1000).unwrap();
        let mut refresh = packet(2, InputPayload::Held(vec![(Press::Button(0), 1)]));
        refresh.created_host_ms = 1050;
        assert!(lease.accept(&refresh, 1250).unwrap().is_empty());
        assert!(!lease.expired(1349));
        assert!(lease.expired(1350)); // Not 1250 + 300.
        // Polling adds at most one nominal interval; a delayed executor stops
        // on its first resumed poll, never re-authorizing expired intent.
        for scheduling_delay in [0, 20, 200] {
            let poll = 1350 + INPUT_WATCHDOG_INTERVAL_MS + scheduling_delay;
            assert!(lease.expired(poll));
        }
    }

    #[test]
    fn modifier_intent_lost_or_reordered_down_never_becomes_bare_character() {
        let mut lease = InputLease::new(binding(1), 1000);
        let chord = InputPayload::Event(InputEvent::Key {
            key_code: 0x08, pressed: true, modifiers: 2,
        });
        assert!(lease.accept(&packet(2, chord.clone()), 1000).is_none());
        assert!(lease.accept(&packet(1, InputPayload::Event(InputEvent::ModifiersChanged(2))), 1000).is_none());
        assert_eq!(lease.accept(&packet(3, InputPayload::Event(InputEvent::ModifiersChanged(2))), 1000),
            Some(vec![InputEvent::ModifiersChanged(2)]));
        assert!(lease.accept(&packet(2, chord.clone()), 1000).is_none());
        // Rejected C never entered held state: a fresh deliberate C can succeed.
        assert_eq!(lease.accept(&packet(4, chord), 1000), Some(vec![InputEvent::Key {
            key_code: 0x08, pressed: true, modifiers: 2,
        }]));
    }

    #[test]
    fn modifier_intent_extra_held_modifier_drops_explicit_plain_key_but_legacy_keydown_remains() {
        let mut lease = InputLease::new(binding(1), 1000);
        lease.accept(&packet(1, InputPayload::Event(InputEvent::ModifiersChanged(2))), 1000).unwrap();
        assert!(lease.accept(&packet(2, InputPayload::Event(InputEvent::Key {
            key_code: 0x08, pressed: true, modifiers: 0,
        })), 1000).is_none());
        assert_eq!(lease.accept(&packet(3, InputPayload::Event(InputEvent::KeyDown(0x08))), 1000),
            Some(vec![InputEvent::Key { key_code: 0x08, pressed: true, modifiers: 2 }]));
        assert_eq!(lease.accept(&packet(4, InputPayload::Event(InputEvent::Key {
            key_code: 0x08, pressed: false, modifiers: 15,
        })), 1000), Some(vec![InputEvent::Key { key_code: 0x08, pressed: false, modifiers: 2 }]));
    }

    #[test]
    fn unowned_key_up_does_not_introduce_modifiers() {
        let mut lease = InputLease::new(binding(1), 1000);
        assert!(
            lease
                .accept(
                    &packet(
                        1,
                        InputPayload::Event(InputEvent::Key {
                            key_code: 9,
                            pressed: false,
                            modifiers: 15
                        })
                    ),
                    1000
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            lease
                .accept(
                    &packet(
                        2,
                        InputPayload::Event(InputEvent::Key {
                            key_code: 9,
                            pressed: true,
                            modifiers: 15
                        })
                    ),
                    1000
                )
                .is_none(),
            true
        );
    }
}
