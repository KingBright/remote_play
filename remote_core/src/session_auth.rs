//! Authenticated challenge/Finish establishment, independent of remote wall time.
//! Paired group identity is unchanged. All state and traffic keys are ephemeral.
use crate::session_crypto::{SessionCrypto, SessionRole, random_bytes_16};
use hmac::{Hmac, Mac};
use protocol::session_auth::{AUTH_SUITE, AUTH_VERSION, HandshakeMessage, RejectCode, Transcript};
use sha2::Sha256;
use std::{
    collections::HashMap,
    net::SocketAddr,
    time::{Duration, Instant, SystemTime},
};

type HmacSha256 = Hmac<Sha256>;
pub const HANDSHAKE_LIFETIME: Duration = Duration::from_secs(6);
const MAX_HANDSHAKES: usize = 32;
const DOMAIN: &[u8] = b"RemotePlay/authenticated-session/v3\0";

#[derive(Clone, Copy, Debug)]
pub struct AuthTime {
    monotonic: Instant,
    wall: SystemTime,
}
impl AuthTime {
    pub fn now() -> Self {
        Self {
            monotonic: Instant::now(),
            wall: SystemTime::now(),
        }
    }
    fn fresh_at(self, now: Self) -> bool {
        // Instant does not include suspend on every supported OS. A second,
        // strictly local elapsed-time guard fails closed on observable suspend
        // or wall-clock discontinuity. No peer timestamp enters this decision.
        let Some(mono) = now.monotonic.checked_duration_since(self.monotonic) else {
            return false;
        };
        let Ok(wall) = now.wall.duration_since(self.wall) else {
            return false;
        };
        mono < HANDSHAKE_LIFETIME
            && wall < HANDSHAKE_LIFETIME
            && mono.abs_diff(wall) <= Duration::from_secs(1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    InvalidMac,
    ProtocolMismatch,
    WrongFlight,
    Expired,
    Rejected(RejectCode),
    Crypto,
}
impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProtocolMismatch => write!(
                f,
                "RemotePlay authentication protocol mismatch (required version {AUTH_VERSION}, suite {AUTH_SUITE}); update both endpoints"
            ),
            Self::Expired => write!(
                f,
                "session challenge expired or local clock continuity changed; retry connection"
            ),
            Self::InvalidMac => write!(f, "session authentication failed"),
            Self::WrongFlight => write!(
                f,
                "session handshake does not match this connection attempt"
            ),
            Self::Rejected(code) => write!(f, "session authentication rejected: {code:?}"),
            Self::Crypto => write!(f, "session key construction failed"),
        }
    }
}
impl std::error::Error for AuthError {}

fn transcript_bytes(t: &Transcript) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(54);
    bytes.extend_from_slice(&t.version.to_be_bytes());
    bytes.extend_from_slice(&t.suite.to_be_bytes());
    bytes.extend_from_slice(&t.client_nonce);
    bytes.extend_from_slice(&t.server_nonce);
    bytes.extend_from_slice(&t.session_id);
    bytes
}
fn hello_bytes(version: u32, suite: u16, nonce: &[u8; 16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(22);
    bytes.extend_from_slice(&version.to_be_bytes());
    bytes.extend_from_slice(&suite.to_be_bytes());
    bytes.extend_from_slice(nonce);
    bytes
}
fn reject_bytes(version: u32, nonce: &[u8; 16], code: RejectCode) -> Vec<u8> {
    let mut bytes = hello_bytes(version, AUTH_SUITE, nonce);
    bytes.push(match code {
        RejectCode::ProtocolMismatch => 0,
        RejectCode::Busy => 1,
        RejectCode::Capacity => 2,
        RejectCode::Expired => 3,
    });
    bytes
}
fn signer(key: &[u8], label: &[u8], bytes: &[u8]) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts arbitrary key length");
    mac.update(DOMAIN);
    mac.update(label);
    mac.update(&[0]);
    mac.update(bytes);
    mac
}
fn tag(key: &[u8], label: &[u8], bytes: &[u8]) -> [u8; 32] {
    signer(key, label, bytes).finalize().into_bytes().into()
}
fn verify(key: &[u8], label: &[u8], bytes: &[u8], mac: &[u8; 32]) -> Result<(), AuthError> {
    signer(key, label, bytes)
        .verify_slice(mac)
        .map_err(|_| AuthError::InvalidMac)
}
fn check_version(t: &Transcript) -> Result<(), AuthError> {
    if t.version == AUTH_VERSION && t.suite == AUTH_SUITE {
        Ok(())
    } else {
        Err(AuthError::ProtocolMismatch)
    }
}

