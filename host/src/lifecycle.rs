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
    pub native_stops_pending: AtomicUsize,
    pub native_stops_completed: AtomicU64,
    pub native_stop_errors: AtomicU64,
    pub captured_frames: AtomicU64,
    pub encoded_frames: AtomicU64,
}

pub(crate) const CLEANUP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// A receipt is observation only. Dropping it cannot release a native owner.
#[derive(Clone)]
pub(crate) struct NativeStopReceipt(Arc<Mutex<Option<Result<(), String>>>>);
pub(crate) struct NativeStopCompletion {
    receipt: NativeStopReceipt,
    resources: Option<Arc<Resources>>,
}
impl NativeStopReceipt {
    pub(crate) async fn wait(&self) -> Result<(), String> {
        tokio::time::timeout(CLEANUP_WAIT, async {
            loop {
                if let Some(result) = self.0.lock().unwrap().clone() {
                    return result;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "native stop completion is unknown; owner remains retained".to_string())?
    }
}
impl NativeStopCompletion {
    pub(crate) fn new(resources: Option<Arc<Resources>>) -> (NativeStopReceipt, Self) {
        if let Some(x) = &resources {
            x.native_stops_pending.fetch_add(1, SeqCst);
        }
        let receipt = NativeStopReceipt(Arc::new(Mutex::new(None)));
        (receipt.clone(), Self { receipt, resources })
    }
    /// Success requires actual native owner destruction; error retains the owner.
    pub(crate) fn complete(self, result: Result<(), String>) {
        if let Some(x) = &self.resources {
            if result.is_ok() {
                x.native_stops_completed.fetch_add(1, SeqCst);
                x.native_streams.fetch_sub(1, SeqCst);
            } else {
                x.native_stop_errors.fetch_add(1, SeqCst);
            }
            x.native_stops_pending.fetch_sub(1, SeqCst);
        }
        *self.receipt.0.lock().unwrap() = Some(result);
    }
}
pub(crate) async fn wait_native_stops(resources: &Resources) {
    while resources.native_stops_pending.load(SeqCst) != 0 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
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
    pub native_stops_pending: usize,
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
            r.state == "open"
                || r.state == "closing"
                || r.resources.workers.load(SeqCst) != 0
                || r.resources.native_streams.load(SeqCst) != 0
                || now.saturating_sub(r.at) < 60_000
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
        .filter(|r| {
            r.state == "open"
                || r.state == "closing"
                || r.resources.workers.load(SeqCst) != 0
                || r.resources.native_streams.load(SeqCst) != 0
                || now.saturating_sub(r.at) < 60_000
        })
        .map(|r| {
            let x = &r.resources;
            ConnectionLifecycleSnapshot {
                connection_id: id,
                state: r.state.clone(),
                observed_at_ms: r.at,
                active_capture_workers: x.workers.load(SeqCst),
                native_streams: x.native_streams.load(SeqCst),
                native_stops_pending: x.native_stops_pending.load(SeqCst),
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
    #[cfg(target_os = "macos")]
    struct CallbackOwner(Arc<std::sync::atomic::AtomicBool>);
    #[cfg(target_os = "macos")]
    impl Drop for CallbackOwner {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
        }
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn actual_owned_callback_bridge_withheld_completion_keeps_owner_without_runtime_waiter() {
        use screencapturekit::utils::completion::OwnedUnitCompletion;
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let resources = Arc::new(Resources::default());
        resources.native_streams.store(1, SeqCst);
        let (receipt, completion) = NativeStopCompletion::new(Some(resources.clone()));
        let context = OwnedUnitCompletion::new(CallbackOwner(dropped.clone()), move |result| {
            completion.complete(result)
        });
        assert!(receipt.wait().await.unwrap_err().contains("unknown"));
        assert!(!dropped.load(SeqCst));
        assert_eq!(resources.native_streams.load(SeqCst), 1);
        assert_eq!(resources.native_stops_pending.load(SeqCst), 1);
        // The same runtime thread still schedules unrelated IPC/timer work.
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            tokio::time::sleep(std::time::Duration::from_millis(5)),
        )
        .await
        .unwrap();
        unsafe {
            OwnedUnitCompletion::<CallbackOwner>::complete_context(context, true, std::ptr::null());
        }
        receipt.wait().await.unwrap();
        assert!(dropped.load(SeqCst));
        assert_eq!(resources.native_streams.load(SeqCst), 0);
        assert_eq!(resources.native_stops_completed.load(SeqCst), 1);
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn actual_owned_callback_bridge_error_keeps_owner_and_failure_counts() {
        use screencapturekit::utils::completion::OwnedUnitCompletion;
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let resources = Arc::new(Resources::default());
        resources.native_streams.store(1, SeqCst);
        let (receipt, completion) = NativeStopCompletion::new(Some(resources.clone()));
        let context = OwnedUnitCompletion::new(CallbackOwner(dropped.clone()), move |result| {
            completion.complete(result)
        });
        unsafe {
            OwnedUnitCompletion::<CallbackOwner>::complete_context(
                context,
                false,
                c"test native failure".as_ptr(),
            );
        }
        assert!(receipt.wait().await.is_err());
        assert!(receipt.wait().await.is_err());
        assert!(!dropped.load(SeqCst));
        assert_eq!(resources.native_streams.load(SeqCst), 1);
        assert_eq!(resources.native_stops_pending.load(SeqCst), 0);
        assert_eq!(resources.native_stop_errors.load(SeqCst), 1);
        // Release only this synthetic owner after asserting the production
        // bridge's retained-error behavior. No SCStream or OS capture exists.
        unsafe {
            drop(Box::from_raw(
                context.cast::<OwnedUnitCompletion<CallbackOwner>>(),
            ));
        }
        assert!(dropped.load(SeqCst));
    }
    #[test]
    fn unknown_connection_does_not_enumerate_and_expired_terminal_is_hidden() {
        assert!(inspect_connection(987654).is_empty());
        let resource = Arc::new(Resources::default());
        publish(987655, &resource, "closed");
        registry().lock().unwrap().get_mut(&987655).unwrap()[0].at = 0;
        assert!(inspect_connection(987655).is_empty());
    }
}
