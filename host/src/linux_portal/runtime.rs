//! Real ScreenCast control path. Each selection owns its bus connection and
//! cleanup task; cancellation never waits for a portal Response after Close.

use super::{
    ClosePlan, Method, PortalSessionState, RestrictedRemote, SelectionOptions, SourceKind,
};
use crate::linux_frame::{FrameMailbox, OwnedFrame};
use futures_util::StreamExt;
use protocol::session::{CaptureSource, CaptureSourceInfo};
use serde::Serialize;
use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::sync::{oneshot, watch};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Type, Value};
use zbus::{Connection, Proxy, proxy::SignalStream};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const SCREENCAST: &str = "org.freedesktop.portal.ScreenCast";
const REMOTE_DESKTOP: &str = "org.freedesktop.portal.RemoteDesktop";
mod input;
pub use input::PortalInputError;
const SESSION: &str = "org.freedesktop.portal.Session";
const REQUEST: &str = "org.freedesktop.portal.Request";
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
static GENERATION: AtomicU64 = AtomicU64::new(1);
type Values = HashMap<String, OwnedValue>;
type Result<T> = std::result::Result<T, PortalCallError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerSources {
    Monitor,
    Window,
    MonitorOrWindow,
}
impl PickerSources {
    fn mask(self) -> u32 {
        match self {
            Self::Monitor => 1,
            Self::Window => 2,
            Self::MonitorOrWindow => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalCallError {
    Cancelled,
    Revoked,
    Timeout,
    Transport,
    Capabilities,
    Parent,
    Response,
    InvalidSource,
    Generation,
}
impl std::fmt::Display for PortalCallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No remote error text, variant values, window titles or restore tokens.
        write!(formatter, "Linux portal capture: {self:?}")
    }
}
impl std::error::Error for PortalCallError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedPortalSource {
    pub generation: u64,
    pub kind: SourceKind,
    /// Session-local PipeWire node, never a persistent catalog/source ID.
    pub node_id: u32,
    /// Stable object serial where offered (ScreenCast v6), not a network ID.
    pub pipewire_serial: Option<u64>,
    /// Compositor logical geometry, never encoder/captured pixel dimensions.
    pub logical_size: Option<(i32, i32)>,
    pub logical_position: Option<(i32, i32)>,
    pub embedded_cursor: bool,
}

#[derive(Clone, Copy)]
struct Timeouts {
    operation: Duration,
    selection: Duration,
    cleanup: Duration,
}
impl Default for Timeouts {
    fn default() -> Self {
        Self {
            operation: Duration::from_secs(5),
            selection: Duration::from_secs(120),
            cleanup: Duration::from_millis(500),
        }
    }
}

struct Control {
    state: Mutex<PortalSessionState>,
    revoked: watch::Sender<bool>,
    frames: Mutex<Option<Arc<FrameMailbox>>>,
    input: Mutex<Option<Arc<input::PortalInput>>>,
}
impl Control {
    fn revoke(&self) -> ClosePlan {
        let plan = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .close();
        self.revoked.send_replace(true);
        if let Some(frames) = self
            .frames
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            frames.close();
        }
        plan
    }
}

struct LeaseOwner {
    control: Arc<Control>,
    close_tx: Option<oneshot::Sender<ClosePlan>>,
    done: Option<oneshot::Receiver<bool>>,
}
impl LeaseOwner {
    fn signal_close(&mut self) {
        let plan = self.control.revoke();
        if let Some(sender) = self.close_tx.take() {
            let _ = sender.send(plan);
        }
    }
}
impl Drop for LeaseOwner {
    fn drop(&mut self) {
        self.signal_close();
    }
}