pub struct ClientHandshake {
    nonce: [u8; 16],
    started: AuthTime,
    transcript: Option<Transcript>,
    finished: bool,
}
impl ClientHandshake {
    pub fn new(now: AuthTime) -> Self {
        Self {
            nonce: random_bytes_16(),
            started: now,
            transcript: None,
            finished: false,
        }
    }
    pub fn client_nonce(&self) -> [u8; 16] {
        self.nonce
    }
    pub fn message(&self, key: &[u8]) -> HandshakeMessage {
        match self.transcript {
            Some(transcript) => HandshakeMessage::ClientFinish {
                mac: tag(key, b"client-finish", &transcript_bytes(&transcript)),
                transcript,
            },
            None => HandshakeMessage::Hello {
                version: AUTH_VERSION,
                suite: AUTH_SUITE,
                client_nonce: self.nonce,
                mac: tag(
                    key,
                    b"client-hello",
                    &hello_bytes(AUTH_VERSION, AUTH_SUITE, &self.nonce),
                ),
            },
        }
    }
    pub fn receive(
        &mut self,
        key: &[u8],
        message: &HandshakeMessage,
        now: AuthTime,
    ) -> Result<Option<SessionCrypto>, AuthError> {
        if self.finished {
            return Err(AuthError::WrongFlight);
        }
        if !self.started.fresh_at(now) {
            return Err(AuthError::Expired);
        }
        match message {
            HandshakeMessage::Challenge { transcript, mac } => {
                if transcript.client_nonce != self.nonce {
                    return Err(AuthError::WrongFlight);
                }
                verify(key, b"host-challenge", &transcript_bytes(transcript), mac)?;
                check_version(transcript)?;
                if self.transcript.is_some_and(|old| old != *transcript) {
                    return Err(AuthError::WrongFlight);
                }
                self.transcript = Some(*transcript);
                Ok(None)
            }
            HandshakeMessage::ServerFinish { transcript, mac } => {
                if self.transcript != Some(*transcript) {
                    return Err(AuthError::WrongFlight);
                }
                verify(key, b"host-finish", &transcript_bytes(transcript), mac)?;
                let crypto = SessionCrypto::for_session(
                    key,
                    &transcript_bytes(transcript),
                    transcript.session_id,
                    SessionRole::Client,
                )
                .map_err(|_| AuthError::Crypto)?;
                self.finished = true;
                Ok(Some(crypto))
            }
            HandshakeMessage::Reject {
                version,
                client_nonce,
                code,
                mac,
            } => {
                if *client_nonce != self.nonce {
                    return Err(AuthError::WrongFlight);
                }
                verify(
                    key,
                    b"host-reject",
                    &reject_bytes(*version, client_nonce, *code),
                    mac,
                )?;
                if *code == RejectCode::ProtocolMismatch {
                    return Err(AuthError::ProtocolMismatch);
                }
                Err(AuthError::Rejected(*code))
            }
            _ => Err(AuthError::WrongFlight),
        }
    }
}

