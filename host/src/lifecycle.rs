//! Scalar process-local diagnostics. No frame, title, credential or input data.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst},
    },
};

#[derive(Default)]
pub(crate) struct Resources {
    pub workers: AtomicUsize,
    pub native_streams: AtomicUsize,
    pub native_stops_completed: AtomicU64,
    pub native_stop_errors: AtomicU64,
    pub captured_frames: AtomicU64,
    pub encoded_frames: AtomicU64,
}
pub(crate) struct WorkerGuard(pub Arc<Resources>);
impl WorkerGuard {
    pub fn new(resources: Arc<Resources>) -> Self {
        resources.workers.fetch_add(1, SeqCst);
        Self(resources)
    }
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.0.workers.fetch_sub(1, SeqCst);
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ConnectionLifecycleSnapshot {
    pub connection_id: u32,
    pub state: String,
    pub observed_at_ms: u64,
    pub active_capture_workers: usize,
    pub native_streams: usize,
    pub native_stops_completed: u64,
    pub native_stop_errors: u64,
    pub captured_frames: u64,
    pub encoded_frames: u64,
}
struct Record {
    resources: Arc<Resources>,
    state: String,
    at: u64,
}
fn registry() -> &'static Mutex<HashMap<u32, Vec<Record>>> {
    static REG: OnceLock<Mutex<HashMap<u32, Vec<Record>>>> = OnceLock::new();
    REG.get_or_init(Default::default)
}
pub(crate) fn publish(id: u32, resources: &Arc<Resources>, state: &str) {
    let now = remote_core::session_crypto::now_unix_ms();
    let mut reg = registry().lock().unwrap();
    // Keep active records; bounded terminal retention. ID collisions remain
    // separate records, never falsely identify one user's session as another.
    reg.retain(|_, records| {
        records.retain(|r| {
            r.state == "open" || r.state == "closing" || now.saturating_sub(r.at) < 60_000
        });
        !records.is_empty()
    });
    if reg.len() >= 128 && !reg.contains_key(&id) {
        return;
    }
    let records = reg.entry(id).or_default();
    if let Some(record) = records
        .iter_mut()
        .find(|r| Arc::ptr_eq(&r.resources, resources))
    {
        record.state = state.into();
        record.at = now;
    } else if records.len() < 8 {
        records.push(Record {
            resources: resources.clone(),
            state: state.into(),
            at: now,
        });
    }
}
/// Same-user IPC can query its known connection ID without enumerating sessions.
pub fn inspect_connection(id: u32) -> Vec<ConnectionLifecycleSnapshot> {
    let now = remote_core::session_crypto::now_unix_ms();
    let reg = registry().lock().unwrap();
    reg.get(&id)
        .into_iter()
        .flatten()
        .filter(|r| r.state == "open" || r.state == "closing" || now.saturating_sub(r.at) < 60_000)
        .map(|r| {
            let x = &r.resources;
            ConnectionLifecycleSnapshot {
                connection_id: id,
                state: r.state.clone(),
                observed_at_ms: r.at,
                active_capture_workers: x.workers.load(SeqCst),
                native_streams: x.native_streams.load(SeqCst),
                native_stops_completed: x.native_stops_completed.load(SeqCst),
                native_stop_errors: x.native_stop_errors.load(SeqCst),
                captured_frames: x.captured_frames.load(SeqCst),
                encoded_frames: x.encoded_frames.load(SeqCst),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_connection_does_not_enumerate_and_expired_terminal_is_hidden() {
        assert!(inspect_connection(987654).is_empty());
        let resource = Arc::new(Resources::default());
        publish(987655, &resource, "closed");
        registry().lock().unwrap().get_mut(&987655).unwrap()[0].at = 0;
        assert!(inspect_connection(987655).is_empty());
    }
}
