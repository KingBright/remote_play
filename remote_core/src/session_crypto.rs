use hmac::{Hmac, Mac};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use sha2::Sha256;
use std::error::Error;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const KEY_CONTEXT: &[u8] = b"remote-play-session-v1";
const HELLO_CONTEXT: &[u8] = b"hello";
const ACCEPT_CONTEXT: &[u8] = b"accept";
const MAX_HELLO_SKEW_MS: u64 = 60_000;
pub const MULTIPLEX_ENCRYPTED: u8 = 0x07;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCryptoError {
    InvalidKey,
    Seal,
    Open,
    ShortPacket,
    InvalidMac,
    TimestampSkew,
}

impl fmt::Display for SessionCryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionCryptoError::InvalidKey => write!(f, "session crypto key is invalid"),
            SessionCryptoError::Seal => write!(f, "failed to seal session packet"),
            SessionCryptoError::Open => write!(f, "failed to open session packet"),
            SessionCryptoError::ShortPacket => write!(f, "encrypted packet is truncated"),
            SessionCryptoError::InvalidMac => write!(f, "session handshake mac mismatch"),
            SessionCryptoError::TimestampSkew => write!(f, "session handshake timestamp is stale"),
        }
    }
}

impl Error for SessionCryptoError {}

pub struct SessionCrypto {
    key: LessSafeKey,
}

impl SessionCrypto {
    pub fn from_psk(psk: &[u8], salt: &[u8]) -> Result<Self, SessionCryptoError> {
        let key_bytes = derive_key(psk, salt);
        let unbound =
            UnboundKey::new(&CHACHA20_POLY1305, &key_bytes).map_err(|_| SessionCryptoError::InvalidKey)?;
        Ok(Self {
            key: LessSafeKey::new(unbound),
        })
    }

    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SessionCryptoError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        fill_random(&mut nonce_bytes)?;
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        let mut buf = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(nonce, Aad::empty(), &mut buf)
            .map_err(|_| SessionCryptoError::Seal)?;
        let mut out = Vec::with_capacity(NONCE_LEN + buf.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&buf);
        Ok(out)
    }

    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SessionCryptoError> {
        if sealed.len() < NONCE_LEN + CHACHA20_POLY1305.tag_len() {
            return Err(SessionCryptoError::ShortPacket);
        }
        let mut nonce_bytes = [0u8; NONCE_LEN];
        nonce_bytes.copy_from_slice(&sealed[..NONCE_LEN]);
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        let mut buf = sealed[NONCE_LEN..].to_vec();
        let plain = self
            .key
            .open_in_place(nonce, Aad::empty(), &mut buf)
            .map_err(|_| SessionCryptoError::Open)?;
        Ok(plain.to_vec())
    }
}

pub fn derive_key(psk: &[u8], salt: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(psk).expect("hmac key");
    mac.update(KEY_CONTEXT);
    mac.update(salt);
    let bytes = mac.finalize().into_bytes();
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    key
}

pub fn mac_session_hello(psk: &[u8], nonce: &[u8; 16], timestamp_ms: u64) -> [u8; 32] {
    keyed_mac(psk, HELLO_CONTEXT, nonce, timestamp_ms)
}

pub fn mac_session_accept(psk: &[u8], salt: &[u8; 16], timestamp_ms: u64) -> [u8; 32] {
    keyed_mac(psk, ACCEPT_CONTEXT, salt, timestamp_ms)
}

pub fn verify_session_mac(
    expected: &[u8; 32],
    actual: &[u8; 32],
    timestamp_ms: u64,
    now_ms: u64,
) -> Result<(), SessionCryptoError> {
    if expected != actual {
        return Err(SessionCryptoError::InvalidMac);
    }
    let skew = now_ms.abs_diff(timestamp_ms);
    if skew > MAX_HELLO_SKEW_MS {
        return Err(SessionCryptoError::TimestampSkew);
    }
    Ok(())
}

pub fn load_session_psk() -> Option<Vec<u8>> {
    std::env::var("REMOTE_PLAY_SESSION_PSK")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(String::into_bytes)
}

pub fn require_session_auth() -> bool {
    load_session_psk().is_some()
        || std::env::var("REMOTE_PLAY_REQUIRE_AUTH").is_ok_and(|value| {
            matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES")
        })
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

fn keyed_mac(psk: &[u8], context: &[u8], material: &[u8], timestamp_ms: u64) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(psk).expect("hmac key");
    mac.update(context);
    mac.update(material);
    mac.update(&timestamp_ms.to_le_bytes());
    let bytes = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_seal_open() {
        let crypto = SessionCrypto::from_psk(b"shared-secret", b"salt-1").unwrap();
        let sealed = crypto.seal(b"rtp-payload").unwrap();
        assert_ne!(&sealed[NONCE_LEN..], b"rtp-payload");
        assert_eq!(crypto.open(&sealed).unwrap(), b"rtp-payload");
    }

    #[test]
    fn wrong_key_fails_open() {
        let a = SessionCrypto::from_psk(b"shared-secret", b"salt-1").unwrap();
        let b = SessionCrypto::from_psk(b"other-secret", b"salt-1").unwrap();
        let sealed = a.seal(b"hello").unwrap();
        assert_eq!(b.open(&sealed).unwrap_err(), SessionCryptoError::Open);
    }

    #[test]
    fn hello_mac_rejects_tamper_and_skew() {
        let psk = b"psk";
        let nonce = [7u8; 16];
        let ts = 1_000_000;
        let mac = mac_session_hello(psk, &nonce, ts);
        assert!(verify_session_mac(&mac, &mac, ts, ts).is_ok());
        let mut bad = mac;
        bad[0] ^= 1;
        assert_eq!(
            verify_session_mac(&mac, &bad, ts, ts).unwrap_err(),
            SessionCryptoError::InvalidMac
        );
        assert_eq!(
            verify_session_mac(&mac, &mac, ts, ts + MAX_HELLO_SKEW_MS + 1).unwrap_err(),
            SessionCryptoError::TimestampSkew
        );
    }
}