struct Entry {
    transcript: Transcript,
    started: AuthTime,
    completed: bool,
}
#[derive(Default)]
pub struct ServerHandshake {
    entries: HashMap<SocketAddr, Entry>,
}
pub struct EstablishedSession {
    pub session_id: [u8; 16],
    pub client_nonce: [u8; 16],
    pub crypto: SessionCrypto,
}
pub struct ServerReply {
    pub message: HandshakeMessage,
    pub established: Option<EstablishedSession>,
}
impl ServerHandshake {
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn forget(&mut self, addr: SocketAddr) {
        self.entries.remove(&addr);
    }
    pub fn expire(&mut self, now: AuthTime) {
        self.entries.retain(|_, entry| entry.started.fresh_at(now));
    }
    fn reject(key: &[u8], nonce: [u8; 16], code: RejectCode) -> ServerReply {
        ServerReply {
            message: HandshakeMessage::Reject {
                version: AUTH_VERSION,
                client_nonce: nonce,
                code,
                mac: tag(
                    key,
                    b"host-reject",
                    &reject_bytes(AUTH_VERSION, &nonce, code),
                ),
            },
            established: None,
        }
    }
    /// `allow_new` is checked at both challenge and Finish. Callers must preserve
    /// existing native ownership and capacity when deriving it. No await occurs
    /// between this decision and installation of the returned established key.
    pub fn receive(
        &mut self,
        key: &[u8],
        message: &HandshakeMessage,
        addr: SocketAddr,
        now: AuthTime,
        allow_new: bool,
    ) -> Result<ServerReply, AuthError> {
        self.expire(now);
        match message {
            HandshakeMessage::Hello {
                version,
                suite,
                client_nonce,
                mac,
            } => {
                verify(
                    key,
                    b"client-hello",
                    &hello_bytes(*version, *suite, client_nonce),
                    mac,
                )?;
                if *version != AUTH_VERSION || *suite != AUTH_SUITE {
                    return Ok(Self::reject(
                        key,
                        *client_nonce,
                        RejectCode::ProtocolMismatch,
                    ));
                }
                if let Some(entry) = self.entries.get(&addr) {
                    if entry.transcript.client_nonce == *client_nonce {
                        let transcript = entry.transcript;
                        return Ok(ServerReply {
                            message: HandshakeMessage::Challenge {
                                mac: tag(key, b"host-challenge", &transcript_bytes(&transcript)),
                                transcript,
                            },
                            established: None,
                        });
                    }
                    return Ok(Self::reject(key, *client_nonce, RejectCode::Busy));
                }
                if self
                    .entries
                    .values()
                    .any(|entry| entry.transcript.client_nonce == *client_nonce)
                {
                    return Ok(Self::reject(key, *client_nonce, RejectCode::Busy));
                }
                if !allow_new {
                    return Ok(Self::reject(key, *client_nonce, RejectCode::Busy));
                }
                if self.entries.len() >= MAX_HANDSHAKES {
                    return Ok(Self::reject(key, *client_nonce, RejectCode::Capacity));
                }
                let transcript = Transcript {
                    version: AUTH_VERSION,
                    suite: AUTH_SUITE,
                    client_nonce: *client_nonce,
                    server_nonce: random_bytes_16(),
                    session_id: random_bytes_16(),
                };
                self.entries.insert(
                    addr,
                    Entry {
                        transcript,
                        started: now,
                        completed: false,
                    },
                );
                Ok(ServerReply {
                    message: HandshakeMessage::Challenge {
                        mac: tag(key, b"host-challenge", &transcript_bytes(&transcript)),
                        transcript,
                    },
                    established: None,
                })
            }
            HandshakeMessage::ClientFinish { transcript, mac } => {
                check_version(transcript)?;
                verify(key, b"client-finish", &transcript_bytes(transcript), mac)?;
                let Some(entry) = self.entries.get_mut(&addr) else {
                    return Ok(Self::reject(
                        key,
                        transcript.client_nonce,
                        RejectCode::Expired,
                    ));
                };
                if entry.transcript != *transcript {
                    return Err(AuthError::WrongFlight);
                }
                let reply = HandshakeMessage::ServerFinish {
                    mac: tag(key, b"host-finish", &transcript_bytes(transcript)),
                    transcript: *transcript,
                };
                if entry.completed {
                    return Ok(ServerReply {
                        message: reply,
                        established: None,
                    });
                }
                if !allow_new {
                    return Ok(Self::reject(key, transcript.client_nonce, RejectCode::Busy));
                }
                let crypto = SessionCrypto::for_session(
                    key,
                    &transcript_bytes(transcript),
                    transcript.session_id,
                    SessionRole::Host,
                )
                .map_err(|_| AuthError::Crypto)?;
                entry.completed = true;
                Ok(ServerReply {
                    message: reply,
                    established: Some(EstablishedSession {
                        session_id: transcript.session_id,
                        client_nonce: transcript.client_nonce,
                        crypto,
                    }),
                })
            }
            _ => Err(AuthError::WrongFlight),
        }
    }
}

