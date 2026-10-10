//! Per-GPUI-window resource owner. Scalar state never contains FD/portal grants.
use super::{ShareKind, TargetRow};
use host::service::{LocalPortalShareCommand, LocalPortalShareTarget};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};

struct Selection {
    key: String,
    cancel: watch::Sender<bool>,
    revoked: Option<watch::Receiver<bool>>,
}
#[derive(Default)]
struct OwnedState {
    targets: HashMap<String, LocalPortalShareTarget>,
    selections: HashMap<u64, Selection>,
    refresh: Option<u64>,
}
impl Drop for OwnedState {
    fn drop(&mut self) {
        for selection in self.selections.values() {
            selection.cancel.send_replace(true);
        }
    }
}
pub(crate) struct Owner {
    control: Option<mpsc::Sender<LocalPortalShareCommand>>,
    state: Arc<Mutex<OwnedState>>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        // Cancellation is immediate even if a completion briefly upgraded Weak.
        for selection in self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .selections
            .values()
        {
            selection.cancel.send_replace(true);
        }
    }
}
impl Owner {
    pub fn new(control: Option<mpsc::Sender<LocalPortalShareCommand>>) -> Self {
        Self {
            control,
            state: Arc::new(Mutex::new(OwnedState::default())),
        }
    }
    pub fn refresh(
        &self,
        receipt: u64,
    ) -> impl std::future::Future<Output = Result<Vec<TargetRow>, String>> + Send + 'static {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).refresh = Some(receipt);
        let control = self.control.clone();
        let state = Arc::downgrade(&self.state);
        async move {
            let control = control.ok_or("Sharing service is unavailable")?;
            let (reply, rx) = oneshot::channel();
            tokio::time::timeout(
                Duration::from_secs(5),
                control.send(LocalPortalShareCommand::Targets { reply }),
            )
            .await
            .map_err(|_| "Viewer refresh timed out")?
            .map_err(|_| "Sharing service stopped")?;
            let mut targets = tokio::time::timeout(Duration::from_secs(5), rx)
                .await
                .map_err(|_| "Viewer refresh timed out")?
                .map_err(|_| "Sharing service stopped")?;
            targets.sort_by_key(LocalPortalShareTarget::local_key);
            let rows = targets
                .iter()
                .map(|target| TargetRow {
                    key: target.local_key(),
                    label: target.label(),
                })
                .collect();
            let state = state.upgrade().ok_or("Sharing window closed")?;
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            if state.refresh != Some(receipt) {
                return Err("Viewer refresh superseded".into());
            }
            state.refresh = None;
            state.targets = targets
                .into_iter()
                .map(|target| (target.local_key(), target))
                .collect();
            Ok(rows)
        }
    }
    pub fn active_keys(&self) -> Vec<String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .selections
            .retain(|_, selection| selection.revoked.as_ref().is_none_or(|rx| !*rx.borrow()));
        state
            .selections
            .values()
            .filter(|selection| selection.revoked.is_some())
            .map(|selection| selection.key.clone())
            .collect()
    }
    pub fn cancel(&self, attempt: u64) {
        if let Some(selection) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .selections
            .remove(&attempt)
        {
            selection.cancel.send_replace(true);
        }
    }
    pub fn stop(&self, key: &str) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .selections
            .retain(|_, selection| {
                if selection.key == key {
                    selection.cancel.send_replace(true);
                    false
                } else {
                    true
                }
            });
    }
    pub fn select(
        &self,
        attempt: u64,
        key: String,
        kind: ShareKind,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send + 'static {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).refresh = None;
        let control = self.control.clone();
        let state = Arc::downgrade(&self.state);
        let target = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .targets
            .get(&key)
            .cloned();
        let (cancel, rx) = watch::channel(false);
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .selections
            .insert(
                attempt,
                Selection {
                    key,
                    cancel,
                    revoked: None,
                },
            );
        async move {
            let result = select(control, target, kind, rx, state.clone(), attempt).await;
            if result.is_err()
                && let Some(state) = state.upgrade()
                && let Some(selection) = state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .selections
                    .remove(&attempt)
            {
                selection.cancel.send_replace(true);
            }
            result
        }
    }
}
async fn select(
    control: Option<mpsc::Sender<LocalPortalShareCommand>>,
    target: Option<LocalPortalShareTarget>,
    kind: ShareKind,
    cancel: watch::Receiver<bool>,
    state: Weak<Mutex<OwnedState>>,
    attempt: u64,
) -> Result<(), String> {
    let control = control.ok_or("Sharing service is unavailable")?;
    let target = target.ok_or("Viewer changed; refresh the viewer list")?;
    let picker = match kind {
        ShareKind::Window => host::PickerSources::Window,
        ShareKind::Display => host::PickerSources::Monitor,
    };
    // Empty parent is explicitly supported by ScreenCast. A Wayland pointer is
    // not an exported xdg-foreign handle; never fabricate one or add GTK.
    let lease = host::request_local_portal_capture("", picker, cancel)
        .await
        .map_err(selection_message)?;
    let capture = host::prepare_pipewire_capture(lease)
        .await
        .map_err(selection_message)?;
    let owned = state.upgrade().ok_or("Sharing window closed")?;
    {
        let mut owned = owned.lock().unwrap_or_else(|e| e.into_inner());
        let selection = owned
            .selections
            .get_mut(&attempt)
            .ok_or("Selection cancelled")?;
        if *selection.cancel.borrow() {
            capture.revoke();
            return Err("Selection cancelled".into());
        }
        selection.revoked = Some(capture.revocation());
    }
    drop(owned);
    let (reply, rx) = oneshot::channel();
    tokio::time::timeout(
        Duration::from_secs(5),
        control.send(LocalPortalShareCommand::Commit {
            target,
            capture,
            reply,
        }),
    )
    .await
    .map_err(|_| "Share commit timed out")?
    .map_err(|_| "Sharing service stopped")?;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .map_err(|_| "Share commit timed out")?
        .map_err(|_| "Sharing service stopped")??;
    Ok(())
}

fn selection_message(error: host::PortalCallError) -> String {
    use host::PortalCallError::*;
    match error {
        Cancelled => "Selection cancelled.",
        Revoked => "Sharing was closed by the system. Select a source again.",
        Timeout => "Sharing did not become ready in time. Try selecting again.",
        Capabilities => "This system cannot share that kind of source.",
        Parent => "The sharing window could not be identified.",
        Response => "The system selector returned an incomplete result. Try again.",
        InvalidSource => {
            "This source has an unsupported format or color description. Try another source."
        }
        Generation => "The selection expired. Select the source again.",
        Transport => "The system sharing service is unavailable.",
    }
    .into()
}
