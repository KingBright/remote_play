use hmac::{Hmac, Mac};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use sha2::Sha256;
use std::error::Error;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const KEY_CONTEXT: &[u8] = b"RemotePlay/fixture-session-id/v3";
static PAIRED_PSK: std::sync::RwLock<Option<Vec<u8>>> = std::sync::RwLock::new(None);

/// Called by application entrypoints after loading or changing the paired group.
/// No process-environment mutation is needed while transport threads are running.
pub fn use_paired_session_secret(secret: &str) {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(b"remote-play/paired-media/v2");
    let next = mac.finalize().into_bytes().to_vec();
    let mut current = PAIRED_PSK.write().unwrap();
    if current.as_ref() != Some(&next) {
        crate::shared_files::shared_file_catalog().clear();
    }
    *current = Some(next);
}
pub fn refresh_paired_session_secret(secret: &str) {
    let configured = PAIRED_PSK.read().unwrap().is_some();
    if configured {
        use_paired_session_secret(secret);
    }
}
pub const MULTIPLEX_ENCRYPTED: u8 = 0x07;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCryptoError {
    InvalidKey,
    Seal,
    Open,
    ShortPacket,
    Replay,
    SequenceExhausted,
    AuthenticationLimit,
    OversizedRecord,
}

impl fmt::Display for SessionCryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionCryptoError::InvalidKey => write!(f, "session crypto key is invalid"),
            SessionCryptoError::Seal => write!(f, "failed to seal session packet"),
            SessionCryptoError::Open => write!(f, "failed to open session packet"),
            SessionCryptoError::ShortPacket => write!(f, "encrypted packet is truncated"),
            SessionCryptoError::Replay => write!(
                f,
                "session record is duplicate or outside its replay window"
            ),
            SessionCryptoError::AuthenticationLimit => write!(
                f,
                "session authentication failure limit reached; establish a new session"
            ),
            SessionCryptoError::OversizedRecord => {
                write!(f, "session record exceeds the authenticated size limit")
            }
            SessionCryptoError::SequenceExhausted => {
                write!(f, "session record limit reached; establish a new session")
            }
        }
    }
}

impl Error for SessionCryptoError {}

/// Direction is part of key derivation and authenticated record metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRole {
    Client,
    Host,
}

const RECORD_VERSION: u8 = 3;
const RECORD_HEADER_LEN: usize = 26;
const REPLAY_WINDOW: usize = 4096;
const MAX_RECORD_SEQUENCE: u64 = (1u64 << 48) - 1;
// Conservative engineering limits for large reassembled UDP records, not a
// claim that this custom PSK protocol inherits TLS/QUIC security bounds.
const MAX_RECORD_PLAINTEXT: usize = 8 * 1024 * 1024;
const MAX_AUTHENTICATION_FAILURES: u64 = 1 << 24;

#[derive(Debug)]
struct ReplayWindow {
    highest: u64,
    bits: [u64; REPLAY_WINDOW / 64],
}
impl Default for ReplayWindow {
    fn default() -> Self {
        Self {
            highest: 0,
            bits: [0; REPLAY_WINDOW / 64],
        }
    }
}
impl ReplayWindow {
    fn contains(&self, sequence: u64) -> bool {
        if sequence == 0 || sequence > MAX_RECORD_SEQUENCE {
            return true;
        }
        if sequence > self.highest {
            return false;
        }
        let age = self.highest - sequence;
        age >= REPLAY_WINDOW as u64 || self.bits[age as usize / 64] & (1 << (age % 64)) != 0
    }
    fn insert(&mut self, sequence: u64) {
        if sequence > self.highest {
            let shift = (sequence - self.highest).min(REPLAY_WINDOW as u64) as usize;
            if shift == REPLAY_WINDOW {
                self.bits.fill(0);
            } else {
                let words = shift / 64;
                let bits = shift % 64;
                for dst in (0..self.bits.len()).rev() {
                    self.bits[dst] = if dst < words {
                        0
                    } else {
                        let src = dst - words;
                        (self.bits[src] << bits)
                            | if bits > 0 && src > 0 {
                                self.bits[src - 1] >> (64 - bits)
                            } else {
                                0
                            }
                    };
                }
            }
            self.highest = sequence;
        }
        let age = (self.highest - sequence) as usize;
        self.bits[age / 64] |= 1 << (age % 64);
    }
}