/// Selection/FD ownership only. A native worker must validate the first format
/// before publishing this grant to a connection-scoped source catalog.
pub struct PortalCaptureLease {
    owner: LeaseOwner,
    remote: Option<RestrictedRemote>,
    selected: SelectedPortalSource,
    cancellation: watch::Receiver<bool>,
}
impl PortalCaptureLease {
    fn is_revoked(&self) -> bool {
        *self.cancellation.borrow()
            || self.cancellation.has_changed().is_err()
            || *self.owner.control.revoked.borrow()
    }
    pub fn selected_source(&self) -> SelectedPortalSource {
        self.selected
    }
    pub fn revocation(&self) -> watch::Receiver<bool> {
        self.owner.control.revoked.subscribe()
    }

    fn input_available(&self) -> bool {
        !self.is_revoked()
            && self
                .owner
                .control
                .input
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|input| input.available())
    }

    /// A restricted native worker calls this only after connect_fd and the first
    /// copied format. Logical portal geometry never supplies captured dimensions.
    pub fn prepare_owned_capture(self, first: OwnedFrame) -> Result<PreparedPortalCapture> {
        let mailbox = Arc::new(
            FrameMailbox::new(self.selected.generation).map_err(|_| PortalCallError::Generation)?,
        );
        self.prepare_with_mailbox(first, mailbox)
    }

    pub(crate) fn prepare_with_mailbox(
        self,
        first: OwnedFrame,
        mailbox: Arc<FrameMailbox>,
    ) -> Result<PreparedPortalCapture> {
        if self.is_revoked() {
            return Err(PortalCallError::Revoked);
        }
        crate::linux_raw_encode::validate_native_input(&first)
            .map_err(|_| PortalCallError::InvalidSource)?;
        if let Some(input) = self
            .owner
            .control
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            input.bind_frame(&first);
        }
        if first.stamp.generation != self.selected.generation
            || self
                .remote
                .as_ref()
                .is_none_or(|remote| remote.fd.is_some())
        {
            return Err(PortalCallError::Generation);
        }
        let id =
            u32::try_from(self.selected.generation).map_err(|_| PortalCallError::Generation)?;
        if id >= 0x8000_0000 {
            return Err(PortalCallError::Generation);
        }
        // Linux X11 resource IDs occupy the lower range. Process-local portal
        // IDs never expose node IDs and cannot alias the legacy X11 catalog.
        let id = id | 0x8000_0000;
        let source = match self.selected.kind {
            SourceKind::Monitor => CaptureSource::Display(id),
            SourceKind::Window => CaptureSource::Window(id),
        };
        let info = CaptureSourceInfo {
            source,
            title: match self.selected.kind {
                SourceKind::Monitor => "Shared display",
                SourceKind::Window => "Shared window",
            }
            .into(),
            application: String::new(),
            process_id: None,
            width: first.width,
            height: first.height,
            supports_input: self.input_available(),
        };
        {
            let mut state = self
                .owner
                .control
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state
                .set_streaming(self.selected.generation)
                .map_err(|_| PortalCallError::Revoked)?;
            *self
                .owner
                .control
                .frames
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(mailbox.clone());
            if !mailbox.publish_initial(first) {
                return Err(PortalCallError::Revoked);
            }
        }
        Ok(PreparedPortalCapture {
            inner: Arc::new(PreparedInner {
                lease: Mutex::new(self),
                mailbox,
                info: Mutex::new(info),
                worker: Mutex::new(None),
            }),
        })
    }

    /// Transfers the restricted FD exactly once. No global PipeWire remote is
    /// opened here. The eventual consumer must observe revocation and disconnect.
    pub fn take_pipewire_remote(&mut self) -> Result<(u32, OwnedFd)> {
        if self.is_revoked() {
            return Err(PortalCallError::Revoked);
        }
        let state = self
            .owner
            .control
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.remote
            .as_mut()
            .ok_or(PortalCallError::Revoked)?
            .take_for_connect(&state)
            .map_err(|_| PortalCallError::Revoked)
    }

    /// Invalidate delivery immediately; true additionally confirms the Close
    /// RPCs replied successfully. Drop schedules the same bounded cleanup.
    pub async fn close(&mut self) -> bool {
        self.remote = None;
        self.owner.signal_close();
        match self.owner.done.take() {
            Some(done) => tokio::time::timeout(Duration::from_secs(2), done)
                .await
                .ok()
                .and_then(|result| result.ok())
                .unwrap_or(false),
            None => false,
        }
    }
}

