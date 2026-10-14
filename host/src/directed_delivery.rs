//! Bounded, in-memory directed intent. Wire identity always comes from the
//! authenticated host connection; a ticket is not a node-ID assertion.
use protocol::session::{DeliveryState, SessionCommand};
use remote_core::shared_files::ShareScope;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
pub const LEASE: Duration = Duration::from_secs(15);
const LIMIT: usize = 16;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Peer {
    pub addr: SocketAddr,
    pub connection: u32,
    pub nonce: [u8; 16],
    pub scope: ShareScope,
}
#[derive(Clone, Serialize, Deserialize)]
pub enum LocalIntent {
    Register {
        ticket: [u8; 32],
        target: String,
        generation: String,
        connection: u32,
        sequence: u64,
    },
    Renew {
        ticket: [u8; 32],
        generation: String,
        revision: u64,
    },
    Revoke {
        ticket: [u8; 32],
        generation: String,
    },
}
struct Ticket {
    target: String,
    generation: String,
    scope: ShareScope,
    _connection: u32,
    revision: u64,
    expires: Instant,
    claimed: Option<Peer>,
    cancel: tokio::sync::watch::Sender<bool>,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
    }
}
struct State {
    identity: Option<String>,
    tickets: HashMap<[u8; 32], Ticket>,
    incoming: HashMap<(Peer, u64), Arc<Incoming>>,
    source_generation: Option<String>,
    registration_high_water: u64,
}
pub struct Broker {
    state: Mutex<State>,
    tx: mpsc::Sender<Arc<Incoming>>,
}
pub struct Incoming {
    pub peer: Peer,
    pub request: u64,
    pub source: String,
    pub target: String,
    pub ticket: [u8; 32],
    inner: Mutex<IncomingState>,
}
struct IncomingState {
    revision: u64,
    expires: Instant,
    state: DeliveryState,
    reason: String,
}
impl Incoming {
    pub fn snapshot(&self) -> (u64, Instant, DeliveryState) {
        let s = self.inner.lock().unwrap();
        (s.revision, s.expires, s.state)
    }
    pub fn alive(&self) -> bool {
        let s = self.inner.lock().unwrap();
        Instant::now() < s.expires
            && matches!(
                s.state,
                DeliveryState::Pending | DeliveryState::Accepted | DeliveryState::Receiving
            )
    }
    pub fn finish(&self, state: DeliveryState, reason: &str) {
        let mut s = self.inner.lock().unwrap();
        if matches!(
            s.state,
            DeliveryState::Pending | DeliveryState::Accepted | DeliveryState::Receiving
        ) {
            s.state = state;
            s.reason = reason.into();
        }
    }
    fn receipt(&self) -> SessionCommand {
        let s = self.inner.lock().unwrap();
        receipt(self.request, s.revision, s.state, &s.reason)
    }
}
fn receipt(request_id: u64, revision: u64, state: DeliveryState, reason: &str) -> SessionCommand {
    SessionCommand::DeliveryReceipt {
        request_id,
        revision,
        state,
        reason: reason.into(),
    }
}
impl Broker {
    pub fn new() -> (Arc<Self>, mpsc::Receiver<Arc<Incoming>>) {
        let (tx, rx) = mpsc::channel(8);
        let broker = Arc::new(Self {
            state: Mutex::new(State {
                identity: None,
                tickets: HashMap::new(),
                incoming: HashMap::new(),
                source_generation: None,
                registration_high_water: 0,
            }),
            tx,
        });
        let weak = Arc::downgrade(&broker);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let Some(broker) = weak.upgrade() else {
                    break;
                };
                broker.expire_tickets(
                    Instant::now(),
                    remote_core::shared_files::current_share_scope(),
                );
            }
        });
        (broker, rx)
    }
    fn expire_tickets(&self, now: Instant, scope: ShareScope) {
        let mut s = self.state.lock().unwrap();
        let expired = s
            .tickets
            .iter()
            .filter(|(_, t)| now >= t.expires || t.scope != scope)
            .map(|(k, _)| *k)
            .collect::<Vec<_>>();
        for k in expired {
            s.tickets.remove(&k);
        }
    }
    pub fn set_identity(&self, id: String) {
        self.state.lock().unwrap().identity = Some(id);
    }
    /// Called only by the same-user service after it has verified the live GUI
    /// generation and binary under its serialized registration lock.
    pub fn verified_gui_generation(&self, generation: &str) {
        let mut s = self.state.lock().unwrap();
        if s.source_generation.as_deref() != Some(generation) {
            s.tickets.clear(); // Drop cancels old generation's exact resources first.
            s.source_generation = Some(generation.to_owned());
            s.registration_high_water = 0;
        }
    }
    pub fn local(&self, command: LocalIntent) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        let now = Instant::now();
        let scope = remote_core::shared_files::current_share_scope();
        match command {
            LocalIntent::Register {
                ticket,
                target,
                generation,
                connection,
                sequence,
            } => {
                register(
                    &mut s, ticket, target, generation, connection, sequence, scope, now,
                )?;
            }
            LocalIntent::Renew {
                ticket,
                generation,
                revision,
            } => {
                let t = s.tickets.get_mut(&ticket).ok_or("Directed intent ended")?;
                renew(t, &generation, revision, now, scope)?;
            }
            LocalIntent::Revoke { ticket, generation } => {
                if s.tickets
                    .get(&ticket)
                    .is_some_and(|t| t.generation == generation)
                {
                    s.tickets.remove(&ticket);
                }
            }
        }
        Ok(())
    }
    pub fn offer(
        &self,
        peer: Peer,
        request: u64,
        source: String,
        target: String,
        ticket: [u8; 32],
        revision: u64,
    ) -> SessionCommand {
        let mut s = self.state.lock().unwrap();
        let key = (peer, request);
        if let Some(offer) = s.incoming.get(&key) {
            if offer.source != source || offer.target != target || offer.ticket != ticket {
                return receipt(
                    request,
                    revision,
                    DeliveryState::Rejected,
                    "Request belongs to another intent",
                );
            }
            let mut state = offer.inner.lock().unwrap();
            if Instant::now() >= state.expires {
                state.state = DeliveryState::Cancelled;
            }
            if revision > state.revision
                && matches!(
                    state.state,
                    DeliveryState::Pending | DeliveryState::Accepted | DeliveryState::Receiving
                )
            {
                state.revision = revision;
                state.expires = Instant::now() + LEASE;
            }
            drop(state);
            return offer.receipt();
        }
        if peer.scope.is_none()
            || s.identity.as_deref() != Some(&target)
            || source.is_empty()
            || source.len() > 256
            || ticket == [0; 32]
            || request == 0
            || revision != 1
            || s.incoming.len() >= LIMIT
            || s.incoming.values().any(|o| o.alive())
        {
            return receipt(
                request,
                revision,
                DeliveryState::Rejected,
                "Directed delivery unavailable or invalid target",
            );
        }
        let offer = Arc::new(Incoming {
            peer,
            request,
            source,
            target,
            ticket,
            inner: Mutex::new(IncomingState {
                revision,
                expires: Instant::now() + LEASE,
                state: DeliveryState::Pending,
                reason: String::new(),
            }),
        });
        if self.tx.try_send(offer.clone()).is_err() {
            return receipt(request, revision, DeliveryState::Rejected, "Receiver busy");
        }
        s.incoming.insert(key, offer);
        receipt(request, revision, DeliveryState::Pending, "")
    }
    pub fn claim(
        &self,
        peer: Peer,
        request: u64,
        target: &str,
        ticket: [u8; 32],
    ) -> SessionCommand {
        self.claim_in_scope(
            peer,
            request,
            target,
            ticket,
            remote_core::shared_files::current_share_scope(),
        )
    }
    fn claim_in_scope(
        &self,
        peer: Peer,
        request: u64,
        target: &str,
        ticket: [u8; 32],
        scope: ShareScope,
    ) -> SessionCommand {
        let mut s = self.state.lock().unwrap();
        let accepted = peer.scope.is_some()
            && peer.scope == scope
            && s.tickets.get_mut(&ticket).is_some_and(|t| {
                if t.target != target
                    || t.scope != peer.scope
                    || Instant::now() >= t.expires
                    || t.claimed.is_some_and(|p| p != peer)
                {
                    return false;
                }
                t.claimed = Some(peer);
                true
            });
        receipt(
            request,
            0,
            if accepted {
                DeliveryState::Claimed
            } else {
                DeliveryState::Rejected
            },
            if accepted {
                ""
            } else {
                "Sending intent is unavailable"
            },
        )
    }
    pub fn cancel(&self, peer: Peer, request: u64) -> SessionCommand {
        if let Some(o) = self.state.lock().unwrap().incoming.get(&(peer, request)) {
            o.finish(DeliveryState::Cancelled, "Sender stopped");
        }
        receipt(request, 0, DeliveryState::Cancelled, "")
    }
    /// A claimed connection stays bound even after its ticket is removed. The
    /// host stores that binding, so a revoked ticket cannot become an ordinary pull.
    pub fn claim_cancel(
        &self,
        peer: Peer,
        ticket: [u8; 32],
    ) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.state
            .lock()
            .unwrap()
            .tickets
            .get(&ticket)
            .filter(|t| t.claimed == Some(peer) && Instant::now() < t.expires)
            .map(|t| t.cancel.subscribe())
    }
    pub fn claim_alive(&self, peer: Peer, ticket: [u8; 32]) -> bool {
        self.state
            .lock()
            .unwrap()
            .tickets
            .get(&ticket)
            .is_some_and(|t| {
                t.claimed == Some(peer)
                    && t.scope == remote_core::shared_files::current_share_scope()
                    && Instant::now() < t.expires
            })
    }
    pub fn reconcile(&self, live: impl Fn(Peer) -> bool) {
        let mut s = self.state.lock().unwrap();
        let now = Instant::now();
        let scope = remote_core::shared_files::current_share_scope();
        for o in s.incoming.values() {
            if !live(o.peer) || o.peer.scope != scope || !o.alive() {
                o.finish(
                    DeliveryState::Cancelled,
                    "Control connection or lease ended",
                );
            }
        }
        // Keep tombstones until the authenticated control connection ends. A
        // repeated Offer cannot resurrect a GUI lease after Stop or rejection.
        s.incoming.retain(|(p, _), _| live(*p));
        let expired = s
            .tickets
            .iter()
            .filter(|(_, t)| {
                now >= t.expires || t.scope != scope || t.claimed.is_some_and(|p| !live(p))
            })
            .map(|(k, _)| *k)
            .collect::<Vec<_>>();
        for k in expired {
            s.tickets.remove(&k);
        }
    }
}

