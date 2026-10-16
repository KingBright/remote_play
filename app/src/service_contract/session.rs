//! Executable contract for the next service-owned viewer integration.
//!
//! NOT wired to WorkspaceConnection or GUI Drop yet. Effects describe adapter
//! obligations, not performed I/O or a grant of input/capture permissions.
//!
//! Integration requirement: one service actor owns this model AND its channel.
//! It must drain every returned effect in order before processing another model
//! command, retain the exact scoped input lease for releases, and never enqueue
//! these unversioned effects onto a replacement channel. Async presentation work
//! needs its own generation fence. This model is not an async effect queue.
use remote_core::media_demand::{ConsumerToken, MediaConsumers, MediaDemand};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub service_generation: String,
    pub connection_id: u32,
    pub channel_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsumerKind {
    Gui,
    Background,
}

/// Scope + per-session consumer generation fence stale callbacks. Not a secret
/// or authentication credential; transport trust must be checked separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    scope: Scope,
    slot: u64,
    consumer: ConsumerToken,
    kind: ConsumerKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    Open,
    Closing,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Revoke input eligibility and send all held key/button releases before
    /// stopping that frontend's heartbeat or changing its media demand.
    /// Teardown must use this old attachment's retained input binding, NOT
    /// accepts_callback (which is already false) or the current GUI's grant.
    ReleaseInput(Attachment),
    SetMediaDemand(MediaDemand),
    CloseChannel,
}

pub struct Session {
    scope: Scope,
    lifecycle: Lifecycle,
    consumers: MediaConsumers,
    attachments: BTreeMap<u64, Attachment>,
}