struct PreparedInner {
    lease: Mutex<PortalCaptureLease>,
    mailbox: Arc<FrameMailbox>,
    info: Mutex<CaptureSourceInfo>,
    worker: Mutex<Option<Box<dyn Send + Sync>>>,
}
impl Drop for PreparedInner {
    fn drop(&mut self) {
        self.mailbox.close();
    }
}

/// Process-local share resource. No PipeWire node IDs or restore tokens are
/// published; host service revalidates the authenticated connection at commit.
#[derive(Clone)]
pub struct PreparedPortalCapture {
    inner: Arc<PreparedInner>,
}
impl PreparedPortalCapture {
    /// Also observe local cancellation synchronously, before the cleanup actor
    /// has processed its watch notification or delivered Session.Closed.
    pub fn is_revoked(&self) -> bool {
        self.inner
            .lease
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_revoked()
    }
    pub(crate) fn attach_worker(&self, worker: Box<dyn Send + Sync>, done: oneshot::Receiver<()>) {
        *self.inner.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(worker);
        // Weak ownership prevents a worker/lease cycle. Unexpected worker exit
        // revokes the grant and schedules the existing bounded portal cleanup.
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let _ = done.await;
            if let Some(inner) = weak.upgrade() {
                inner
                    .lease
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .owner
                    .signal_close();
            }
        });
    }
    pub fn source_info(&self) -> CaptureSourceInfo {
        let mut info = self
            .inner
            .info
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some((width, height)) = self.inner.mailbox.dimensions() {
            info.width = width;
            info.height = height;
        }
        info.supports_input = self.input_available();
        info
    }
    /// This process-local route never uses the global desktop/uinput injector.
    /// The caller must first check authenticated peer, source ID and revision.
    pub fn input_available(&self) -> bool {
        self.input_status().is_ok()
    }
    pub fn input_status(&self) -> std::result::Result<(), PortalInputError> {
        if self.is_revoked() {
            return Err(PortalInputError::SessionClosed);
        }
        self.input().ok_or(PortalInputError::ViewOnly)?.status()
    }
    fn input(&self) -> Option<Arc<input::PortalInput>> {
        self.inner
            .lease
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .owner
            .control
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub async fn inject_input(
        &self,
        event: protocol::InputEvent,
    ) -> std::result::Result<(), PortalInputError> {
        self.input_status()?;
        self.input()
            .ok_or(PortalInputError::ViewOnly)?
            .inject(event)
            .await
    }
    pub async fn release_input(&self) -> std::result::Result<(), PortalInputError> {
        match self.input() {
            Some(input) => input.release().await,
            None => Ok(()),
        }
    }
    pub fn generation(&self) -> u64 {
        self.inner
            .lease
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .selected
            .generation
    }
    pub fn revocation(&self) -> watch::Receiver<bool> {
        self.inner
            .lease
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .revocation()
    }
    pub fn publish_owned(&self, frame: OwnedFrame) -> Result<bool> {
        if crate::linux_raw_encode::validate_native_input(&frame).is_err() {
            self.revoke();
            return Err(PortalCallError::InvalidSource);
        }
        let lease = self
            .inner
            .lease
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let state = lease
            .owner
            .control
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if lease.is_revoked()
            || !matches!(
                state.phase(),
                super::Phase::Streaming | super::Phase::Paused
            )
            || frame.stamp.generation != lease.selected.generation
        {
            return Err(PortalCallError::Revoked);
        }
        let (width, height) = (frame.width, frame.height);
        if let Some(input) = lease
            .owner
            .control
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            input.observe_frame(&frame);
        }
        let published = self.inner.mailbox.publish(frame);
        if published {
            let mut info = self
                .inner
                .info
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            info.width = width;
            info.height = height;
        }
        Ok(published)
    }
    pub fn revoke(&self) {
        self.inner
            .lease
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .owner
            .signal_close();
    }
    pub(crate) fn mailbox(&self) -> Arc<FrameMailbox> {
        self.inner.mailbox.clone()
    }
}

