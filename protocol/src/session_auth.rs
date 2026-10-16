//! Single-version authenticated session establishment. No wall-clock values are
//! carried or used to authorize a session.
use serde::{Deserialize, Serialize};

pub const AUTH_VERSION: u32 = 3;
pub const AUTH_SUITE: u16 = 1; // HMAC-SHA256 / ChaCha20-Poly1305

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transcript {
    pub version: u32,
    pub suite: u16,
    pub client_nonce: [u8; 16],
    pub server_nonce: [u8; 16],
    pub session_id: [u8; 16],
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RejectCode {
    ProtocolMismatch,
    Busy,
    Capacity,
    Expired,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum HandshakeMessage {
    Hello {
        version: u32,
        suite: u16,
        client_nonce: [u8; 16],
        mac: [u8; 32],
    },
    Challenge {
        transcript: Transcript,
        mac: [u8; 32],
    },
    ClientFinish {
        transcript: Transcript,
        mac: [u8; 32],
    },
    ServerFinish {
        transcript: Transcript,
        mac: [u8; 32],
    },
    Reject {
        version: u32,
        client_nonce: [u8; 16],
        code: RejectCode,
        mac: [u8; 32],
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClockProbe {
    pub session_id: [u8; 16],
    pub probe_id: u64,
    pub client_send_us: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClockReply {
    pub probe: ClockProbe,
    pub host_recv_us: u64,
    pub host_send_us: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_handshake_and_clock_messages_roundtrip() {
        let t = Transcript {
            version: AUTH_VERSION,
            suite: AUTH_SUITE,
            client_nonce: [1; 16],
            server_nonce: [2; 16],
            session_id: [3; 16],
        };
        let messages = [
            HandshakeMessage::Hello {
                version: AUTH_VERSION,
                suite: AUTH_SUITE,
                client_nonce: t.client_nonce,
                mac: [4; 32],
            },
            HandshakeMessage::Challenge {
                transcript: t,
                mac: [4; 32],
            },
            HandshakeMessage::ClientFinish {
                transcript: t,
                mac: [4; 32],
            },
            HandshakeMessage::ServerFinish {
                transcript: t,
                mac: [4; 32],
            },
            HandshakeMessage::Reject {
                version: AUTH_VERSION,
                client_nonce: t.client_nonce,
                code: RejectCode::ProtocolMismatch,
                mac: [4; 32],
            },
        ];
        for message in messages {
            let control = crate::ControlMessage::SessionHandshake(Box::new(message.clone()));
            let decoded = crate::ControlMessage::decode(&control.encode().unwrap()).unwrap();
            assert!(
                matches!(decoded,crate::ControlMessage::SessionHandshake(actual)if *actual==message)
            );
        }
        let probe = ClockProbe {
            session_id: t.session_id,
            probe_id: u64::MAX,
            client_send_us: u64::MAX,
        };
        let reply = ClockReply {
            probe,
            host_recv_us: 7,
            host_send_us: 9,
        };
        for control in [
            crate::ControlMessage::ClockProbe(probe),
            crate::ControlMessage::ClockReply(reply),
        ] {
            let encoded = control.encode().unwrap();
            assert_eq!(
                crate::ControlMessage::decode(&encoded)
                    .unwrap()
                    .encode()
                    .unwrap(),
                encoded
            );
        }
    }
}
