//! Credential-free observations of discovery transport and filtering.
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryStamp {
    pub runtime_generation: u64,
    pub sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryFailure {
    Startup,
    Encode,
    Send,
    Receive,
    Route,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoveryTraffic {
    pub sends: u64,
    pub send_errors: u64,
    pub received: u64,
    pub decoded: u64,
    pub accepted: u64,
    pub rejected_format: u64,
    pub rejected_network: u64,
    pub rejected_self: u64,
    pub rejected_route: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoverySnapshot {
    pub running: bool,
    pub udp: DiscoveryTraffic,
    pub relay: DiscoveryTraffic,
    pub encode_errors: u64,
    pub receive_errors: u64,
    pub last_error: Option<DiscoveryFailure>,
}

#[derive(Debug)]
pub struct DiscoveryDiagnostics {
    generation: u64,
    sequence: Arc<AtomicU64>,
    state: Mutex<DiscoverySnapshot>,
}

impl DiscoveryDiagnostics {
    pub fn new(generation: u64, sequence: Arc<AtomicU64>) -> Self {
        Self {
            generation,
            sequence,
            state: Mutex::new(DiscoverySnapshot::default()),
        }
    }
    pub fn snapshot(&self) -> DiscoverySnapshot {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn next_stamp(&self) -> DiscoveryStamp {
        DiscoveryStamp {
            runtime_generation: self.generation,
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
        }
    }
    pub(crate) fn traffic(&self, relay: bool, update: impl FnOnce(&mut DiscoveryTraffic)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        update(if relay {
            &mut state.relay
        } else {
            &mut state.udp
        });
    }
    pub(crate) fn running(&self, value: bool) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).running = value;
    }
    pub(crate) fn failure(&self, failure: DiscoveryFailure) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.last_error = Some(failure);
        match failure {
            DiscoveryFailure::Encode => state.encode_errors += 1,
            DiscoveryFailure::Receive => state.receive_errors += 1,
            _ => {}
        }
    }
}

pub(crate) struct StopObservation(pub Option<Arc<DiscoveryDiagnostics>>);
impl Drop for StopObservation {
    fn drop(&mut self) {
        if let Some(diagnostics) = &self.0 {
            diagnostics.running(false);
        }
    }
}