async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    loop {
        if *receiver.borrow() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T>>,
    timeout: Duration,
    cancel: &mut watch::Receiver<bool>,
    revoked: &mut watch::Receiver<bool>,
) -> Result<T> {
    tokio::select! {
        biased;
        _ = cancelled(cancel) => Err(PortalCallError::Cancelled),
        _ = cancelled(revoked) => Err(PortalCallError::Revoked),
        result = tokio::time::timeout(timeout, future) => result.map_err(|_| PortalCallError::Timeout)?,
    }
}

fn token() -> String {
    let random = remote_core::session_crypto::random_bytes_16();
    let mut result = String::from("rp_");
    for byte in random {
        use std::fmt::Write;
        let _ = write!(result, "{byte:02x}");
    }
    result
}

async fn proxy(
    connection: &Connection,
    destination: &str,
    path: &str,
    interface: &'static str,
) -> Result<Proxy<'static>> {
    Proxy::new_owned(
        connection.clone(),
        destination.to_owned(),
        path.to_owned(),
        interface,
    )
    .await
    .map_err(|_| PortalCallError::Transport)
}

async fn close_object(
    connection: &Connection,
    destination: &str,
    path: &str,
    interface: &'static str,
    timeout: Duration,
) -> bool {
    tokio::time::timeout(timeout, async {
        let proxy = proxy(connection, destination, path, interface).await?;
        proxy
            .call::<_, _, ()>("Close", &())
            .await
            .map_err(|_| PortalCallError::Transport)
    })
    .await
    .is_ok_and(|result| result.is_ok())
}

fn start_owner(
    connection: Connection,
    destination: String,
    mut session_closed: SignalStream<'static>,
    mut owner_changed: SignalStream<'static>,
    control: Arc<Control>,
    mut cancel: watch::Receiver<bool>,
    timeouts: Timeouts,
) -> LeaseOwner {
    let (close_tx, close_rx) = oneshot::channel::<ClosePlan>();
    let (done_tx, done) = oneshot::channel();
    let watched = control.clone();
    tokio::spawn(async move {
        let plan = tokio::select! {
            biased;
            plan = close_rx => plan.unwrap_or_else(|_| watched.revoke()),
            _ = cancelled(&mut cancel) => watched.revoke(),
            _ = session_closed.next() => watched.revoke(),
            _ = owner_changed.next() => watched.revoke(),
        };
        watched.revoked.send_replace(true);
        let input = watched
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let input_ok = match input {
            Some(input) => tokio::time::timeout(Duration::from_millis(750), input.release())
                .await
                .is_ok_and(|result| result.is_ok()),
            None => true,
        };
        let request_ok = match plan.request {
            Some(path) => {
                close_object(&connection, &destination, &path, REQUEST, timeouts.cleanup).await
            }
            None => true,
        };
        let session_ok = match plan.session {
            Some(path) => {
                close_object(&connection, &destination, &path, SESSION, timeouts.cleanup).await
            }
            None => true,
        };
        let _ = done_tx.send(input_ok && request_ok && session_ok);
        // Proxies/connection are dropped here even if Close is unavailable.
        // A restarted portal cannot inherit the old unique destination or FD.
    });
    LeaseOwner {
        control,
        close_tx: Some(close_tx),
        done: Some(done),
    }
}