pub struct SessionCrypto {
    send_key: LessSafeKey,
    receive_key: LessSafeKey,
    session_id: [u8; 16],
    send_direction: u8,
    next_sequence: std::sync::atomic::AtomicU64,
    received: std::sync::Mutex<ReplayWindow>,
    authentication_failures: std::sync::atomic::AtomicU64,
    exhausted: std::sync::atomic::AtomicBool,
}
impl SessionCrypto {
    /// Low-level key construction, also used by isolated transport fixtures.
    /// Production admission uses the challenge/Finish state machine.
    pub fn from_psk(
        psk: &[u8],
        salt: &[u8],
        role: SessionRole,
    ) -> Result<Self, SessionCryptoError> {
        let digest = derive_key(psk, salt);
        let mut session_id = [0; 16];
        session_id.copy_from_slice(&digest[..16]);
        Self::for_session(psk, salt, session_id, role)
    }
    pub(crate) fn for_session(
        psk: &[u8],
        transcript: &[u8],
        session_id: [u8; 16],
        role: SessionRole,
    ) -> Result<Self, SessionCryptoError> {
        let key = |label: &[u8]| {
            let mut mac =
                HmacSha256::new_from_slice(psk).map_err(|_| SessionCryptoError::InvalidKey)?;
            mac.update(b"RemotePlay/session-record/v3\0");
            mac.update(label);
            mac.update(transcript);
            let bytes = mac.finalize().into_bytes();
            UnboundKey::new(&CHACHA20_POLY1305, &bytes)
                .map(LessSafeKey::new)
                .map_err(|_| SessionCryptoError::InvalidKey)
        };
        let (send, receive, direction) = match role {
            SessionRole::Client => (b"client".as_slice(), b"host".as_slice(), 0),
            SessionRole::Host => (b"host".as_slice(), b"client".as_slice(), 1),
        };
        Ok(Self {
            send_key: key(send)?,
            receive_key: key(receive)?,
            session_id,
            send_direction: direction,
            next_sequence: std::sync::atomic::AtomicU64::new(1),
            received: std::sync::Mutex::new(ReplayWindow::default()),
            authentication_failures: std::sync::atomic::AtomicU64::new(0),
            exhausted: std::sync::atomic::AtomicBool::new(false),
        })
    }
    pub fn session_id(&self) -> [u8; 16] {
        self.session_id
    }
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SessionCryptoError> {
        use std::sync::atomic::Ordering::Relaxed;
        if self.exhausted.load(Relaxed) {
            return Err(SessionCryptoError::AuthenticationLimit);
        }
        if plaintext.len() > MAX_RECORD_PLAINTEXT {
            return Err(SessionCryptoError::OversizedRecord);
        }
        let sequence = self
            .next_sequence
            .fetch_update(Relaxed, Relaxed, |n| {
                (n <= MAX_RECORD_SEQUENCE).then(|| n + 1)
            })
            .map_err(|_| SessionCryptoError::SequenceExhausted)?;
        let mut out =
            Vec::with_capacity(RECORD_HEADER_LEN + plaintext.len() + CHACHA20_POLY1305.tag_len());
        out.push(RECORD_VERSION);
        out.extend_from_slice(&self.session_id);
        out.push(self.send_direction);
        out.extend_from_slice(&sequence.to_be_bytes());
        let mut nonce = [0; NONCE_LEN];
        nonce[4..].copy_from_slice(&sequence.to_be_bytes());
        let mut body = plaintext.to_vec();
        self.send_key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(out.as_slice()),
                &mut body,
            )
            .map_err(|_| SessionCryptoError::Seal)?;
        out.extend_from_slice(&body);
        Ok(out)
    }
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SessionCryptoError> {
        use std::sync::atomic::Ordering::Relaxed;
        if self.exhausted.load(Relaxed) {
            return Err(SessionCryptoError::AuthenticationLimit);
        }
        if sealed.len() > MAX_RECORD_PLAINTEXT + RECORD_HEADER_LEN + CHACHA20_POLY1305.tag_len() {
            return Err(SessionCryptoError::OversizedRecord);
        }
        if sealed.len() < RECORD_HEADER_LEN + CHACHA20_POLY1305.tag_len() {
            return Err(SessionCryptoError::ShortPacket);
        }
        if sealed[0] != RECORD_VERSION
            || sealed[1..17] != self.session_id
            || sealed[17] == self.send_direction
            || sealed[17] > 1
        {
            return Err(SessionCryptoError::Open);
        }
        let sequence = u64::from_be_bytes(sealed[18..26].try_into().unwrap());
        // Serialize validation and commit so cloned receivers cannot admit one record twice.
        let mut window = self.received.lock().map_err(|_| SessionCryptoError::Open)?;
        if self.exhausted.load(Relaxed) {
            return Err(SessionCryptoError::AuthenticationLimit);
        }
        if window.contains(sequence) {
            return Err(SessionCryptoError::Replay);
        }
        let mut nonce = [0; NONCE_LEN];
        nonce[4..].copy_from_slice(&sequence.to_be_bytes());
        let mut body = sealed[RECORD_HEADER_LEN..].to_vec();
        let plain = match self.receive_key.open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(&sealed[..RECORD_HEADER_LEN]),
            &mut body,
        ) {
            Ok(plain) => plain,
            Err(_) => {
                let failures = self.authentication_failures.fetch_add(1, Relaxed) + 1;
                if failures >= MAX_AUTHENTICATION_FAILURES {
                    self.exhausted.store(true, Relaxed);
                    return Err(SessionCryptoError::AuthenticationLimit);
                }
                return Err(SessionCryptoError::Open);
            }
        };
        let result = plain.to_vec();
        window.insert(sequence);
        Ok(result)
    }
}

