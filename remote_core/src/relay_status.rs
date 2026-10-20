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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayStage {
    #[default]
    Idle,
    Dns,
    Tcp,
    Tls,
    Websocket,
    Registration,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayFailure {
    Timeout,
    Connection,
    Tls,
    TlsCertificate,
    Protocol,
    Closed,
    Other,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RelayTunnelSnapshot {
    pub state: RelayConnectionState,
    pub stage: RelayStage,
    pub last_failure_stage: Option<RelayStage>,
    pub last_os_error: Option<i32>,
    pub http_status: Option<u16>,
    pub dns_results: u64,
    pub dns_address_sha256: Vec<String>,
    pub dns_benchmark_count: u64,
    pub tcp_connections: u64,
    pub tls_handshakes: u64,
    pub websocket_upgrades: u64,
    pub tls_bytes_sent: u64,
    pub tls_bytes_received: u64,
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
            s.value.stage = RelayStage::Dns;
            s.last_pong = None;
        });
    }
    pub(crate) fn connected(&self) {
        self.update(|s| {
            s.value.state = RelayConnectionState::Connected;
            s.value.last_error = None;
            s.value.last_failure_stage = None;
            s.value.last_os_error = None;
            s.value.http_status = Some(101);
            s.value.websocket_upgrades = s.value.websocket_upgrades.saturating_add(1);
            s.value.stage = RelayStage::Registration;
        });
    }
    pub(crate) fn stage(&self, stage: RelayStage) {
        self.update(|s| s.value.stage = stage);
    }
    pub(crate) fn dns_resolved(&self, addresses: &[std::net::SocketAddr]) {
        use sha2::Digest;
        self.update(|s| {
            s.value.dns_results = addresses.len() as u64;
            s.value.dns_address_sha256 = addresses.iter().take(16).map(|addr| {
                let bytes = match addr.ip() { std::net::IpAddr::V4(ip) => ip.octets().to_vec(), std::net::IpAddr::V6(ip) => ip.octets().to_vec() };
                format!("{:x}", sha2::Sha256::digest(bytes))
            }).collect();
            s.value.dns_benchmark_count = addresses.iter().filter(|addr| matches!(addr.ip(),
                std::net::IpAddr::V4(ip) if ip.octets()[0] == 198 && (ip.octets()[1] == 18 || ip.octets()[1] == 19))).count() as u64;
        });
    }
    pub(crate) fn tcp_connected(&self) {
        self.update(|s| s.value.tcp_connections = s.value.tcp_connections.saturating_add(1));
    }
    pub(crate) fn tls_connected(&self) {
        self.update(|s| s.value.tls_handshakes = s.value.tls_handshakes.saturating_add(1));
    }
    pub(crate) fn transport_bytes(&self, sent: bool, count: usize) {
        self.update(|s| {
            if s.value.stage == RelayStage::Tls {
                let counter = if sent {
                    &mut s.value.tls_bytes_sent
                } else {
                    &mut s.value.tls_bytes_received
                };
                *counter = counter.saturating_add(count as u64);
            }
        });
    }
    pub(crate) fn connection_error(&self, errno: Option<i32>, status: Option<u16>) {
        self.update(|s| {
            s.value.last_os_error = errno;
            s.value.http_status = status;
        });
    }
    pub(crate) fn registration_sent(&self) {
        self.update(|s| {
            s.value.registrations_sent = s.value.registrations_sent.saturating_add(1);
            s.value.stage = RelayStage::Active;
        });
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
            s.value.last_failure_stage = Some(s.value.stage);
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

/// Never serialize the IO error text: it may contain addresses or URLs.
pub(crate) fn tls_io_failure(error: &std::io::Error) -> RelayFailure {
    if let Some(rustls::Error::InvalidCertificate(_)) = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<rustls::Error>())
    {
        RelayFailure::TlsCertificate
    } else {
        match io_failure(error) {
            RelayFailure::Other => RelayFailure::Tls,
            failure => failure,
        }
    }
}