async fn request<B: Serialize + Type>(
    connection: &Connection,
    destination: &str,
    session_sender: &str,
    method: Method,
    interface: &'static str,
    request_token: &str,
    body: &B,
    owner: &LeaseOwner,
    cancel: &mut watch::Receiver<bool>,
    timeout: Duration,
) -> Result<Values> {
    let path = format!("{DESKTOP_PATH}/request/{session_sender}/{request_token}");
    let mut revoked = owner.control.revoked.subscribe();
    let results = bounded(
        async {
            let request_proxy = proxy(connection, destination, &path, REQUEST).await?;
            // Registration itself is cancellable and bounded, and completes
            // before the method can emit an early Response.
            let mut responses = request_proxy
                .receive_signal("Response")
                .await
                .map_err(|_| PortalCallError::Transport)?;
            owner
                .control
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .begin_request(method, path.clone())
                .map_err(|_| PortalCallError::Revoked)?;
            let portal = proxy(connection, destination, DESKTOP_PATH, interface).await?;
            let name = match method {
                Method::Create => "CreateSession",
                Method::SelectDevices => "SelectDevices",
                Method::Select => "SelectSources",
                Method::Start => "Start",
            };
            let returned: OwnedObjectPath = portal
                .call(name, body)
                .await
                .map_err(|_| PortalCallError::Transport)?;
            if returned.as_str() != path {
                return Err(PortalCallError::Response);
            }
            let signal = responses.next().await.ok_or(PortalCallError::Revoked)?;
            let (code, values): (u32, Values) = signal
                .body()
                .deserialize()
                .map_err(|_| PortalCallError::Response)?;
            match code {
                0 => Ok(values),
                1 => Err(PortalCallError::Cancelled),
                _ => Err(PortalCallError::Response),
            }
        },
        timeout,
        cancel,
        &mut revoked,
    )
    .await?;
    Ok(results)
}

fn complete(
    owner: &LeaseOwner,
    method: Method,
    path: &str,
    session: Option<&str>,
    kind: Option<SourceKind>,
) -> Result<()> {
    let mut state = owner
        .control
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _ = method; // The reserved pending state checks the actual method/path.
    state
        .complete_request(path, 0, session, kind)
        .map_err(|_| PortalCallError::Response)
}

fn selected(
    mut results: Values,
    generation: u64,
    options: SelectionOptions,
) -> Result<SelectedPortalSource> {
    let streams: Vec<(u32, Values)> = results
        .remove("streams")
        .ok_or(PortalCallError::InvalidSource)?
        .try_into()
        .map_err(|_| PortalCallError::InvalidSource)?;
    if streams.len() != 1 {
        return Err(PortalCallError::InvalidSource);
    }
    let (node, mut properties) = streams.into_iter().next().unwrap();
    if node == 0 || node == u32::MAX {
        return Err(PortalCallError::InvalidSource);
    }
    let source_type: u32 = properties
        .remove("source_type")
        .ok_or(PortalCallError::InvalidSource)?
        .try_into()
        .map_err(|_| PortalCallError::InvalidSource)?;
    let kind = match source_type {
        1 if options.monitor => SourceKind::Monitor,
        2 if options.window => SourceKind::Window,
        _ => return Err(PortalCallError::InvalidSource),
    };
    let geometry = |value: Option<OwnedValue>| -> Result<Option<(i32, i32)>> {
        value
            .map(|value| {
                <(i32, i32)>::try_from(Value::from(value))
                    .map_err(|_| PortalCallError::InvalidSource)
            })
            .transpose()
    };
    let logical_size = geometry(properties.remove("size"))?;
    if logical_size.is_some_and(|(width, height)| width <= 0 || height <= 0) {
        return Err(PortalCallError::InvalidSource);
    }
    Ok(SelectedPortalSource {
        generation,
        kind,
        node_id: node,
        pipewire_serial: properties
            .remove("pipewire-serial")
            .map(|value| {
                let serial = u64::try_from(value).map_err(|_| PortalCallError::InvalidSource)?;
                if serial == 0 {
                    return Err(PortalCallError::InvalidSource);
                }
                Ok(serial)
            })
            .transpose()?,
        logical_size,
        logical_position: geometry(properties.remove("position"))?,
        embedded_cursor: options.embedded_cursor,
    })
}

