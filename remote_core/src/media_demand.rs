//! Local consumers of an existing media session. Never an authorization grant.
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MediaDemand {
    pub video: bool,
    pub audio: bool,
}
impl MediaDemand {
    pub const NONE: Self = Self { video: false, audio: false };
    pub const BOTH: Self = Self { video: true, audio: true };
    pub fn union(self, other: Self) -> Self {
        Self { video: self.video || other.video, audio: self.audio || other.audio }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsumerToken { slot: u64, generation: u64 }

/// Replacing a window/worker invalidates its old callbacks even if its slot is reused.
/// Each session owns one registry; an empty registry pauses payload, never transport.
#[derive(Default)]
pub struct MediaConsumers {
    next_generation: u64,
    consumers: BTreeMap<u64, (u64, MediaDemand)>,
}
impl MediaConsumers {
    pub fn attach(&mut self, slot: u64, demand: MediaDemand) -> Option<ConsumerToken> {
        if self.consumers.len() >= 64 && !self.consumers.contains_key(&slot) { return None; }
        let generation = self.next_generation.checked_add(1)?;
        self.next_generation = generation;
        self.consumers.insert(slot, (generation, demand));
        Some(ConsumerToken { slot, generation })
    }
    pub fn update(&mut self, token: ConsumerToken, demand: MediaDemand) -> bool {
        let Some((generation, current)) = self.consumers.get_mut(&token.slot) else { return false; };
        if *generation != token.generation { return false; }
        *current = demand;
        true
    }
    pub fn detach(&mut self, token: ConsumerToken) -> bool {
        if !self.consumers.get(&token.slot).is_some_and(|(g, _)| *g == token.generation) { return false; }
        self.consumers.remove(&token.slot);
        true
    }
    pub fn demand(&self) -> MediaDemand {
        self.consumers.values().fold(MediaDemand::NONE, |all, (_, d)| all.union(*d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_window_cannot_pause_another_consumer_and_audio_is_independent() {
        let mut consumers = MediaConsumers::default();
        let window = consumers.attach(1, MediaDemand::BOTH).unwrap();
        let viewer = consumers.attach(2, MediaDemand { video: true, audio: false }).unwrap();
        let player = consumers.attach(3, MediaDemand { video: false, audio: true }).unwrap();
        assert!(consumers.update(window, MediaDemand::NONE));
        assert_eq!(consumers.demand(), MediaDemand::BOTH);
        assert!(consumers.detach(viewer));
        assert_eq!(consumers.demand(), MediaDemand { video: false, audio: true });
        assert!(consumers.detach(player));
        assert_eq!(consumers.demand(), MediaDemand::NONE);
    }
    #[test]
    fn delayed_detach_or_update_cannot_modify_a_replacement() {
        let mut consumers = MediaConsumers::default();
        let old = consumers.attach(7, MediaDemand::BOTH).unwrap();
        let current = consumers.attach(7, MediaDemand::BOTH).unwrap();
        assert!(!consumers.update(old, MediaDemand::NONE));
        assert!(!consumers.detach(old));
        assert_eq!(consumers.demand(), MediaDemand::BOTH);
        assert!(consumers.detach(current));
        assert_eq!(consumers.demand(), MediaDemand::NONE);
    }
}
