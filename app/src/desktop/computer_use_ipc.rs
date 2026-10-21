//! Same-user observation and bounded input bridge for the existing Octendra desktop authority.
//! Actor fields are not credentials: Octendra authenticates its task/executor;
//! private IPC authenticates the local user and the owner checks the exact source.
use super::{
    computer_use::Access,
    original_owner::{OriginalOwner, SourceViewBinding},
};
use protocol::session::CaptureSource;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Actor {
    pub owner: String,
    pub task_id: String,
    pub executor_device_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Target {
    pub peer_device_id: String,
    pub source: CaptureSource,
    pub connection_id: u32,
}
impl Target {
    pub fn desktop_id(&self) -> String {
        let source = match self.source {
            CaptureSource::MainDisplay => "main".into(),
            CaptureSource::Display(id) => format!("display:{id}"),
            CaptureSource::Window(id) => format!("window:{id}"),
        };
        format!(
            "weavelo:{}:{source}:{}",
            self.connection_id, self.peer_device_id
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Call {
    Open {
        actor: Actor,
        target: Target,
        lease_id: String,
        lease_ms: u32,
        max_bytes: usize,
        #[serde(default)]
        control: bool,
    },
    Observe {
        actor: Actor,
        lease_id: String,
    },
    Act {
        actor: Actor,
        lease_id: String,
        source_revision: u64,
        frame_sequence: u64,
        action: super::computer_use::Action,
    },
    Validate {
        actor: Actor,
        lease_id: String,
    },
    Close {
        actor: Actor,
        lease_id: String,
    },
}
struct Lease {
    caller: super::computer_use_authority::Caller,
    watchdog: tokio::task::JoinHandle<()>,
    scope: super::original_owner::ComputerUseObservation,
    actor: Actor,
    target: Target,
    id: String,
    expires: Instant,
    maximum: usize,
    sequence: u64,
    control: bool,
    pending: Option<PendingFrame<super::original_owner::ComputerUseObservation>>,
    binding: SourceViewBinding,
}
struct PendingFrame<T> {
    revision: u64,
    sequence: u64,
    observation: T,
}
fn take_frame<T>(
    pending: &mut Option<PendingFrame<T>>,
    revision: u64,
    sequence: u64,
) -> Result<T, &'static str> {
    if !pending
        .as_ref()
        .is_some_and(|p| p.revision == revision && p.sequence == sequence)
    {
        return Err("frame_mismatch_or_consumed");
    }
    Ok(pending.take().unwrap().observation)
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.watchdog.abort();
    }
}
pub(crate) struct Dispatcher(
    Mutex<Option<Lease>>,
    super::computer_use_authority::Authority,
);
pub(crate) struct Output {
    pub body: serde_json::Value,
    pub pixels: Option<Arc<Vec<u8>>>,
}
fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
        && value.bytes().any(|b| b != b'0' && b != b'-')
}
impl Call {
    pub fn identity(&self) -> (&Actor, &str) {
        match self {
            Self::Open {
                actor, lease_id, ..
            }
            | Self::Act {
                actor, lease_id, ..
            }
            | Self::Observe { actor, lease_id }
            | Self::Validate { actor, lease_id }
            | Self::Close { actor, lease_id } => (actor, lease_id),
        }
    }
    pub fn valid(&self) -> bool {
        let (actor, id) = match self {
            Self::Open {
                actor, lease_id, ..
            }
            | Self::Act {
                actor, lease_id, ..
            }
            | Self::Observe { actor, lease_id }
            | Self::Close { actor, lease_id } => (actor, lease_id),
            Self::Validate { actor, lease_id } => (actor, lease_id),
        };
        if actor.owner.is_empty()
            || actor.owner.len() > 128
            || actor.owner.chars().any(char::is_control)
            || !uuid(&actor.task_id)
            || !uuid(&actor.executor_device_id)
            || !uuid(id)
        {
            return false;
        }
        match self {
            Self::Open {
                target,
                lease_ms,
                max_bytes,
                ..
            } => {
                target.connection_id > 0
                    && !target.peer_device_id.is_empty()
                    && target.peer_device_id.len() <= 128
                    && target
                        .peer_device_id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
                    && (1..=30_000).contains(lease_ms)
                    && (1..=16 * 1024 * 1024).contains(max_bytes)
            }
            Self::Act {
                source_revision,
                frame_sequence,
                action,
                ..
            } => {
                *source_revision > 0
                    && *frame_sequence > 0
                    && match action {
                        super::computer_use::Action::Click { button, .. } => *button <= 2,
                        super::computer_use::Action::Scroll {
                            delta_x, delta_y, ..
                        } => delta_x.unsigned_abs() <= 1200 && delta_y.unsigned_abs() <= 1200,
                        // Explicit Mac key-space is approved by both authority records.
                        super::computer_use::Action::KeyPress { key_code } => {
                            *key_code < 128
                                && ![54, 55, 56, 57, 58, 59, 60, 61, 62, 63].contains(key_code)
                        }
                    }
            }
            _ => true,
        }
    }
}
impl Dispatcher {
    pub fn new(directory: &std::path::Path, generation: &str, hash: &str) -> Self {
        Self(
            Mutex::new(None),
            super::computer_use_authority::Authority::new(directory, generation, hash),
        )
    }
    pub fn configured(&self) -> bool {
        self.1.configured()
    }
    pub fn dispatch(
        self: &Arc<Self>,
        owner: &OriginalOwner,
        call: Call,
        peer: Option<i32>,
        cancelled: impl Fn() -> bool,
    ) -> Output {
        let result = (|| {
            if !call.valid() {
                return Err("invalid_request");
            }
            let saved = self
                .0
                .lock()
                .map_err(|_| "state_unavailable")?
                .as_ref()
                .map(|l| l.caller.clone());
            let caller = self
                .1
                .verify(&call, peer, saved.as_ref())
                .inspect_err(|_| {
                    // Observed revocation cannot be revived by restoring an authority file.
                    if saved.as_ref().is_some_and(|caller| caller.owns_peer(peer)) {
                        if let Ok(mut state) = self.0.lock() {
                            state.take();
                        }
                    }
                })?;
            let output = self.run(owner, call.clone(), caller.clone(), &cancelled)?;
            if output.pixels.is_some() {
                self.1.verify(&call, peer, Some(&caller))?;
            }
            Ok(output)
        })();
        result.unwrap_or_else(|code| Output {
            body: serde_json::json!({"ok":false,"code":code}),
            pixels: None,
        })
    }
    fn run(
        self: &Arc<Self>,
        owner: &OriginalOwner,
        call: Call,
        caller: super::computer_use_authority::Caller,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Output, &'static str> {
        if !call.valid() {
            return Err("invalid_request");
        }
        let mut state = self.0.lock().map_err(|_| "state_unavailable")?;
        if state.as_ref().is_some_and(|l| Instant::now() >= l.expires) {
            state.take();
        }
        match call {
            Call::Open {
                actor,
                target,
                lease_id,
                lease_ms,
                max_bytes,
                control,
            } => {
                if state.is_some() {
                    return Err("observer_busy");
                }
                let binding = owner
                    .computer_use_target_binding(
                        &target.peer_device_id,
                        target.source,
                        target.connection_id,
                    )
                    .ok_or("target_not_selected")?;
                let mut scope = owner
                    .observe_bound_computer_use(
                        &binding,
                        &target.peer_device_id,
                        target.source,
                        Access::Observe,
                    )
                    .map_err(|_| "session_unavailable")?;
                owner
                    .validate_bound_computer_use(&mut scope)
                    .map_err(|_| "frame_expired")?;
                let expires = Instant::now() + Duration::from_millis(lease_ms.into());
                let weak = Arc::downgrade(self);
                let key = lease_id.clone();
                let watchdog = tokio::spawn(async move {
                    tokio::time::sleep_until(tokio::time::Instant::from_std(expires)).await;
                    if let Some(dispatcher) = weak.upgrade() {
                        if let Ok(mut state) = dispatcher.0.lock() {
                            if state
                                .as_ref()
                                .is_some_and(|l| l.id == key && l.expires == expires)
                            {
                                state.take();
                            }
                        }
                    }
                });
                *state = Some(Lease {
                    caller,
                    watchdog,
                    scope,
                    actor,
                    target,
                    id: lease_id.clone(),
                    expires,
                    maximum: max_bytes,
                    sequence: 0,
                    control,
                    pending: None,
                    binding,
                });
                Ok(Output {
                    body: serde_json::json!({"ok":true,"lease_id":lease_id,"lease_ms":lease_ms}),
                    pixels: None,
                })
            }
            Call::Close { actor, lease_id } => {
                if state.is_some()
                    && !state
                        .as_ref()
                        .is_some_and(|l| l.actor == actor && l.id == lease_id)
                {
                    return Err("lease_mismatch");
                }
                state.take();
                Ok(Output {
                    body: serde_json::json!({"ok":true}),
                    pixels: None,
                })
            }
            Call::Act {
                actor,
                lease_id,
                source_revision,
                frame_sequence,
                action,
            } => {
                let lease = state
                    .as_mut()
                    .filter(|l| l.actor == actor && l.id == lease_id && l.control)
                    .ok_or("control_lease_mismatch")?;
                owner
                    .validate_bound_computer_use(&mut lease.scope)
                    .map_err(|_| "source_changed")?;
                let mut observation =
                    take_frame(&mut lease.pending, source_revision, frame_sequence)?;
                let remaining = caller.expires_at_ms.saturating_sub(crate::unix_now_ms());
                let deadline = lease
                    .expires
                    .min(Instant::now() + Duration::from_millis(remaining));
                // Retire the entire lease before any effect. The owner revokes its
                // epoch on dispatch, so refreshing the old lease would revive a fence.
                // A new gesture requires close/open/observe through existing authority.
                state.take();
                owner
                    .act_bound_computer_use(&mut observation, action, deadline, cancelled)
                    .map_err(|error| match error {
                        super::computer_use::Rejection::QueueRejected { .. } => "outcome_unknown",
                        _ => "input_rejected",
                    })?;
                Ok(Output {
                    body: serde_json::json!({"ok":true,"effect_confirmed":false}),
                    pixels: None,
                })
            }
            Call::Validate { actor, lease_id } => {
                let lease = state
                    .as_mut()
                    .filter(|l| l.actor == actor && l.id == lease_id)
                    .ok_or("lease_mismatch")?;
                owner
                    .validate_bound_computer_use(&mut lease.scope)
                    .map_err(|_| "source_changed")?;
                Ok(Output {
                    body: serde_json::json!({"ok":true}),
                    pixels: None,
                })
            }
            Call::Observe { actor, lease_id } => {
                let lease = state
                    .as_mut()
                    .filter(|l| l.actor == actor && l.id == lease_id)
                    .ok_or("lease_mismatch")?;
                owner
                    .validate_bound_computer_use(&mut lease.scope)
                    .map_err(|_| "source_changed")?;
                lease.pending.take();
                let (binding, target, maximum, control) = (
                    lease.binding.clone(),
                    lease.target.clone(),
                    lease.maximum,
                    lease.control,
                );
                drop(state);
                let mut observation = owner
                    .observe_bound_computer_use(
                        &binding,
                        &target.peer_device_id,
                        target.source,
                        if control {
                            Access::Control
                        } else {
                            Access::Observe
                        },
                    )
                    .map_err(|_| "session_unavailable")?;
                let revision = OriginalOwner::computer_use_source_revision(&observation);
                let pixels = owner
                    .export_bound_computer_use(&mut observation, maximum)
                    .map_err(|_| "pixels_unavailable")?;
                let mut state = self.0.lock().map_err(|_| "state_unavailable")?;
                let lease = state
                    .as_mut()
                    .filter(|l| l.actor == actor && l.id == lease_id && Instant::now() < l.expires)
                    .ok_or("lease_expired")?;
                owner
                    .validate_bound_computer_use(&mut lease.scope)
                    .map_err(|_| "source_changed")?;
                lease.sequence = lease.sequence.checked_add(1).ok_or("sequence_exhausted")?;
                lease.pending = Some(PendingFrame {
                    revision,
                    sequence: lease.sequence,
                    observation,
                });
                let body = serde_json::json!({"ok":true,"lease_id":lease_id,"source_revision":revision,
                "frame_sequence":lease.sequence,"size":[pixels.layout().width(),pixels.layout().height()],
                "bytes":pixels.layout().bytes(),"encoding":match pixels.color() {
                    client::observation_pixels::ColorMetadata::Srgb=>"rgba8_srgb",
                    client::observation_pixels::ColorMetadata::Unspecified=>"rgba8",
                }});
                Ok(Output {
                    body,
                    pixels: Some(pixels.into_pixels()),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_frame_receipt_is_exact_and_single_use() {
        let mut pending = Some(PendingFrame {
            revision: 7,
            sequence: 3,
            observation: "original",
        });
        assert!(take_frame(&mut pending, 8, 3).is_err());
        assert!(take_frame(&mut pending, 7, 4).is_err());
        assert_eq!(take_frame(&mut pending, 7, 3), Ok("original"));
        assert!(take_frame(&mut pending, 7, 3).is_err());
    }

    #[test]
    fn strict_actor_keeps_executor_distinct_from_peer_and_rejects_incomplete_calls() {
        let actor = Actor {
            owner: "fixture".into(),
            task_id: "11111111-1111-1111-1111-111111111111".into(),
            executor_device_id: "22222222-2222-2222-2222-222222222222".into(),
        };
        let call = Call::Open {
            actor: actor.clone(),
            target: Target {
                peer_device_id: "synthetic-weavelo-peer".into(),
                source: CaptureSource::Window(42),
                connection_id: 7,
            },
            lease_id: "33333333-3333-3333-3333-333333333333".into(),
            lease_ms: 30_000,
            max_bytes: 1024,
            control: false,
        };
        assert!(call.valid());
        let mut value = serde_json::to_value(call).unwrap();
        value["actor"]["executor_device_id"] = serde_json::json!("synthetic-weavelo-peer");
        assert!(!serde_json::from_value::<Call>(value).unwrap().valid());
        assert!(
            serde_json::from_str::<Call>(
                r#"{"operation":"observe","actor":{},"lease_id":"x","act":true}"#
            )
            .is_err()
        );
    }
}