pub fn derive_key(psk: &[u8], salt: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(psk).expect("hmac key");
    mac.update(KEY_CONTEXT);
    mac.update(salt);
    mac.finalize().into_bytes().into()
}

pub fn load_session_psk() -> Option<Vec<u8>> {
    std::env::var("WEAVELO_SESSION_PSK")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(String::into_bytes)
        .or_else(|| PAIRED_PSK.read().unwrap().clone())
}

/// File sessions use the paired profile, never a conflicting ambient key.
pub fn paired_file_session_key() -> Option<Vec<u8>> {
    let paired=PAIRED_PSK.read().unwrap().clone();
    let selected=load_session_psk();
    if matches_paired_file_key(paired.as_deref(),selected.as_deref()) {paired} else {None}
}
fn matches_paired_file_key(paired:Option<&[u8]>,selected:Option<&[u8]>)->bool {
    paired.is_some_and(|key|!key.is_empty()) && paired==selected
}
#[cfg(test)]
mod native_file_scope_tests {
    #[test]
    fn ambient_key_cannot_authorize_files_without_the_current_paired_group() {
        use super::matches_paired_file_key as allowed;
        assert!(!allowed(None,Some(b"old group")));
        assert!(!allowed(Some(b"new group"),Some(b"old group")));
        assert!(!allowed(Some(b"new group"),None));
        assert!(allowed(Some(b"new group"),Some(b"new group")));
        assert!(!allowed(Some(b""),Some(b"")));
    }
}

