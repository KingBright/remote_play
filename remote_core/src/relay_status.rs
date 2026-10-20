//! Local, credential-free observations. A registration write is not a server ACK.
use serde::Serialize;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayConnectionState {
    #[default]
    Idle,
    Connecting,
    Connected,
    RetryWait,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayFailure {
    Timeout,
    Connection,
    Tls,
    Protocol,
    Closed,
    Other,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RelayTunnelSnapshot {
    pub state: RelayConnectionState,
    pub attempts: u64,
    pub registrations_sent: u64,
    pub pings_sent: u64,
    pub pongs_received: u64,
    pub data_frames_received: u64,
    pub last_pong_age_ms: Option<u64>,
    pub last_error: Option<RelayFailure>,
}

#[derive(Debug, Default)]
struct Stored {
    value: RelayTunnelSnapshot,
    last_pong: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct RelayTunnelDiagnostics {
    inner: Mutex<Stored>,
}
impl RelayTunnelDiagnostics {
    fn update(&self, f: impl FnOnce(&mut Stored)) {
        f(&mut self.inner.lock().unwrap_or_else(|e| e.into_inner()));
    }
    pub fn snapshot(&self) -> RelayTunnelSnapshot {
        let stored = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut result = stored.value.clone();
        result.last_pong_age_ms = stored
            .last_pong
            .map(|t| t.elapsed().as_millis().min(u64::MAX as u128) as u64);
        result
    }
    pub(crate) fn attempt(&self) {
        self.update(|s| {
            s.value.state = RelayConnectionState::Connecting;
            s.value.attempts = s.value.attempts.saturating_add(1);
            s.value.last_error = None;
            s.last_pong = None;
        });
    }
    pub(crate) fn connected(&self) {
        self.update(|s| s.value.state = RelayConnectionState::Connected);
    }
    pub(crate) fn registration_sent(&self) {
        self.update(|s| s.value.registrations_sent = s.value.registrations_sent.saturating_add(1));
    }
    pub(crate) fn ping_sent(&self) {
        self.update(|s| s.value.pings_sent = s.value.pings_sent.saturating_add(1));
    }
    pub(crate) fn pong_received(&self) {
        self.update(|s| {
            s.value.pongs_received = s.value.pongs_received.saturating_add(1);
            s.last_pong = Some(Instant::now());
        });
    }
    pub(crate) fn data_received(&self) {
        self.update(|s| {
            s.value.data_frames_received = s.value.data_frames_received.saturating_add(1)
        });
    }
    pub(crate) fn retry(&self, failure: RelayFailure) {
        self.update(|s| {
            s.value.state = RelayConnectionState::RetryWait;
            s.value.last_error = Some(failure);
            s.last_pong = None;
        });
    }
    pub(crate) fn stopped(&self) {
        self.update(|s| {
            s.value.state = RelayConnectionState::Stopped;
            s.last_pong = None;
        });
    }
}

pub(crate) fn io_failure(error: &std::io::Error) -> RelayFailure {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::TimedOut => RelayFailure::Timeout,
        ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe => {
            RelayFailure::Closed
        }
        ErrorKind::ConnectionRefused
        | ErrorKind::AddrNotAvailable
        | ErrorKind::NotConnected
        | ErrorKind::NetworkUnreachable
        | ErrorKind::HostUnreachable => RelayFailure::Connection,
        _ => RelayFailure::Other,
    }
}
pub(crate) fn websocket_failure(error: &tokio_tungstenite::tungstenite::Error) -> RelayFailure {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Tls(_) => RelayFailure::Tls,
        Error::Io(error) => io_failure(error),
        Error::Protocol(_) | Error::Http(_) | Error::HttpFormat(_) => RelayFailure::Protocol,
        Error::ConnectionClosed | Error::AlreadyClosed => RelayFailure::Closed,
        _ => RelayFailure::Other,
    }
}

pub(crate) struct StopObservation(pub std::sync::Arc<RelayTunnelDiagnostics>);
impl Drop for StopObservation {
    fn drop(&mut self) {
        self.0.stopped();
    }
}