fn register(
    s: &mut State,
    ticket: [u8; 32],
    target: String,
    generation: String,
    connection: u32,
    sequence: u64,
    scope: ShareScope,
    now: Instant,
) -> Result<(), String> {
    if s.identity.is_none()
        || scope.is_none()
        || ticket == [0; 32]
        || target.is_empty()
        || target.len() > 256
        || generation.is_empty()
        || generation.len() > 128
        || connection == 0
    {
        return Err("Invalid directed intent".into());
    }
    if s.source_generation.as_deref() != Some(&generation) || sequence <= s.registration_high_water
    {
        return Err("Sending window changed or registration was replayed".into());
    }
    if s.tickets.len() >= LIMIT || s.tickets.contains_key(&ticket) {
        return Err("Too many active deliveries".into());
    }
    s.registration_high_water = sequence;
    s.tickets.insert(
        ticket,
        Ticket {
            target,
            generation,
            scope,
            _connection: connection,
            revision: 1,
            expires: now + LEASE,
            claimed: None,
            cancel: tokio::sync::watch::channel(false).0,
        },
    );
    Ok(())
}

fn renew(
    t: &mut Ticket,
    generation: &str,
    revision: u64,
    now: Instant,
    scope: ShareScope,
) -> Result<(), String> {
    if t.generation != generation || t.scope != scope || now >= t.expires || revision < t.revision {
        return Err("Directed intent expired or stale".into());
    }
    if revision > t.revision {
        t.revision = revision;
        t.expires = now + LEASE;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn peer() -> Peer {
        Peer {
            addr: "127.0.0.1:1".parse().unwrap(),
            connection: 7,
            nonce: [3; 16],
            scope: Some([4; 32]),
        }
    }
    fn ticket() -> Ticket {
        Ticket {
            target: "receiver".into(),
            generation: "gui-1".into(),
            scope: peer().scope,
            _connection: 8,
            revision: 1,
            expires: Instant::now() + LEASE,
            claimed: None,
            cancel: tokio::sync::watch::channel(false).0,
        }
    }
    #[test]
    fn duplicate_revision_does_not_extend_and_expired_intent_cannot_revive() {
        let mut t = ticket();
        let old = t.expires;
        let now = Instant::now();
        renew(&mut t, "gui-1", 1, now, peer().scope).unwrap();
        assert_eq!(old, t.expires);
        assert!(renew(&mut t, "gui-2", 2, now, peer().scope).is_err());
        assert!(renew(&mut t, "gui-1", 2, now, Some([5; 32])).is_err());
        assert!(renew(&mut t, "gui-1", 2, old, peer().scope).is_err());
        renew(&mut t, "gui-1", 2, now, peer().scope).unwrap();
        assert_eq!(t.revision, 2);
    }
    #[tokio::test]
    async fn claim_binds_exact_peer_and_revocation_signals_without_host_poll() {
        let (b, _rx) = Broker::new();
        let token = [8; 32];
        b.state.lock().unwrap().tickets.insert(token, ticket());
        assert!(matches!(
            b.claim_in_scope(peer(), 1, "receiver", token, peer().scope),
            SessionCommand::DeliveryReceipt {
                state: DeliveryState::Claimed,
                ..
            }
        ));
        let mut cancel = b.claim_cancel(peer(), token).unwrap();
        let mut other = peer();
        other.nonce = [9; 16];
        assert!(matches!(
            b.claim_in_scope(other, 1, "receiver", token, peer().scope),
            SessionCommand::DeliveryReceipt {
                state: DeliveryState::Rejected,
                ..
            }
        ));
        assert!(b.claim_cancel(peer(), [9; 32]).is_none());
        b.local(LocalIntent::Revoke {
            ticket: token,
            generation: "gui-1".into(),
        })
        .unwrap();
        cancel.changed().await.unwrap();
        assert!(*cancel.borrow());
        assert!(b.claim_cancel(peer(), token).is_none());
    }
    #[tokio::test]
    async fn terminal_offer_is_not_resurrected_by_retry_or_new_revision() {
        let (b, mut rx) = Broker::new();
        b.set_identity("receiver".into());
        let offer = |revision| {
            b.offer(
                peer(),
                1,
                "source".into(),
                "receiver".into(),
                [8; 32],
                revision,
            )
        };
        assert!(matches!(
            offer(2),
            SessionCommand::DeliveryReceipt {
                state: DeliveryState::Rejected,
                ..
            }
        ));
        assert!(matches!(
            offer(1),
            SessionCommand::DeliveryReceipt {
                state: DeliveryState::Pending,
                ..
            }
        ));
        let incoming = rx.recv().await.unwrap();
        let expires = incoming.snapshot().1;
        offer(1);
        assert_eq!(incoming.snapshot().1, expires);
        incoming.finish(DeliveryState::Cancelled, "Receiver stopped");
        assert!(matches!(
            offer(2),
            SessionCommand::DeliveryReceipt {
                state: DeliveryState::Cancelled,
                ..
            }
        ));
        assert!(!incoming.alive());
        assert!(rx.try_recv().is_err());
    }
    #[tokio::test]
    async fn source_deadline_cancels_claim_even_without_network_loop() {
        let (b, _rx) = Broker::new();
        let mut t = ticket();
        t.scope = remote_core::shared_files::current_share_scope();
        t.expires = Instant::now() + Duration::from_millis(10);
        let mut cancel = t.cancel.subscribe();
        b.state.lock().unwrap().tickets.insert([8; 32], t);
        tokio::time::timeout(Duration::from_secs(1), cancel.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(*cancel.borrow());
    }
    #[tokio::test]
    async fn another_offer_cannot_displace_current_receiver() {
        let (b, mut rx) = Broker::new();
        b.set_identity("receiver".into());
        b.offer(peer(), 1, "source".into(), "receiver".into(), [8; 32], 1);
        let first = rx.recv().await.unwrap();
        assert!(matches!(
            b.offer(peer(), 2, "other".into(), "receiver".into(), [9; 32], 1),
            SessionCommand::DeliveryReceipt {
                state: DeliveryState::Rejected,
                ..
            }
        ));
        assert!(first.alive());
        let mut other = peer();
        other.connection += 1;
        b.cancel(other, 1);
        assert!(first.alive());
    }
    #[tokio::test]
    async fn registration_history_is_bounded_without_a_lifetime_delivery_limit() {
        let (b, _) = Broker::new();
        b.set_identity("source".into());
        b.verified_gui_generation("gui-1");
        for sequence in 1u64..=1024 {
            let mut token = [8; 32];
            token[..8].copy_from_slice(&sequence.to_le_bytes());
            register(
                &mut b.state.lock().unwrap(),
                token,
                "receiver".into(),
                "gui-1".into(),
                8,
                sequence,
                peer().scope,
                Instant::now(),
            )
            .unwrap();
            b.local(LocalIntent::Revoke {
                ticket: token,
                generation: "gui-1".into(),
            })
            .unwrap();
        }
        assert!(b.state.lock().unwrap().tickets.is_empty());
        assert_eq!(b.state.lock().unwrap().registration_high_water, 1024);
        assert!(
            register(
                &mut b.state.lock().unwrap(),
                [9; 32],
                "receiver".into(),
                "gui-1".into(),
                8,
                1,
                peer().scope,
                Instant::now()
            )
            .is_err()
        );
        b.verified_gui_generation("gui-2");
        assert!(
            register(
                &mut b.state.lock().unwrap(),
                [9; 32],
                "receiver".into(),
                "gui-1".into(),
                8,
                1025,
                peer().scope,
                Instant::now()
            )
            .is_err()
        );
        register(
            &mut b.state.lock().unwrap(),
            [9; 32],
            "receiver".into(),
            "gui-2".into(),
            8,
            1,
            peer().scope,
            Instant::now(),
        )
        .unwrap();
    }
}