pub fn require_session_auth() -> bool {
    load_session_psk().is_some()
        || std::env::var("WEAVELO_REQUIRE_AUTH")
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

pub fn random_bytes_16() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes).expect("system random");
    bytes
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn fill_random(buf: &mut [u8]) -> Result<(), SessionCryptoError> {
    SystemRandom::new()
        .fill(buf)
        .map_err(|_| SessionCryptoError::Seal)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> (SessionCrypto, SessionCrypto) {
        (
            SessionCrypto::from_psk(b"shared-secret", b"fixture", SessionRole::Client).unwrap(),
            SessionCrypto::from_psk(b"shared-secret", b"fixture", SessionRole::Host).unwrap(),
        )
    }
    #[test]
    fn directional_roundtrip_and_reflection_rejection() {
        let (c, h) = pair();
        let p = c.seal(b"hello").unwrap();
        assert!(c.open(&p).is_err());
        assert_eq!(h.open(&p).unwrap(), b"hello");
        let q = h.seal(b"world").unwrap();
        assert_eq!(c.open(&q).unwrap(), b"world");
    }
    #[test]
    fn wrong_key_session_direction_and_tag_are_rejected() {
        let (c, h) = pair();
        let p = c.seal(b"hello").unwrap();
        let other = SessionCrypto::from_psk(b"wrong", b"fixture", SessionRole::Host).unwrap();
        assert!(other.open(&p).is_err());
        for index in [0, 1, 17, 18, p.len() - 1] {
            let mut bad = p.clone();
            bad[index] ^= 1;
            assert!(h.open(&bad).is_err());
        }
        assert_eq!(h.open(&p).unwrap(), b"hello");
    }
    #[test]
    fn duplicate_and_out_of_window_rejected_but_reordering_accepted() {
        let (c, h) = pair();
        let packets: Vec<_> = (0..4100).map(|_| c.seal(b"x").unwrap()).collect();
        assert!(h.open(&packets[2]).is_ok());
        assert!(h.open(&packets[0]).is_ok());
        assert_eq!(h.open(&packets[0]).unwrap_err(), SessionCryptoError::Replay);
        assert!(h.open(&packets[4099]).is_ok());
        assert_eq!(h.open(&packets[1]).unwrap_err(), SessionCryptoError::Replay);
        assert!(h.open(&packets[4098]).is_ok());
    }
    #[test]
    fn invalid_far_future_tag_does_not_advance_window() {
        let (c, h) = pair();
        let p = c.seal(b"x").unwrap();
        let mut bad = p.clone();
        bad[18..26].copy_from_slice(&MAX_RECORD_SEQUENCE.to_be_bytes());
        assert!(h.open(&bad).is_err());
        assert!(h.open(&p).is_ok());
    }
    #[test]
    fn simultaneous_duplicate_can_commit_only_once() {
        let (c, h) = pair();
        let p = c.seal(b"x").unwrap();
        let accepted = std::thread::scope(|scope| {
            let a = scope.spawn(|| h.open(&p).is_ok());
            let b = scope.spawn(|| h.open(&p).is_ok());
            usize::from(a.join().unwrap()) + usize::from(b.join().unwrap())
        });
        assert_eq!(accepted, 1);
    }
    #[test]
    fn sequence_exhaustion_requires_new_handshake() {
        let (c, h) = pair();
        c.next_sequence
            .store(MAX_RECORD_SEQUENCE, std::sync::atomic::Ordering::Relaxed);
        assert!(h.open(&c.seal(b"last").unwrap()).is_ok());
        assert_eq!(
            c.seal(b"never").unwrap_err(),
            SessionCryptoError::SequenceExhausted
        );
    }
    #[test]
    fn authentication_failure_budget_is_cumulative_and_never_downgrades() {
        use std::sync::atomic::Ordering::Relaxed;
        let (c, h) = pair();
        let packet = c.seal(b"valid").unwrap();
        let mut bad = packet.clone();
        *bad.last_mut().unwrap() ^= 1;
        h.authentication_failures
            .store(MAX_AUTHENTICATION_FAILURES - 2, Relaxed);
        assert_eq!(h.open(&bad).unwrap_err(), SessionCryptoError::Open);
        assert!(h.open(&packet).is_ok());
        assert_eq!(
            h.authentication_failures.load(Relaxed),
            MAX_AUTHENTICATION_FAILURES - 1
        );
        let mut next = c.seal(b"next").unwrap();
        *next.last_mut().unwrap() ^= 1;
        assert_eq!(
            h.open(&next).unwrap_err(),
            SessionCryptoError::AuthenticationLimit
        );
        assert_eq!(
            h.seal(b"no plaintext fallback").unwrap_err(),
            SessionCryptoError::AuthenticationLimit
        );
        assert_eq!(
            h.open(&c.seal(b"valid but retired").unwrap()).unwrap_err(),
            SessionCryptoError::AuthenticationLimit
        );
    }
    #[test]
    fn record_size_is_bounded() {
        let (c, h) = pair();
        assert_eq!(
            c.seal(&vec![0; MAX_RECORD_PLAINTEXT + 1]).unwrap_err(),
            SessionCryptoError::OversizedRecord
        );
        assert_eq!(
            h.open(&vec![0; MAX_RECORD_PLAINTEXT + RECORD_HEADER_LEN + 17])
                .unwrap_err(),
            SessionCryptoError::OversizedRecord
        );
    }
    #[test]
    fn replay_window_word_boundaries() {
        for shift in [1, 63, 64, 65, 127, 128, 4095, 4096, 4097] {
            let mut w = ReplayWindow::default();
            w.insert(1);
            w.insert(65);
            w.insert(65 + shift);
            assert!(w.contains(65 + shift));
            assert!(w.contains(65));
            assert!(!w.contains(66 + shift));
        }
    }
}
