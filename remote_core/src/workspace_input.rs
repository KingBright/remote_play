use protocol::realtime_input::{HeldState, InputBinding, InputPayload, RealtimeInput};
use protocol::session::SessionCommand;
use std::collections::HashMap;
use std::time::Instant;

#[derive(Default)]
pub(crate) struct InputState {
    slots: HashMap<u32, Slot>,
    next_request: u64,
}
struct Slot {
    revision: u32,
    request: u64,
    lease: Option<Lease>,
}
struct Lease {
    binding: InputBinding,
    host_ms: u64,
    received: Instant,
    sequence: u64,
    held: HeldState,
    release: Option<u64>,
}
impl Lease {
    fn packet(&mut self, payload: InputPayload) -> Option<RealtimeInput> {
        self.sequence = self.sequence.checked_add(1)?;
        Some(RealtimeInput {
            binding: self.binding,
            sequence: self.sequence,
            created_host_ms: self
                .host_ms
                .saturating_add(self.received.elapsed().as_millis() as u64),
            payload,
        })
    }
}
impl InputState {
    pub(crate) fn source(&mut self, id: u32, revision: u32) {
        self.slots.insert(
            id,
            Slot {
                revision,
                request: 0,
                lease: None,
            },
        );
    }
    pub(crate) fn remove(&mut self, id: u32) {
        self.slots.remove(&id);
    }
    pub(crate) fn ready(&self, id: u32, revision: u32) -> bool {
        self.slots.get(&id).is_some_and(|slot| {
            slot.revision == revision
                && slot
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.received.elapsed().as_millis() <= 750)
        })
    }
    pub(crate) fn grant(
        &mut self,
        connection: u32,
        binding: InputBinding,
        request: u64,
        host_ms: u64,
    ) {
        let Some(slot) = self.slots.get_mut(&binding.subscription_id) else {
            return;
        };
        if binding.connection_id != connection
            || slot.revision != binding.source_revision
            || slot.request != request
            || request == 0
        {
            return;
        }
        slot.request = 0;
        if let Some(lease) = &mut slot.lease
            && lease.binding == binding
        {
            lease.host_ms = host_ms;
            lease.received = Instant::now();
            return;
        }
        // Reconnect/source generation changes discard held intent, never replay it.
        slot.lease = Some(Lease {
            binding,
            host_ms,
            received: Instant::now(),
            sequence: 0,
            held: HeldState::default(),
            release: None,
        });
    }
    pub(crate) fn requests(&mut self, connection: u32) -> Vec<SessionCommand> {
        self.slots
            .iter_mut()
            .filter_map(|(id, slot)| {
                self.next_request = self.next_request.checked_add(1)?;
                slot.request = self.next_request;
                Some(SessionCommand::RequestInputBinding {
                    connection_id: connection,
                    id: *id,
                    source_revision: slot.revision,
                    request: slot.request,
                })
            })
            .collect()
    }
    pub(crate) fn snapshots(&mut self) -> Vec<RealtimeInput> {
        self.slots
            .values_mut()
            .filter_map(|slot| {
                let lease = slot.lease.as_mut()?;
                // An unresponsive grant path cannot keep old ownership alive forever.
                if lease.received.elapsed().as_millis() > 750 {
                    slot.lease = None;
                    return None;
                }
                lease.packet(InputPayload::Held(lease.held.snapshot()))
            })
            .collect()
    }
    pub(crate) fn seal(&mut self, command: SessionCommand) -> Result<SessionCommand, String> {
        let (id, revision, event, release) = match command {
            SessionCommand::Input { id, event } => (id, None, Some(event), false),
            SessionCommand::SourceInput {
                id,
                source_revision,
                event,
            } => (id, Some(source_revision), Some(event), false),
            SessionCommand::ReleaseInput { id } => (id, None, None, true),
            SessionCommand::SwitchSource {
                id,
                request_id,
                source,
            } => {
                self.source(id, request_id);
                return Ok(SessionCommand::SwitchSource {
                    id,
                    request_id,
                    source,
                });
            }
            SessionCommand::SetActivity {
                id,
                revision,
                video,
                audio,
            } => {
                if !video {
                    if let Some(slot) = self.slots.get_mut(&id) {
                        slot.lease = None;
                    }
                }
                return Ok(SessionCommand::SetActivity {
                    id,
                    revision,
                    video,
                    audio,
                });
            }
            SessionCommand::Unsubscribe { id } => {
                self.remove(id);
                return Ok(SessionCommand::Unsubscribe { id });
            }
            SessionCommand::RealtimeInput(_) => {
                return Err("raw input envelopes are not accepted by this connection".into());
            }
            other => return Ok(other),
        };
        let slot = self.slots.get_mut(&id).ok_or("input binding unavailable")?;
        if revision.is_some_and(|r| r != slot.revision) {
            return Err("input source changed".into());
        }
        let lease = slot.lease.as_mut().ok_or("input binding unavailable")?;
        if lease.received.elapsed().as_millis() > 750 {
            slot.lease = None;
            return Err("input binding expired".into());
        }
        let payload = if release {
            lease.held = HeldState::default();
            InputPayload::Release
        } else {
            let seq = lease
                .sequence
                .checked_add(1)
                .ok_or("input sequence exhausted")?;
            InputPayload::Event(
                lease
                    .held
                    .event(event.unwrap(), seq)
                    .ok_or("duplicate or unowned input transition")?,
            )
        };
        let packet = lease.packet(payload).ok_or("input sequence exhausted")?;
        if release {
            lease.release = Some(packet.sequence);
        }
        Ok(SessionCommand::RealtimeInput(packet))
    }
    pub(crate) fn release_ack(&mut self, binding: InputBinding, sequence: u64) -> bool {
        let Some(lease) = self
            .slots
            .get_mut(&binding.subscription_id)
            .and_then(|s| s.lease.as_mut())
        else {
            return false;
        };
        if lease.binding == binding && lease.release == Some(sequence) {
            lease.release = None;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn grant(state: &mut InputState, epoch: u8) {
        let SessionCommand::RequestInputBinding { request, .. } = state.requests(1).remove(0)
        else {
            panic!()
        };
        state.grant(
            1,
            InputBinding {
                connection_id: 1,
                subscription_id: 7,
                source_revision: 0,
                epoch: [epoch; 16],
            },
            request,
            1000,
        );
    }
    #[test]
    fn queued_input_is_stamped_before_switch_and_never_rebound() {
        let mut state = InputState::default();
        state.source(7, 0);
        grant(&mut state, 1);
        let SessionCommand::RealtimeInput(old) = state
            .seal(SessionCommand::Input {
                id: 7,
                event: protocol::InputEvent::MouseDown(0),
            })
            .unwrap()
        else {
            panic!()
        };
        state.source(7, 0);
        grant(&mut state, 2);
        let snapshot = state.snapshots().remove(0);
        assert_ne!(old.binding, snapshot.binding);
        let InputPayload::Held(held) = snapshot.payload else {
            panic!()
        };
        assert!(held.is_empty());
    }
    #[test]
    fn release_is_sent_once_and_ack_cannot_follow_reconnect() {
        let mut state = InputState::default();
        state.source(7, 0);
        grant(&mut state, 1);
        let SessionCommand::RealtimeInput(release) =
            state.seal(SessionCommand::ReleaseInput { id: 7 }).unwrap()
        else {
            panic!()
        };
        state.source(7, 0);
        grant(&mut state, 2);
        assert!(!state.release_ack(release.binding, release.sequence));
    }
}