/// Shared client implementation for desktop, bridge, and protocol fixtures.
pub async fn authenticate_with_key(
    sender: &crate::net::UdpSender,
    receiver: &crate::net::UdpReceiver,
    target: SocketAddr,
    key: &[u8],
) -> Result<[u8; 16], String> {
    use crate::net::MultiplexedPacket;
    use protocol::ControlMessage;
    let mut handshake = ClientHandshake::new(AuthTime::now());
    let deadline = tokio::time::Instant::now() + HANDSHAKE_LIFETIME;
    let mut retry = tokio::time::interval(Duration::from_millis(250));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _=tokio::time::sleep_until(deadline)=>return Err("session authentication timed out; required protocol v3 (no legacy fallback)".into()),
            _=retry.tick()=>{sender.send_control(&ControlMessage::SessionHandshake(Box::new(handshake.message(key))),target).await.map_err(|e|e.to_string())?;},
            received=receiver.recv()=>{
                let (message,addr)=match received {Ok(MultiplexedPacket::Control(ControlMessage::SessionHandshake(message),addr))=>(*message,addr),_=>continue};
                if addr!=target{continue;}
                match handshake.receive(key,&message,AuthTime::now()) {
                    Ok(Some(crypto))=>{let session_id=crypto.session_id();sender.install_peer_crypto(target,crypto);return Ok(session_id);},
                    Ok(None)=>{sender.send_control(&ControlMessage::SessionHandshake(Box::new(handshake.message(key))),target).await.map_err(|e|e.to_string())?;},
                    Err(AuthError::InvalidMac|AuthError::WrongFlight)=>continue,
                    Err(error)=>return Err(error.to_string()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &[u8] = b"synthetic-test-group-key";
    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }
    fn later(time: AuthTime, seconds: u64) -> AuthTime {
        AuthTime {
            monotonic: time.monotonic + Duration::from_secs(seconds),
            wall: time.wall + Duration::from_secs(seconds),
        }
    }
    fn challenge(
        client: &mut ClientHandshake,
        server: &mut ServerHandshake,
        now: AuthTime,
    ) -> Transcript {
        let reply = server
            .receive(KEY, &client.message(KEY), addr(1), now, true)
            .unwrap();
        assert!(reply.established.is_none());
        client.receive(KEY, &reply.message, now).unwrap();
        let HandshakeMessage::Challenge { transcript, .. } = reply.message else {
            panic!()
        };
        transcript
    }
    #[test]
    fn remote_wall_clock_offset_never_enters_authentication() {
        let now = AuthTime::now();
        let client_time = AuthTime {
            wall: now.wall + Duration::from_secs(86400),
            ..now
        };
        let mut client = ClientHandshake::new(client_time);
        let mut server = ServerHandshake::default();
        let response = server
            .receive(KEY, &client.message(KEY), addr(1), now, true)
            .unwrap();
        client.receive(KEY, &response.message, client_time).unwrap();
        let response = server
            .receive(KEY, &client.message(KEY), addr(1), now, true)
            .unwrap();
        let host = response.established.unwrap().crypto;
        let viewer = client
            .receive(KEY, &response.message, client_time)
            .unwrap()
            .unwrap();
        assert_eq!(
            host.open(&viewer.seal(b"media").unwrap()).unwrap(),
            b"media"
        );
    }
    #[test]
    fn every_flight_is_bound_to_transcript_and_role() {
        let now = AuthTime::now();
        let mut c = ClientHandshake::new(now);
        let mut s = ServerHandshake::default();
        let transcript = challenge(&mut c, &mut s, now);
        let HandshakeMessage::ClientFinish { mac, .. } = c.message(KEY) else {
            panic!()
        };
        assert!(matches!(
            c.receive(
                KEY,
                &HandshakeMessage::ServerFinish { transcript, mac },
                now
            ),
            Err(AuthError::InvalidMac)
        ));
        for field in 0..5 {
            let mut t = transcript;
            match field {
                0 => t.client_nonce[0] ^= 1,
                1 => t.server_nonce[0] ^= 1,
                2 => t.session_id[0] ^= 1,
                3 => t.version += 1,
                _ => t.suite += 1,
            };
            assert!(
                s.receive(
                    KEY,
                    &HandshakeMessage::ClientFinish { transcript: t, mac },
                    addr(1),
                    now,
                    true
                )
                .is_err()
            );
        }
        assert!(
            s.receive(KEY, &c.message(KEY), addr(2), now, true)
                .unwrap()
                .established
                .is_none()
        );
    }
    #[test]
    fn unauthenticated_mismatch_cannot_abort_client() {
        let now = AuthTime::now();
        let mut c = ClientHandshake::new(now);
        let mut s = ServerHandshake::default();
        let transcript = challenge(&mut c, &mut s, now);
        let mut wrong = transcript;
        wrong.version += 1;
        assert!(matches!(
            c.receive(
                KEY,
                &HandshakeMessage::Challenge {
                    transcript: wrong,
                    mac: [0; 32]
                },
                now
            ),
            Err(AuthError::InvalidMac)
        ));
        let response = s.receive(KEY, &c.message(KEY), addr(1), now, true).unwrap();
        assert!(c.receive(KEY, &response.message, now).unwrap().is_some());
    }
    #[test]
    fn authenticated_version_mismatch_is_explicit_without_downgrade() {
        let now = AuthTime::now();
        let mut c = ClientHandshake::new(now);
        let mut s = ServerHandshake::default();
        let version = AUTH_VERSION + 1;
        let hello = HandshakeMessage::Hello {
            version,
            suite: AUTH_SUITE,
            client_nonce: c.nonce,
            mac: tag(
                KEY,
                b"client-hello",
                &hello_bytes(version, AUTH_SUITE, &c.nonce),
            ),
        };
        let response = s.receive(KEY, &hello, addr(1), now, true).unwrap();
        assert!(matches!(
            c.receive(KEY, &response.message, now),
            Err(AuthError::ProtocolMismatch)
        ));
        assert!(s.entries.is_empty());
    }
    #[test]
    fn duplicate_hello_and_finish_do_not_renew_or_reinstall() {
        let now = AuthTime::now();
        let mut c = ClientHandshake::new(now);
        let mut s = ServerHandshake::default();
        let hello = c.message(KEY);
        let transcript = challenge(&mut c, &mut s, now);
        let response = s
            .receive(KEY, &hello, addr(1), later(now, 5), false)
            .unwrap();
        assert!(
            matches!(response.message,HandshakeMessage::Challenge{transcript:t,..}if t==transcript)
        );
        let response = s
            .receive(KEY, &c.message(KEY), addr(1), later(now, 5), true)
            .unwrap();
        let host = response.established.unwrap().crypto;
        let viewer = c
            .receive(KEY, &response.message, later(now, 5))
            .unwrap()
            .unwrap();
        let packet = viewer.seal(b"once").unwrap();
        assert!(host.open(&packet).is_ok());
        let replay = HandshakeMessage::ClientFinish {
            transcript,
            mac: tag(KEY, b"client-finish", &transcript_bytes(&transcript)),
        };
        assert!(
            s.receive(KEY, &replay, addr(1), later(now, 5), false)
                .unwrap()
                .established
                .is_none()
        );
        assert!(host.open(&packet).is_err());
        assert!(matches!(
            s.receive(KEY, &replay, addr(1), later(now, 6), true)
                .unwrap()
                .message,
            HandshakeMessage::Reject {
                code: RejectCode::Expired,
                ..
            }
        ));
    }
    #[test]
    fn finish_rechecks_admission_after_challenge() {
        let now = AuthTime::now();
        let mut c = ClientHandshake::new(now);
        let mut s = ServerHandshake::default();
        challenge(&mut c, &mut s, now);
        let denied = s
            .receive(KEY, &c.message(KEY), addr(1), now, false)
            .unwrap();
        assert!(denied.established.is_none());
        assert!(matches!(
            denied.message,
            HandshakeMessage::Reject {
                code: RejectCode::Busy,
                ..
            }
        ));
        assert!(
            s.receive(KEY, &c.message(KEY), addr(1), now, true)
                .unwrap()
                .established
                .is_some()
        );
    }
    #[test]
    fn retired_finish_cannot_replace_new_same_address_session() {
        let now = AuthTime::now();
        let mut old = ClientHandshake::new(now);
        let mut s = ServerHandshake::default();
        let old_id = challenge(&mut old, &mut s, now).session_id;
        let old_finish = old.message(KEY);
        s.forget(addr(1));
        let mut new = ClientHandshake::new(now);
        let new_id = challenge(&mut new, &mut s, now).session_id;
        assert_ne!(old_id, new_id);
        assert!(matches!(
            s.receive(KEY, &old_finish, addr(1), now, true),
            Err(AuthError::WrongFlight)
        ));
        assert_eq!(
            s.receive(KEY, &new.message(KEY), addr(1), now, true)
                .unwrap()
                .established
                .unwrap()
                .session_id,
            new_id
        );
    }
    #[test]
    fn key_rotation_and_process_restart_drop_pending_challenges() {
        let now = AuthTime::now();
        let mut c = ClientHandshake::new(now);
        let mut s = ServerHandshake::default();
        challenge(&mut c, &mut s, now);
        let finish = c.message(KEY);
        s.clear();
        assert!(s.receive(b"new-key", &finish, addr(1), now, true).is_err());
        assert!(
            s.receive(KEY, &finish, addr(1), now, true)
                .unwrap()
                .established
                .is_none()
        );
        let mut restarted = ServerHandshake::default();
        assert!(
            restarted
                .receive(KEY, &finish, addr(1), now, true)
                .unwrap()
                .established
                .is_none()
        );
    }
    #[test]
    fn invalid_mac_allocates_nothing_and_valid_replayed_nonce_cannot_fill_addresses() {
        let now = AuthTime::now();
        let c = ClientHandshake::new(now);
        let mut s = ServerHandshake::default();
        assert!(
            s.receive(b"wrong", &c.message(KEY), addr(1), now, true)
                .is_err()
        );
        assert!(s.entries.is_empty());
        s.receive(KEY, &c.message(KEY), addr(1), now, true).unwrap();
        for port in 2..50 {
            assert!(matches!(
                s.receive(KEY, &c.message(KEY), addr(port), now, true)
                    .unwrap()
                    .message,
                HandshakeMessage::Reject {
                    code: RejectCode::Busy,
                    ..
                }
            ));
        }
        assert_eq!(s.entries.len(), 1);
        for port in 2..=MAX_HANDSHAKES as u16 {
            let c = ClientHandshake::new(now);
            s.receive(KEY, &c.message(KEY), addr(port), now, true)
                .unwrap();
        }
        let c = ClientHandshake::new(now);
        assert!(matches!(
            s.receive(KEY, &c.message(KEY), addr(99), now, true)
                .unwrap()
                .message,
            HandshakeMessage::Reject {
                code: RejectCode::Capacity,
                ..
            }
        ));
        assert_eq!(s.entries.len(), MAX_HANDSHAKES);
    }
    #[test]
    fn local_suspend_backward_and_forward_clock_steps_fail_closed() {
        let now = AuthTime::now();
        assert!(now.fresh_at(later(now, 5)));
        assert!(!now.fresh_at(later(now, 6)));
        assert!(!now.fresh_at(AuthTime {
            monotonic: now.monotonic,
            wall: now.wall + Duration::from_secs(60)
        }));
        assert!(!now.fresh_at(AuthTime {
            monotonic: now.monotonic + Duration::from_secs(1),
            wall: now.wall - Duration::from_secs(1)
        }));
        assert!(!now.fresh_at(AuthTime {
            monotonic: now.monotonic + Duration::from_secs(4),
            wall: now.wall + Duration::from_secs(1)
        }));
        let mut c = ClientHandshake::new(now);
        let mut s = ServerHandshake::default();
        let response = s.receive(KEY, &c.message(KEY), addr(1), now, true).unwrap();
        assert!(matches!(
            c.receive(KEY, &response.message, later(now, 6)),
            Err(AuthError::Expired)
        ));
    }
}