impl Session {
    /// Construct once per service generation + connection. Never reuse the
    /// scope for a replacement channel after teardown, even if an ID is reused.
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            lifecycle: Lifecycle::Open,
            consumers: MediaConsumers::default(),
            attachments: BTreeMap::new(),
        }
    }

    pub fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }
    pub fn demand(&self) -> MediaDemand {
        self.consumers.demand()
    }

    /// Checks freshness only. Input still requires the existing source binding,
    /// focus, paint, activity, permission and authentication checks.
    pub fn accepts_callback(&self, attachment: &Attachment) -> bool {
        self.lifecycle == Lifecycle::Open
            && self.scope == attachment.scope
            && self.attachments.get(&attachment.slot) == Some(attachment)
    }

    pub fn attach(
        &mut self,
        slot: u64,
        kind: ConsumerKind,
        demand: MediaDemand,
    ) -> Option<(Attachment, Vec<Effect>)> {
        if self.lifecycle != Lifecycle::Open {
            return None;
        }
        let before = self.demand();
        let consumer = self.consumers.attach(slot, demand)?;
        let attachment = Attachment {
            scope: self.scope.clone(),
            slot,
            consumer,
            kind,
        };
        let mut effects = Vec::new();
        if let Some(previous) = self.attachments.insert(slot, attachment.clone()) {
            if previous.kind == ConsumerKind::Gui {
                effects.push(Effect::ReleaseInput(previous));
            }
        }
        self.media_effect(before, &mut effects);
        Some((attachment, effects))
    }

    pub fn update(&mut self, attachment: &Attachment, demand: MediaDemand) -> Option<Vec<Effect>> {
        if !self.accepts_callback(attachment) {
            return None;
        }
        let before = self.demand();
        if !self.consumers.update(attachment.consumer, demand) {
            return None;
        }
        let mut effects = Vec::new();
        // Even if another consumer keeps aggregate video active, a hidden GUI
        // must release its held input before its activity/heartbeat can stop.
        if attachment.kind == ConsumerKind::Gui && !demand.video {
            effects.push(Effect::ReleaseInput(attachment.clone()));
        }
        self.media_effect(before, &mut effects);
        Some(effects)
    }

    /// GUI disconnect pauses only its A/V demand. Other consumers, channel,
    /// clipboard and file-transfer lifetimes are not implicitly destroyed.
    pub fn detach(&mut self, attachment: &Attachment) -> Option<Vec<Effect>> {
        if !self.accepts_callback(attachment) {
            return None;
        }
        let before = self.demand();
        if !self.consumers.detach(attachment.consumer) {
            return None;
        }
        self.attachments.remove(&attachment.slot);
        let mut effects = Vec::new();
        if attachment.kind == ConsumerKind::Gui {
            effects.push(Effect::ReleaseInput(attachment.clone()));
        }
        self.media_effect(before, &mut effects);
        Some(effects)
    }

    /// Used for explicit session close/ExitAll, owner loss or fatal transport
    /// failure. Closing is not proof that the remote channel has been released.
    pub fn begin_close(&mut self) -> Vec<Effect> {
        if self.lifecycle != Lifecycle::Open {
            return Vec::new();
        }
        self.lifecycle = Lifecycle::Closing;
        let before = self.demand();
        let mut effects = Vec::new();
        for attachment in std::mem::take(&mut self.attachments).into_values() {
            if attachment.kind == ConsumerKind::Gui {
                effects.push(Effect::ReleaseInput(attachment));
            }
        }
        self.consumers = MediaConsumers::default();
        self.media_effect(before, &mut effects);
        effects.push(Effect::CloseChannel);
        effects
    }

    pub fn confirm_closed(&mut self) -> bool {
        if self.lifecycle != Lifecycle::Closing {
            return false;
        }
        self.lifecycle = Lifecycle::Closed;
        true
    }

    fn media_effect(&self, before: MediaDemand, effects: &mut Vec<Effect>) {
        let after = self.demand();
        if before != after {
            effects.push(Effect::SetMediaDemand(after));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(generation: &str, id: u32) -> Session {
        Session::new(Scope {
            service_generation: generation.into(),
            connection_id: id,
            channel_generation: 1,
        })
    }

    #[test]
    fn last_gui_detach_releases_input_before_pausing_payload_without_closing_channel() {
        let mut session = session("service-1", 42);
        let (gui, _) = session
            .attach(1, ConsumerKind::Gui, MediaDemand::BOTH)
            .unwrap();
        assert_eq!(
            session.detach(&gui),
            Some(vec![
                Effect::ReleaseInput(gui.clone()),
                Effect::SetMediaDemand(MediaDemand::NONE)
            ])
        );
        assert_eq!(session.lifecycle(), Lifecycle::Open);
        assert!(!session.accepts_callback(&gui));
        assert_eq!(session.detach(&gui), None);
        let (new_gui, effects) = session
            .attach(1, ConsumerKind::Gui, MediaDemand::BOTH)
            .unwrap();
        assert_ne!(new_gui, gui);
        assert_eq!(effects, vec![Effect::SetMediaDemand(MediaDemand::BOTH)]);
        // No input-grant/replay effect accompanies media resumption.
    }

    #[test]
    fn remaining_consumer_keeps_its_audio_while_gui_video_pauses() {
        let mut session = session("service-1", 42);
        let audio = MediaDemand {
            video: false,
            audio: true,
        };
        let (worker, _) = session.attach(2, ConsumerKind::Background, audio).unwrap();
        let (gui, _) = session
            .attach(1, ConsumerKind::Gui, MediaDemand::BOTH)
            .unwrap();
        assert_eq!(
            session.detach(&gui),
            Some(vec![
                Effect::ReleaseInput(gui),
                Effect::SetMediaDemand(audio)
            ])
        );
        assert_eq!(session.demand(), audio);
        assert_eq!(
            session.detach(&worker),
            Some(vec![Effect::SetMediaDemand(MediaDemand::NONE)])
        );
        assert_eq!(session.lifecycle(), Lifecycle::Open);
    }

    #[test]
    fn replaced_gui_and_cross_service_or_session_callbacks_cannot_affect_current_attachment() {
        let mut one = session("service-1", 42);
        let (old, _) = one.attach(1, ConsumerKind::Gui, MediaDemand::BOTH).unwrap();
        let (new, effects) = one.attach(1, ConsumerKind::Gui, MediaDemand::BOTH).unwrap();
        assert_eq!(effects, vec![Effect::ReleaseInput(old.clone())]);
        assert_eq!(one.update(&old, MediaDemand::NONE), None);
        assert_eq!(one.detach(&old), None);
        assert!(one.accepts_callback(&new));
        for mut other in [
            session("service-2", 42),
            session("service-1", 43),
            Session::new(Scope {
                service_generation: "service-1".into(),
                connection_id: 42,
                channel_generation: 2,
            }),
        ] {
            other
                .attach(1, ConsumerKind::Gui, MediaDemand::BOTH)
                .unwrap();
            assert!(!other.accepts_callback(&new));
            assert_eq!(other.detach(&new), None);
            assert_eq!(other.update(&new, MediaDemand::NONE), None);
            assert_eq!(other.demand(), MediaDemand::BOTH);
        }
    }

    #[test]
    fn hiding_gui_releases_input_before_its_activity_stops_even_with_another_viewer() {
        let mut session = session("service-1", 42);
        let (gui, _) = session
            .attach(1, ConsumerKind::Gui, MediaDemand::BOTH)
            .unwrap();
        let (worker, _) = session
            .attach(2, ConsumerKind::Background, MediaDemand::BOTH)
            .unwrap();
        assert_eq!(
            session.update(&gui, MediaDemand::NONE),
            Some(vec![Effect::ReleaseInput(gui.clone())])
        );
        session.detach(&worker).unwrap();
        session.update(&gui, MediaDemand::BOTH).unwrap();
        assert_eq!(
            session.update(&gui, MediaDemand::NONE),
            Some(vec![
                Effect::ReleaseInput(gui),
                Effect::SetMediaDemand(MediaDemand::NONE)
            ])
        );
    }

    #[test]
    fn close_releases_before_channel_close_and_terminal_states_never_resume() {
        let mut session = session("service-1", 42);
        assert!(!session.confirm_closed());
        let (gui, _) = session
            .attach(1, ConsumerKind::Gui, MediaDemand::BOTH)
            .unwrap();
        assert_eq!(
            session.begin_close(),
            vec![
                Effect::ReleaseInput(gui.clone()),
                Effect::SetMediaDemand(MediaDemand::NONE),
                Effect::CloseChannel
            ]
        );
        assert_eq!(session.lifecycle(), Lifecycle::Closing);
        for _ in 0..2 {
            assert!(session.begin_close().is_empty());
            assert_eq!(session.detach(&gui), None);
            assert_eq!(session.update(&gui, MediaDemand::BOTH), None);
            assert!(!session.accepts_callback(&gui));
            assert!(
                session
                    .attach(1, ConsumerKind::Gui, MediaDemand::BOTH)
                    .is_none()
            );
            session.confirm_closed();
        }
        assert_eq!(session.lifecycle(), Lifecycle::Closed);
    }

    #[test]
    fn detach_then_exit_still_closes_idle_channel_once() {
        let mut session = session("service-1", 42);
        let (gui, _) = session
            .attach(1, ConsumerKind::Gui, MediaDemand::BOTH)
            .unwrap();
        session.detach(&gui).unwrap();
        assert_eq!(session.begin_close(), vec![Effect::CloseChannel]);
        assert!(session.begin_close().is_empty());
    }
}
