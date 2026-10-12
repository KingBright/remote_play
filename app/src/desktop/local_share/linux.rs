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
    target: Option<LocalPortalShareTarget>,
    cancel: watch::Sender<bool>,
    revoked: Option<watch::Receiver<bool>>,
}
struct SavedRestoration {
    target: LocalPortalShareTarget,
    kind: ShareKind,
    token: host::PortalSessionRestore,
}
#[derive(Default)]
struct OwnedState {
    targets: HashMap<String, LocalPortalShareTarget>,
    selections: HashMap<u64, Selection>,
    refresh: Option<u64>,
    restorations: HashMap<String, SavedRestoration>,
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
            let next_targets: HashMap<_, _> = targets
                .into_iter()
                .map(|target| (target.local_key(), target))
                .collect();
            state.restorations.retain(|key, saved| {
                saved.token.is_usable() && next_targets.get(key)
                    .is_some_and(|target| saved.target.same_authenticated_target(target))
            });
            state.selections.retain(|_, selection| {
                let current = next_targets.get(&selection.key).is_some_and(|target| {
                    selection.target.as_ref()
                        .is_some_and(|previous| previous.same_authenticated_target(target))
                });
                if !current { selection.cancel.send_replace(true); }
                current
            });
            state.targets = next_targets;
            Ok(rows)
        }
    }
    pub fn active_keys(&self) -> Vec<String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .selections
            .retain(|_, selection| selection.revoked.as_ref().is_none_or(|rx| !*rx.borrow()));
        state.restorations.retain(|_, saved| saved.token.is_usable());
        state
            .selections
            .values()
            .filter(|selection| selection.revoked.is_some())
            .map(|selection| selection.key.clone())
            .collect()
    }
    pub fn cancel(&self, attempt: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(selection) = state.selections.remove(&attempt) {
            state.restorations.remove(&selection.key);
            selection.cancel.send_replace(true);
        }
    }
    pub fn stop(&self, key: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.restorations.remove(key);
        state.selections.retain(|_, selection| {
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
        self.select_source(attempt, key, kind, true)
    }
    pub fn select_source(
        &self, attempt: u64, key: String, kind: ShareKind, reuse: bool,
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
        // Remove before any await: a single-use Portal token cannot be replayed
        // by concurrent attempts, and an address/name never supplies authority.
        let restoration = self.state.lock().unwrap_or_else(|e| e.into_inner())
            .restorations.remove(&key).filter(|saved| reuse && saved.kind == kind
                && saved.token.is_usable() && target.as_ref()
                    .is_some_and(|target| saved.target.same_authenticated_target(target)))
            .map(|saved| saved.token);
        let (cancel, rx) = watch::channel(false);
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .selections
            .insert(
                attempt,
                Selection {
                    key,
                    target: target.clone(),
                    cancel,
                    revoked: None,
                },
            );
        async move {
            let result = select(control, target, kind, rx, state.clone(), attempt, restoration).await;
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
    restoration: Option<host::PortalSessionRestore>,
) -> Result<(), String> {
    let control = control.ok_or("Sharing service is unavailable")?;
    let previous = target.ok_or("Viewer changed; refresh the viewer list")?;
    // Revalidate live server-derived authority before asking the system portal,
    // rather than relying on a row retained from an earlier refresh.
    let (reply, current) = oneshot::channel();
    tokio::time::timeout(Duration::from_secs(5), control.send(LocalPortalShareCommand::Targets { reply }))
        .await.map_err(|_| "Viewer check timed out")?.map_err(|_| "Sharing service stopped")?;
    let current = tokio::time::timeout(Duration::from_secs(5), current).await
        .map_err(|_| "Viewer check timed out")?.map_err(|_| "Sharing service stopped")?;
    let target = current.into_iter().find(|target| previous.same_authenticated_target(target))
        .ok_or("Authenticated viewer changed; refresh the viewer list")?;
    let picker = match kind {
        ShareKind::Window => host::PickerSources::Window,
        ShareKind::Display => host::PickerSources::Monitor,
        ShareKind::ControlledDisplay => host::PickerSources::Monitor,
    };
    // Empty parent is explicitly supported by ScreenCast. A Wayland pointer is
    // not an exported xdg-foreign handle; never fabricate one or add GTK.
    let mut lease = host::request_local_portal_capture_restorable(
        "", picker, cancel, kind == ShareKind::ControlledDisplay, restoration,
    ).await.map_err(selection_message)?;
    let restoration = lease.take_restoration();
    let capture = host::prepare_pipewire_capture(lease)
        .await
        .map_err(selection_message)?;
    if kind == ShareKind::ControlledDisplay
        && let Err(error) = capture.input_status()
    {
        capture.revoke();
        return Err(error.to_string());
    }
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
            target: target.clone(),
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
    if let Some(token) = restoration {
        let state = state.upgrade().ok_or("Sharing window closed")?;
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        let key = target.local_key();
        if token.is_usable() && state.selections.get(&attempt)
            .is_some_and(|selection| !*selection.cancel.borrow())
            && state.targets.get(&key)
                .is_some_and(|current| target.same_authenticated_target(current)) {
            state.restorations.insert(key, SavedRestoration { target, kind, token });
        }
    }
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