/// Explicit local action only. ListSources and network commands must never call
/// this function. Keep the cancel sender alive for the desired lease lifetime.
pub async fn request_local_portal_capture(
    parent_identifier: &str,
    sources: PickerSources,
    cancel: watch::Receiver<bool>,
) -> Result<PortalCaptureLease> {
    request_local_capture(parent_identifier, sources, cancel, false).await
}

/// Explicit local control request, with normal RemoteDesktop consent. Generic
/// window captures have no verified focus binding and cannot grant desktop input.
pub async fn request_local_portal_control_capture(
    parent_identifier: &str,
    sources: PickerSources,
    cancel: watch::Receiver<bool>,
) -> Result<PortalCaptureLease> {
    if sources != PickerSources::Monitor {
        return Err(PortalCallError::Capabilities);
    }
    request_local_capture(parent_identifier, sources, cancel, true).await
}

async fn request_local_capture(
    parent_identifier: &str,
    sources: PickerSources,
    mut cancel: watch::Receiver<bool>,
    control_input: bool,
) -> Result<PortalCaptureLease> {
    if *cancel.borrow() {
        return Err(PortalCallError::Cancelled);
    }
    let timeouts = Timeouts::default();
    let connection = tokio::select! {
        biased;
        _ = cancelled(&mut cancel) => return Err(PortalCallError::Cancelled),
        result = tokio::time::timeout(timeouts.operation, zbus::connection::Builder::session()
            .map_err(|_| PortalCallError::Transport)?.method_timeout(timeouts.operation).build()) =>
            result.map_err(|_| PortalCallError::Timeout)?.map_err(|_| PortalCallError::Transport)?,
    };
    select_with_control(
        connection,
        parent_identifier,
        sources,
        cancel,
        timeouts,
        control_input,
    )
    .await
}

#[cfg(test)]
async fn select_on_connection(
    connection: Connection,
    parent_identifier: &str,
    sources: PickerSources,
    cancel: watch::Receiver<bool>,
    timeouts: Timeouts,
) -> Result<PortalCaptureLease> {
    select_with_control(
        connection,
        parent_identifier,
        sources,
        cancel,
        timeouts,
        false,
    )
    .await
}

async fn select_with_control(
    connection: Connection,
    parent_identifier: &str,
    sources: PickerSources,
    mut cancel: watch::Receiver<bool>,
    timeouts: Timeouts,
    control_input: bool,
) -> Result<PortalCaptureLease> {
    if control_input && sources != PickerSources::Monitor {
        return Err(PortalCallError::Capabilities);
    }
    if parent_identifier.len() > 4096
        || !parent_identifier.is_empty()
            && !parent_identifier.starts_with("x11:")
            && !parent_identifier.starts_with("wayland:")
    {
        return Err(PortalCallError::Parent);
    }
    // Keep the initial channel alive until the real session owner is installed.
    let (initial_tx, mut initial_revoked) = watch::channel(false);
    let (destination, screencast, options, screencast_version) = bounded(
        async {
            let initial = proxy(&connection, DESKTOP, DESKTOP_PATH, SCREENCAST).await?;
            let _: u32 = initial
                .get_property("version")
                .await
                .map_err(|_| PortalCallError::Transport)?;
            let bus = proxy(&connection, DBUS, DBUS_PATH, DBUS).await?;
            let destination: String = bus
                .call("GetNameOwner", &(DESKTOP,))
                .await
                .map_err(|_| PortalCallError::Transport)?;
            let screencast = proxy(&connection, &destination, DESKTOP_PATH, SCREENCAST).await?;
            let version: u32 = screencast
                .get_property("version")
                .await
                .map_err(|_| PortalCallError::Transport)?;
            if version < 4 {
                return Err(PortalCallError::Capabilities);
            }
            let available: u32 = screencast
                .get_property("AvailableSourceTypes")
                .await
                .map_err(|_| PortalCallError::Transport)?;
            let cursors: u32 = screencast
                .get_property("AvailableCursorModes")
                .await
                .map_err(|_| PortalCallError::Transport)?;
            let options = SelectionOptions::from_capabilities(available & sources.mask(), cursors)
                .map_err(|_| PortalCallError::Capabilities)?;
            if control_input {
                let remote = proxy(&connection, &destination, DESKTOP_PATH, REMOTE_DESKTOP).await?;
                let version: u32 = remote
                    .get_property("version")
                    .await
                    .map_err(|_| PortalCallError::Capabilities)?;
                let devices: u32 = remote
                    .get_property("AvailableDeviceTypes")
                    .await
                    .map_err(|_| PortalCallError::Capabilities)?;
                if version == 0 || devices & 3 != 3 {
                    return Err(PortalCallError::Capabilities);
                }
            }
            Ok((destination, screencast, options, version))
        },
        timeouts.operation,
        &mut cancel,
        &mut initial_revoked,
    )
    .await?;
    drop(initial_tx);
    let sender = connection
        .unique_name()
        .ok_or(PortalCallError::Transport)?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let generation = GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            old.checked_add(1)
        })
        .map_err(|_| PortalCallError::Generation)?;
    let session_token = token();
    let session_path = format!("{DESKTOP_PATH}/session/{sender}/{session_token}");
    let state = PortalSessionState::new(generation, session_path.clone(), options)
        .map_err(|_| PortalCallError::Response)?;
    let session_proxy = proxy(&connection, &destination, &session_path, SESSION).await?;
    let session_closed = session_proxy
        .receive_signal("Closed")
        .await
        .map_err(|_| PortalCallError::Transport)?;
    let bus = proxy(&connection, DBUS, DBUS_PATH, DBUS).await?;
    let owner_changed = bus
        .receive_signal_with_args("NameOwnerChanged", &[(0, DESKTOP)])
        .await
        .map_err(|_| PortalCallError::Transport)?;
    let confirmed_owner: String = bus
        .call("GetNameOwner", &(DESKTOP,))
        .await
        .map_err(|_| PortalCallError::Transport)?;
    if confirmed_owner != destination {
        return Err(PortalCallError::Revoked);
    }
    let (revoked, _) = watch::channel(false);
    let control = Arc::new(Control {
        state: Mutex::new(state),
        revoked,
        frames: Mutex::new(None),
        input: Mutex::new(None),
    });
    let owner = start_owner(
        connection.clone(),
        destination.clone(),
        session_closed,
        owner_changed,
        control,
        cancel.clone(),
        timeouts,
    );
    let create_token = token();
    let create_options: HashMap<&str, Value<'_>> = HashMap::from([
        ("handle_token", Value::from(create_token.as_str())),
        ("session_handle_token", Value::from(session_token.as_str())),
    ]);
    let mut response = request(
        &connection,
        &destination,
        &sender,
        Method::Create,
        if control_input {
            REMOTE_DESKTOP
        } else {
            SCREENCAST
        },
        &create_token,
        &create_options,
        &owner,
        &mut cancel,
        timeouts.operation,
    )
    .await?;
    // ScreenCast's session_handle response is a STRING, not an object-path
    // variant (historical portal API). Validate against the reserved token.
    let returned_session: String = response
        .remove("session_handle")
        .ok_or(PortalCallError::Response)?
        .try_into()
        .map_err(|_| PortalCallError::Response)?;
    if returned_session != session_path {
        return Err(PortalCallError::Response);
    }
    complete(
        &owner,
        Method::Create,
        &format!("{DESKTOP_PATH}/request/{sender}/{create_token}"),
        Some(&session_path),
        None,
    )?;
    let session =
        OwnedObjectPath::try_from(session_path.as_str()).map_err(|_| PortalCallError::Response)?;
    if control_input {
        let device_token = token();
        let device_options: HashMap<&str, Value<'_>> = HashMap::from([
            ("handle_token", Value::from(device_token.as_str())),
            ("types", Value::from(3u32)),
        ]);
        request(
            &connection,
            &destination,
            &sender,
            Method::SelectDevices,
            REMOTE_DESKTOP,
            &device_token,
            &(&session, device_options),
            &owner,
            &mut cancel,
            timeouts.operation,
        )
        .await?;
        complete(
            &owner,
            Method::SelectDevices,
            &format!("{DESKTOP_PATH}/request/{sender}/{device_token}"),
            None,
            None,
        )?;
    }
    let select_token = token();
    let mut select_options: HashMap<&str, Value<'_>> = HashMap::from([
        ("handle_token", Value::from(select_token.as_str())),
        (
            "types",
            Value::from((u32::from(options.monitor)) | (u32::from(options.window) << 1)),
        ),
        ("multiple", Value::from(false)),
        ("persist_mode", Value::from(0u32)),
        (
            "cursor_mode",
            Value::from(if options.embedded_cursor { 2u32 } else { 1u32 }),
        ),
    ]);
    // Combined sessions must use RemoteDesktop persistence options exclusively;
    // omitting them there means nonpersistent consent. Never request restoration.
    if control_input {
        select_options.remove("persist_mode");
    }
    request(
        &connection,
        &destination,
        &sender,
        Method::Select,
        SCREENCAST,
        &select_token,
        &(&session, select_options),
        &owner,
        &mut cancel,
        timeouts.operation,
    )
    .await?;
    complete(
        &owner,
        Method::Select,
        &format!("{DESKTOP_PATH}/request/{sender}/{select_token}"),
        None,
        None,
    )?;
    let start_token = token();
    let start_options: HashMap<&str, Value<'_>> =
        HashMap::from([("handle_token", Value::from(start_token.as_str()))]);
    let response = request(
        &connection,
        &destination,
        &sender,
        Method::Start,
        if control_input {
            REMOTE_DESKTOP
        } else {
            SCREENCAST
        },
        &start_token,
        &(&session, parent_identifier, start_options),
        &owner,
        &mut cancel,
        timeouts.selection,
    )
    .await?;
    let devices = if control_input {
        let devices = u32::try_from(response.get("devices").ok_or(PortalCallError::Response)?)
            .map_err(|_| PortalCallError::Response)?;
        if devices & !3 != 0 {
            return Err(PortalCallError::Response);
        }
        devices
    } else {
        0
    };
    let selected = selected(response, generation, options)?;
    if control_input && screencast_version >= 6 && selected.pipewire_serial.is_none() {
        return Err(PortalCallError::InvalidSource);
    }
    complete(
        &owner,
        Method::Start,
        &format!("{DESKTOP_PATH}/request/{sender}/{start_token}"),
        None,
        Some(selected.kind),
    )?;
    if control_input {
        *owner
            .control
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(input::PortalInput::new(
            connection.clone(),
            destination.clone(),
            session.clone(),
            selected,
            devices,
            owner.control.revoked.subscribe(),
            cancel.clone(),
        )));
    }
    let mut revoked = owner.control.revoked.subscribe();
    let fd: zbus::zvariant::OwnedFd = bounded(
        async {
            screencast
                .call(
                    "OpenPipeWireRemote",
                    &(&session, HashMap::<&str, Value<'_>>::new()),
                )
                .await
                .map_err(|_| PortalCallError::Transport)
        },
        timeouts.operation,
        &mut cancel,
        &mut revoked,
    )
    .await?;
    let remote = {
        let state = owner
            .control
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        RestrictedRemote::new(&state, selected.node_id, fd.into())
            .map_err(|_| PortalCallError::Revoked)?
    };
    Ok(PortalCaptureLease {
        owner,
        remote: Some(remote),
        selected,
        cancellation: cancel,
    })
}

#[cfg(test)]
pub(crate) mod tests;
