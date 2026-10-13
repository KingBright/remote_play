#[cfg(all(unix, any(test, target_os = "linux")))]
use crate::linux_portal::{PeerOwner, runtime::PreparedPortalCapture};
use crate::{
    StreamingRunConfig, SubscriptionAudioConfig, env_flag_enabled, run_streaming,
    start_subscription_audio,
};
use protocol::realtime_input::{InputBinding, InputLease, InputPayload, RealtimeInput};
use protocol::session::{
    CaptureSource, MAX_SUBSCRIPTIONS, SESSION_VERSION, SessionCommand, SubscriptionRequest,
};
use protocol::{AudioControlTarget, ContentKind, ControlMessage, DataEnvelope, InputEvent};
use remote_core::net::{MultiplexedPacket, UdpMultiplexer, UdpSender};
use remote_core::scheduled_sender::{ScheduledDataSender, ScheduledDataSenderConfig};
use remote_core::session_crypto::{
    SessionCrypto, load_session_psk, mac_session_accept, mac_session_hello, now_unix_ms,
    random_bytes_16, require_session_auth, verify_session_mac,
};
use remote_core::{InputInjector, stats::Statistics};
use std::collections::HashMap;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;

/// Local process capability; a network address alone cannot authorize a grant.
#[cfg(all(unix, any(test, target_os = "linux")))]
#[derive(Clone)]
pub struct LocalPortalShareTarget {
    addr: SocketAddr,
    owner: PeerOwner,
    source_revision: u32,
    subscription_id: u32,
}
#[cfg(all(unix, any(test, target_os = "linux")))]
impl LocalPortalShareTarget {
    pub fn local_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.addr,
            self.owner.connection_id(),
            self.subscription_id
        )
    }
    pub fn label(&self) -> String {
        format!("{} · stream {}", self.addr, self.subscription_id)
    }
    pub fn subscription_id(&self) -> u32 {
        self.subscription_id
    }
    /// Local restoration cannot follow an address/name to another authenticated
    /// connection. Source revision is rechecked separately at commit.
    pub fn same_authenticated_target(&self, other: &Self) -> bool {
        self.addr == other.addr && self.owner == other.owner
            && self.subscription_id == other.subscription_id
    }
    #[cfg(test)]
    pub(crate) fn fixture_target(nonce: [u8;16]) -> Self {
        Self { addr: "127.0.0.1:39577".parse().unwrap(),
            owner: PeerOwner::from_authenticated_connection(31, nonce, Some([2;32])).unwrap(),
            source_revision: 10, subscription_id: 100 }
    }
}
#[cfg(all(unix, any(test, target_os = "linux")))]
pub enum LocalPortalShareCommand {
    Targets {
        reply: tokio::sync::oneshot::Sender<Vec<LocalPortalShareTarget>>,
    },
    Commit {
        target: LocalPortalShareTarget,
        capture: PreparedPortalCapture,
        reply: tokio::sync::oneshot::Sender<Result<protocol::session::CaptureSourceInfo, String>>,
    },
}

#[derive(Clone)]
pub struct HostServiceConfig {
    pub bind_addr: SocketAddr,
    pub stats: Arc<Statistics>,
    pub enable_clipboard_sync: bool,
    pub enable_file_transfer: bool,
    pub enable_talkback: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum AudioGroupKey {
    System,
    Application(i32),
}

#[cfg(target_os = "windows")]
static WINDOWS_VIDEO_CAPTURE_RESERVED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
struct WindowsVideoCaptureReservation;

#[cfg(target_os = "windows")]
impl WindowsVideoCaptureReservation {
    fn acquire() -> Result<Self, String> {
        WINDOWS_VIDEO_CAPTURE_RESERVED
            .compare_exchange(false, true, Relaxed, Relaxed)
            .map(|_| Self)
            .map_err(|_| {
                "Windows capture is already in use. This safe build allows one video capture session at a time."
                    .to_string()
            })
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsVideoCaptureReservation {
    fn drop(&mut self) {
        WINDOWS_VIDEO_CAPTURE_RESERVED.store(false, Relaxed);
    }
}

fn validate_host_video_settings(
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
) -> Result<(), &'static str> {
    protocol::validate_video_settings(width, height, fps, bitrate_kbps)?;
    #[cfg(target_os = "windows")]
    {
        const MAX_PIXELS: u64 = 2560 * 1440;
        if u64::from(width) * u64::from(height) > MAX_PIXELS || fps > 60 || bitrate_kbps > 20_000 {
            return Err("Windows safe capture budget is 2560x1440 pixels, 60 fps, and 20000 kbps");
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputTarget {
    Desktop,
    #[cfg(target_os = "macos")]
    Display {
        id: u32,
    },
    Window {
        id: u32,
        pid: i32,
    },
    #[cfg(all(unix, any(test, target_os = "linux")))]
    Portal,
}
fn input_target_for(
    source: CaptureSource,
    pid: Option<i32>,
    supported: bool,
) -> Option<InputTarget> {
    if !supported {
        return None;
    }
    match source {
        CaptureSource::MainDisplay => Some(InputTarget::Desktop),
        CaptureSource::Display(id) => {
            #[cfg(target_os = "macos")]
            {
                Some(InputTarget::Display { id })
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = id;
                Some(InputTarget::Desktop)
            }
        }
        CaptureSource::Window(id) => pid
            .filter(|pid| *pid > 0)
            .map(|pid| InputTarget::Window { id, pid }),
    }
}

// Read-only readiness, separate from catalog geometry capability. This never
// requests consent or posts an event. Constructors/events recheck permission.
#[cfg(target_os = "macos")]
fn mac_input_status(target: InputTarget) -> Result<(), String> {
    crate::input_injector::post_event_access_status().map_err(|e| e.to_string())?;
    match target {
        InputTarget::Desktop => crate::input_injector::display_input_status(
            core_graphics::display::CGDisplay::main().id,
        ),
        InputTarget::Display { id } => crate::input_injector::display_input_status(id),
        InputTarget::Window { .. } => {
            if crate::window_input::permission_available() { Ok(()) }
            else { Err("The running RemotePlay host needs Accessibility permission for selected-window control; verify its signed App identity, then reselect the source".into()) }
        }
        #[cfg(test)]
        InputTarget::Portal => return Err("Portal input requires its exact consented session".into()),
    }.map_err(|e| e.to_string())
}

struct Subscription {
    settings_revision: u64,
    request: SubscriptionRequest,
    settings: watch::Sender<crate::StreamSettings>,
    revision: u64,
    keyframe: Arc<AtomicBool>,
    cancel: broadcast::Sender<()>,
    task: tokio::task::JoinHandle<()>,
    task_joined: bool,
    audio_group: Option<AudioGroupKey>,
    audio_active: bool,
    supports_input: bool,
    input_target: Option<InputTarget>,
    #[cfg(target_os = "macos")]
    input_probe: fn(InputTarget) -> Result<(), String>,
    last_input_error: Option<Instant>,
    source_revision: u32,
    video_sequence: Arc<std::sync::atomic::AtomicU16>,
    #[cfg(target_os = "windows")]
    _capture_reservation: Arc<WindowsVideoCaptureReservation>,
    #[cfg(all(unix, any(test, target_os = "linux")))]
    native_capture: Option<PreparedPortalCapture>,
}
impl Subscription {
    fn supports_input_now(&self) -> bool {
        self.input_status().is_ok()
    }
    fn input_status(&self) -> Result<(), String> {
        if !self.supports_input || self.input_target.is_none() {
            return Err("Input is not supported for the selected capture source".into());
        }
        #[cfg(all(unix, any(test, target_os = "linux")))]
        if let Some(capture) = &self.native_capture {
            return capture.input_status().map_err(|e| e.to_string());
        }
        #[cfg(target_os = "macos")]
        (self.input_probe)(self.input_target.unwrap())?;
        Ok(())
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        #[cfg(all(unix, any(test, target_os = "linux")))]
        if let Some(capture) = &self.native_capture {
            capture.revoke();
        }
        let _ = self.cancel.send(());
        self.task.abort();
    }
}
struct AudioGroup {
    owner: u32,
    settings: watch::Sender<crate::StreamSettings>,
    cancel: broadcast::Sender<()>,
    _tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for AudioGroup {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        for task in &self._tasks {
            task.abort();
        }
    }
}
#[cfg(test)]
thread_local! {static TEST_CAPTURE_CATALOG: std::cell::RefCell<Option<Vec<protocol::session::CaptureSourceInfo>>> = const {std::cell::RefCell::new(None)};}

struct InputLeaseSlot {
    lease:InputLease,
    target:InputTarget,
    portal_generation:Option<u64>,
}

struct Connection {
    versioned_control: bool,
    id: u32,
    last_seen: Instant,
    cancel: broadcast::Sender<()>,
    sender: ScheduledDataSender,
    clipboard: Option<mpsc::Sender<DataEnvelope>>,
    clipboard_cancel: Option<broadcast::Sender<()>>,
    clipboard_enabled: Arc<std::sync::atomic::AtomicBool>,
    files: Option<mpsc::Sender<DataEnvelope>>,
    talkback: Option<mpsc::Sender<DataEnvelope>>,
    #[cfg(target_os = "macos")]
    talkback_settings: Option<watch::Sender<crate::talkback_player::TalkbackPlaybackSettings>>,
    subscriptions: HashMap<u32, Subscription>,
    input_leases: HashMap<u32, InputLeaseSlot>,
    // First terminal result is immutable, including unknown/failed cleanup.
    retired: HashMap<u32, Result<(), String>>,
    worker_failure: bool,
    audio_groups: HashMap<AudioGroupKey, AudioGroup>,
    _reporter: crate::ScheduledStatsReporterGuard,
    resources: Arc<crate::lifecycle::Resources>,
    terminal_reported: bool,
    services_task: tokio::task::JoinHandle<Result<(), String>>,
    sender_task: remote_core::scheduled_sender::ScheduledDataSenderJoin,
    clipboard_task: Option<tokio::task::JoinHandle<()>>,
    retiring_tasks: Vec<tokio::task::JoinHandle<()>>,
    #[cfg(all(unix, any(test, target_os = "linux")))]
    native_capture: HashMap<u32, PreparedPortalCapture>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        if !self.terminal_reported {
            crate::lifecycle::publish(self.id, &self.resources, "drop_requested");
        }
        self.services_task.abort();
        self.sender_task.abort();
        for task in &self.retiring_tasks {
            task.abort();
        }
        if let Some(task) = &self.clipboard_task {
            task.abort();
        }
        #[cfg(all(unix, any(test, target_os = "linux")))]
        for (_, capture) in self.native_capture.drain() {
            capture.revoke();
        }
        if let Some(cancel) = self.clipboard_cancel.take() {
            let _ = cancel.send(());
        }
        let _ = self.cancel.send(());
    }
}

struct ClosingTask {
    resources: Arc<crate::lifecycle::Resources>,
    id: u32,
    notify: bool,
    task: tokio::task::JoinHandle<Result<(), String>>,
}
impl Drop for ClosingTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct ClosedReceipt {
    resources: Arc<crate::lifecycle::Resources>,
    id: u32,
    result: Result<(), String>,
    at: Instant,
}
impl ClosedReceipt {
    fn resources_live(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.resources.workers.load(SeqCst) != 0 || self.resources.native_streams.load(SeqCst) != 0
    }
}
fn pending_closed_count(closed: &HashMap<SocketAddr, ClosedReceipt>) -> usize {
    closed
        .values()
        .filter(|receipt| receipt.resources_live())
        .count()
}

// Keep Hello admission in the same production path used by the state regression.
fn accept_session_hello(
    psk: Option<&[u8]>,
    hello: ([u8; 16], u64, [u8; 32]),
    addr: SocketAddr,
    udp_sender: &UdpSender,
    peers: &HashMap<SocketAddr, Connection>,
    closing: &HashMap<SocketAddr, ClosingTask>,
    closed: &mut HashMap<SocketAddr, ClosedReceipt>,
    authenticated: &mut HashMap<SocketAddr, ([u8; 16], [u8; 16], u64)>,
) -> Result<([u8; 16], u64), String> {
    let (nonce, timestamp_ms, mac) = hello;
    let key = psk.ok_or("host has no session PSK configured")?;
    verify_session_mac(
        &mac_session_hello(key, &nonce, timestamp_ms),
        &mac,
        timestamp_ms,
        now_unix_ms(),
    )
    .map_err(|e| e.to_string())?;
    if let Some((previous, salt, ts)) = authenticated.get(&addr) {
        if *previous == nonce {
            return Ok((*salt, *ts));
        }
    }
    // Authentication can be cleared on key rotation while native owners survive.
    // The retained resource slot must be checked before any identity/receipt replacement.
    if peers.contains_key(&addr)
        || closing.contains_key(&addr)
        || closed.get(&addr).is_some_and(ClosedReceipt::resources_live)
    {
        return Err("close the existing connection before rekeying".into());
    }
    if authenticated.len() >= 16 && !authenticated.contains_key(&addr) {
        return Err("authentication capacity reached".into());
    }
    let salt = random_bytes_16();
    let ts = now_unix_ms();
    udp_sender.install_peer_crypto(
        addr,
        SessionCrypto::from_psk(key, &salt).map_err(|e| e.to_string())?,
    );
    authenticated.insert(addr, (nonce, salt, ts));
    closed.remove(&addr);
    Ok((salt, ts))
}

// Both Open and legacy StartStream must account for retained native ownership.
fn connection_admission_error(
    addr: SocketAddr,
    peers: &HashMap<SocketAddr, Connection>,
    closing: &HashMap<SocketAddr, ClosingTask>,
    closed: &HashMap<SocketAddr, ClosedReceipt>,
) -> Option<&'static str> {
    if closed.get(&addr).is_some_and(ClosedReceipt::resources_live) {
        Some("previous native teardown remains unknown")
    } else if peers.len() + closing.len() + pending_closed_count(closed) >= 8
        && !peers.contains_key(&addr)
    {
        Some("connection limit reached")
    } else {
        None
    }
}

fn joined(result: Result<(), tokio::task::JoinError>) -> Result<(), String> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(_) => Err("connection worker panicked during teardown".into()),
    }
}
fn begin_close(
    peers: &mut HashMap<SocketAddr, Connection>,
    closing: &mut HashMap<SocketAddr, ClosingTask>,
    addr: SocketAddr,
    notify: bool,
) {
    if let Some(peer) = peers.remove(&addr) {
        let id = peer.id;
        crate::lifecycle::publish(id, &peer.resources, "closing");
        closing.insert(
            addr,
            ClosingTask {
                resources: peer.resources.clone(),
                id,
                notify,
                task: tokio::spawn(peer.shutdown()),
            },
        );
    }
}
fn request_close(
    peers: &mut HashMap<SocketAddr, Connection>,
    closing: &mut HashMap<SocketAddr, ClosingTask>,
    closed: &HashMap<SocketAddr, ClosedReceipt>,
    addr: SocketAddr,
    id: u32,
) -> Option<SessionCommand> {
    if let Some(ending) = closing.get_mut(&addr) {
        if ending.id == id {
            ending.notify = true;
            return None;
        }
    } else if let Some(peer) = peers.get(&addr) {
        if peer.id == id {
            begin_close(peers, closing, addr, true);
            return None;
        }
    } else if let Some(receipt) = closed.get(&addr).filter(|r| r.id == id) {
        return Some(terminal_reply(id, &receipt.result));
    }
    Some(SessionCommand::Error {
        request_id: id,
        reason: "connection ID is unavailable; no other connection was closed".into(),
    })
}
fn terminal_reply(id: u32, result: &Result<(), String>) -> SessionCommand {
    match result {
        Ok(()) => SessionCommand::Closed {
            connection_id: id,
            reason: "all capture and connection workers stopped".into(),
        },
        Err(reason) => SessionCommand::Error {
            request_id: id,
            reason: reason.clone(),
        },
    }
}

fn stream_settings(request: &SubscriptionRequest) -> crate::StreamSettings {
    crate::StreamSettings {
        width: request.width,
        height: request.height,
        fps: request.fps,
        bitrate_kbps: request.bitrate_kbps,
        paused: false,
    }
}

impl Connection {
    fn accepts_legacy_control(&self) -> bool { !self.versioned_control }
    fn accepts_initial_subscribe(&self, request:&SubscriptionRequest) -> bool {
        self.subscriptions.get(&request.id).is_none_or(|sub|
            !sub.task.is_finished() && sub.request.source==request.source && sub.request.audio==request.audio)
    }
    fn retires_input(&self, command:&SessionCommand) -> Option<u32> {
        match command {
            SessionCommand::Subscribe(request) if !self.subscriptions.contains_key(&request.id)=>Some(request.id),
            SessionCommand::SwitchSource{id,request_id,..} if self.subscriptions.get(id).is_some_and(|sub|*request_id>sub.source_revision)=>Some(*id),
            SessionCommand::SetActivity{id,revision,video,..} if self.subscriptions.get(id).is_some_and(|sub|*revision>sub.revision && sub.settings.borrow().paused==*video)=>Some(*id),
            _=>None,
        }
    }
    fn input_signature(&self,id:u32) -> Option<(u32,InputTarget,Option<u64>)> {
        if !self.can_input(id) {return None;}
        let sub=self.subscriptions.get(&id)?;
        #[cfg(all(unix, any(test,target_os="linux")))]
        let generation=sub.native_capture.as_ref().map(PreparedPortalCapture::generation);
        #[cfg(not(all(unix, any(test,target_os="linux"))))]
        let generation=None;
        Some((sub.source_revision,sub.input_target?,generation))
    }
    fn input_binding(&mut self,connection:u32,id:u32,revision:u32,request:u64) -> Option<SessionCommand> {
        if connection!=self.id || request==0 {return None;}
        let (current,target,generation)=self.input_signature(id)?;
        if current!=revision {return None;}
        let now=now_unix_ms();
        let rotate=self.input_leases.get(&id).is_none_or(|slot|
            slot.lease.expired(now) || slot.lease.binding.source_revision!=revision
            || slot.target!=target || slot.portal_generation!=generation);
        if rotate {
            let binding=InputBinding{connection_id:connection,subscription_id:id,source_revision:revision,epoch:random_bytes_16()};
            self.input_leases.insert(id,InputLeaseSlot{lease:InputLease::new(binding,now),target,portal_generation:generation});
        }
        self.versioned_control=true;
        Some(SessionCommand::InputBinding{binding:self.input_leases.get(&id)?.lease.binding,request,host_time_ms:now})
    }
    fn accept_input(&mut self,packet:&RealtimeInput) -> Option<Vec<InputEvent>> {
        let id=packet.binding.subscription_id;
        let (revision,target,generation)=self.input_signature(id)?;
        let slot=self.input_leases.get_mut(&id)?;
        if packet.binding.connection_id!=self.id || packet.binding.source_revision!=revision
            || slot.target!=target || slot.portal_generation!=generation {return None;}
        slot.lease.accept(packet,now_unix_ms())
    }
    async fn unsubscribe(&mut self, id: u32) -> Result<(), String> {
        if let Some(receipt) = self.retired.get(&id) {
            return receipt.clone();
        }
        if !self.subscriptions.contains_key(&id) {
            return Err("subscription is unavailable".into());
        }
        self.retired
            .insert(id, Err("unsubscribe cleanup outcome is unknown".into()));
        let result = self.unsubscribe_owned(id).await;
        self.retired.insert(id, result.clone());
        result
    }
    async fn unsubscribe_owned(&mut self, id: u32) -> Result<(), String> {
        let subscription = self.subscriptions.get_mut(&id).unwrap();
        subscription.supports_input = false;
        subscription.input_target = None;
        let _ = subscription.cancel.send(());
        subscription.task.abort();
        let capture_result = if subscription.task_joined {
            Ok(())
        } else {
            let result =
                tokio::time::timeout(crate::lifecycle::CLEANUP_WAIT, &mut subscription.task)
                    .await
                    .map_err(|_| {
                        "capture teardown still pending; no unsubscribe acknowledgement".to_string()
                    })?;
            subscription.task_joined = true;
            joined(result)
        };
        self.subscriptions.remove(&id);
        if let Err(error) = capture_result {
            self.worker_failure = true;
            return Err(error);
        }
        if self.worker_failure {
            return Err("prior connection worker failed; teardown is not confirmed".into());
        }
        #[cfg(all(unix, any(test, target_os = "linux")))]
        if let Some(capture) = self.native_capture.remove(&id) {
            capture.revoke();
        }
        self.refresh_audio();
        while let Some(task) = self.retiring_tasks.last_mut() {
            let result = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .map_err(|_| "audio/clipboard teardown still pending".to_string())?;
            self.retiring_tasks.pop();
            if let Err(error) = joined(result) {
                self.worker_failure = true;
                return Err(error);
            }
        }
        tokio::time::timeout(
            crate::lifecycle::CLEANUP_WAIT,
            crate::lifecycle::wait_native_stops(&self.resources),
        )
        .await
        .map_err(|_| "native teardown still pending; unsubscribe outcome is unknown".to_string())?;
        if self
            .resources
            .native_stop_errors
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
        {
            return Err("native capture stop failed; teardown is not confirmed".into());
        }
        Ok(())
    }
    async fn shutdown(mut self) -> Result<(), String> {
        crate::lifecycle::publish(self.id, &self.resources, "closing");
        match tokio::time::timeout(crate::lifecycle::CLEANUP_WAIT, self.shutdown_owned()).await {
            Ok(result) => result,
            Err(_) => {
                self.terminal_reported = true;
                crate::lifecycle::publish(self.id, &self.resources, "teardown_unknown");
                Err("teardown deadline expired; native owners remain retained until actual completion".into())
            }
        }
    }
    async fn shutdown_owned(&mut self) -> Result<(), String> {
        let mut worker_failure = self.worker_failure;
        let _ = self.cancel.send(());
        if let Some(cancel) = self.clipboard_cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(task) = self.clipboard_task.take() {
            task.abort();
            self.retiring_tasks.push(task);
        }
        for subscription in self.subscriptions.values_mut() {
            let _ = subscription.cancel.send(());
            subscription.task.abort();
        }
        for subscription in self.subscriptions.values_mut() {
            if !subscription.task_joined {
                worker_failure |= joined((&mut subscription.task).await).is_err();
                subscription.task_joined = true;
            }
        }
        self.subscriptions.clear();
        #[cfg(all(unix, any(test, target_os = "linux")))]
        for (_, capture) in self.native_capture.drain() {
            capture.revoke();
        }
        for group in self.audio_groups.values_mut() {
            let _ = group.cancel.send(());
            for task in group._tasks.drain(..) {
                task.abort();
                self.retiring_tasks.push(task);
            }
        }
        self.audio_groups.clear();
        for task in &mut self.retiring_tasks {
            worker_failure |= joined(task.await).is_err();
        }
        self.retiring_tasks.clear();
        // Parent cancellation joins its file/clipboard child JoinSet and drops
        // talkback. Do not abort it before that shutdown has executed.
        worker_failure |= !matches!((&mut self.services_task).await, Ok(Ok(())));
        self._reporter.handle.abort();
        worker_failure |= joined((&mut self._reporter.handle).await).is_err();
        self.sender_task.abort();
        worker_failure |= match (&mut self.sender_task).await {
            Ok(Ok(())) => false,
            Err(error) if error.is_cancelled() => false,
            _ => true,
        };
        crate::lifecycle::wait_native_stops(&self.resources).await;
        use std::sync::atomic::Ordering::SeqCst;
        let confirmed = !worker_failure
            && self.resources.workers.load(SeqCst) == 0
            && self.resources.native_streams.load(SeqCst) == 0
            && self.resources.native_stops_pending.load(SeqCst) == 0
            && self.resources.native_stop_errors.load(SeqCst) == 0;
        self.terminal_reported = true;
        crate::lifecycle::publish(
            self.id,
            &self.resources,
            if confirmed {
                "closed"
            } else {
                "teardown_failed"
            },
        );
        if confirmed {
            Ok(())
        } else {
            Err("native resources did not confirm successful teardown".into())
        }
    }
    fn prune_finished_subscriptions(&mut self, retain_for_local_share: bool) {
        self.subscriptions.retain(|_, subscription| {
            if !subscription.task.is_finished() {
                return true;
            }
            #[cfg(all(unix, any(test, target_os = "linux")))]
            if retain_for_local_share {
                // Retain the bounded (MAX_SUBSCRIPTIONS) request/revision intent
                // so a local picker can recover a Wayland viewer after the
                // initial compatibility capture ended. It grants no input or
                // audio and is removed by unsubscribe/connection expiry.
                subscription.supports_input = false;
                subscription.input_target = None;
                subscription.audio_group = None;
                return true;
            }
            use std::future::Future;
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            if !subscription.task_joined
                && let std::task::Poll::Ready(result) =
                    std::pin::Pin::new(&mut subscription.task).poll(&mut cx)
            {
                if let Err(error) = joined(result) {
                    self.worker_failure = true;
                    self.retired.insert(subscription.request.id, Err(error));
                }
            }
            false
        });
        #[cfg(not(all(unix, any(test, target_os = "linux"))))]
        let _ = retain_for_local_share;
        #[cfg(all(unix, any(test, target_os = "linux")))]
        self.native_capture.retain(|id, capture| {
            if self.subscriptions.contains_key(id) {
                true
            } else {
                capture.revoke();
                false
            }
        });
        self.refresh_audio();
    }

    fn capture_sources(
        &self,
    ) -> Result<Vec<protocol::session::CaptureSourceInfo>, Box<dyn Error + Send + Sync>> {
        #[cfg(test)]
        if let Some(catalog) = TEST_CAPTURE_CATALOG.with(|value| value.borrow().clone()) {
            return Ok(catalog);
        }
        let legacy = crate::capture_sources::list();
        #[cfg(not(all(unix, any(test, target_os = "linux"))))]
        let mut sources = legacy?;
        #[cfg(all(unix, any(test, target_os = "linux")))]
        let mut sources = Vec::new();
        #[cfg(all(unix, any(test, target_os = "linux")))]
        for capture in self.native_capture.values() {
            if !capture.is_revoked() {
                sources.push(capture.source_info());
            }
        }
        #[cfg(all(unix, any(test, target_os = "linux")))]
        match legacy {
            Ok(legacy) => sources.extend(legacy),
            Err(error) if sources.is_empty() => return Err(error),
            Err(_) => {}
        }
        sources.truncate(256);
        Ok(sources)
    }

    #[cfg(all(unix, any(test, target_os = "linux")))]
    fn commit_native_capture(
        &mut self,
        target: LocalPortalShareTarget,
        current: PeerOwner,
        capture: PreparedPortalCapture,
    ) -> Result<protocol::session::CaptureSourceInfo, String> {
        if target.owner != current
            || !self
                .subscriptions
                .get(&target.subscription_id)
                .is_some_and(|subscription| subscription.source_revision == target.source_revision)
            || capture.is_revoked()
        {
            capture.revoke();
            return Err(
                "local share target changed or was revoked; previous source was kept".into(),
            );
        }
        let info = capture.source_info();
        // Existing streaming tasks retain their own grant until SwitchSource
        // tears them down. A cancelled picker never reaches this mutation.
        self.native_capture.insert(target.subscription_id, capture);
        Ok(info)
    }

    fn new(id: u32, addr: SocketAddr, udp: UdpSender, config: &HostServiceConfig) -> Self {
        let (cancel, cancel_rx) = broadcast::channel(8);
        let (sender, sender_task) =
            ScheduledDataSender::spawn(udp, addr, ScheduledDataSenderConfig::default());
        let (files, file_inbound_rx) = channel_if(
            config.enable_file_transfer
                || env_flag_enabled("REMOTE_PLAY_FILE_CLIPBOARD")
                || std::env::var_os("REMOTE_PLAY_HOST_SEND_FILE").is_some(),
        );
        let (talkback, talkback_inbound_rx) =
            channel_if(config.enable_talkback && cfg!(target_os = "macos"));
        #[cfg(target_os = "macos")]
        let (talkback_settings, talkback_settings_rx) = if talkback.is_some() {
            let (tx, rx) =
                watch::channel(crate::talkback_player::TalkbackPlaybackSettings::default());
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        #[cfg(not(target_os = "macos"))]
        let talkback_settings_rx = None;
        let reporter = crate::start_scheduled_sender_reporter(sender.clone(), cancel.subscribe());
        let clipboard_enabled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let services_task = tokio::spawn(crate::connection_services::run(
            crate::connection_services::ConnectionServicesConfig {
                clipboard_enabled: clipboard_enabled.clone(),
                session_id: id,
                scheduled_sender: sender.clone(),
                cancel_rx,
                file_inbound_rx,
                talkback_inbound_rx,
                talkback_settings_rx,
                host_send_file: std::env::var_os("REMOTE_PLAY_HOST_SEND_FILE").map(Into::into),
            },
        ));
        let resources = Arc::new(crate::lifecycle::Resources::default());
        crate::lifecycle::publish(id, &resources, "open");
        Self {
            versioned_control: false,
            id,
            last_seen: Instant::now(),
            cancel,
            sender,
            clipboard: None,
            clipboard_cancel: None,
            clipboard_enabled,
            files,
            talkback,
            #[cfg(target_os = "macos")]
            talkback_settings,
            subscriptions: HashMap::new(),
            input_leases: HashMap::new(),
            retired: HashMap::new(),
            worker_failure: false,
            audio_groups: HashMap::new(),
            _reporter: reporter,
            resources,
            terminal_reported: false,
            services_task,
            sender_task,
            clipboard_task: None,
            retiring_tasks: Vec::new(),
            #[cfg(all(unix, any(test, target_os = "linux")))]
            native_capture: HashMap::new(),
        }
    }

    fn set_clipboard(&mut self, enabled: bool) {
        self.clipboard_enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        if self.clipboard.is_some() == enabled {
            return;
        }
        if let Some(cancel) = self.clipboard_cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(task) = self.clipboard_task.take() {
            task.abort();
            self.retiring_tasks.push(task);
        }
        self.clipboard = None;
        if enabled {
            let (tx, cancel, task) =
                crate::connection_services::start_clipboard(self.sender.clone());
            self.clipboard = Some(tx);
            self.clipboard_cancel = Some(cancel);
            self.clipboard_task = Some(task);
        }
    }

    fn refresh_audio(&mut self) {
        self.audio_groups.retain(|key, group| {
            let members: Vec<_> = self
                .subscriptions
                .values()
                .filter(|s| s.audio_group == Some(*key))
                .collect();
            if members.is_empty() {
                for task in group._tasks.drain(..) {
                    task.abort();
                    self.retiring_tasks.push(task);
                }
                return false;
            }
            let paused = !members.iter().any(|s| s.audio_active);
            if group.settings.borrow().paused != paused {
                group.settings.send_modify(|s| s.paused = paused);
            }
            true
        });
    }

    async fn subscribe(
        &mut self,
        request: SubscriptionRequest,
        addr: SocketAddr,
        udp: UdpSender,
        stats: Arc<Statistics>,
        legacy: bool,
    ) -> Result<SessionCommand, String> {
        self.subscribe_with_preflight(request, addr, udp, stats, legacy, || {
            #[cfg(target_os = "windows")]
            crate::capture_readiness::ffmpeg_program()?;
            Ok(())
        })
        .await
    }

    async fn subscribe_with_preflight(
        &mut self,
        request: SubscriptionRequest,
        addr: SocketAddr,
        udp: UdpSender,
        stats: Arc<Statistics>,
        legacy: bool,
        preflight: impl FnOnce() -> Result<(), String>,
    ) -> Result<SessionCommand, String> {
        validate_host_video_settings(
            request.width,
            request.height,
            request.fps,
            request.bitrate_kbps,
        )
        .map_err(str::to_owned)?;
        if self.retired.contains_key(&request.id) || self.retired.len() >= 4096 {
            return Err("subscription was closed; use a new ID".into());
        }
        if self
            .resources
            .native_stops_pending
            .load(std::sync::atomic::Ordering::SeqCst)
            != 0
            || self
                .resources
                .native_stop_errors
                .load(std::sync::atomic::Ordering::SeqCst)
                != 0
            || self.retiring_tasks.iter().any(|task| !task.is_finished())
        {
            return Err(
                "previous native/audio teardown is unconfirmed; no replacement started".into(),
            );
        }
        if request.width > 8192
            || request.height > 8192
            || u64::from(request.width) * u64::from(request.height) > 33_554_432
        {
            return Err("requested size exceeds this host's capture budget".into());
        }
        if request.id == 0 || request.id >= 0x1000_0000 {
            return Err("subscription ID outside supported range".into());
        }
        if let Some(existing) = self.subscriptions.get_mut(&request.id)
            && existing.request.source == request.source
            && existing.request.audio == request.audio
            && !existing.task.is_finished()
        {
            // Updating rates on an unchanged source must not restart capture,
            // reset the packet sequence or unpause an inactive subscription.
            if existing.request != request {
                existing.request = request.clone();
                existing.settings.send_modify(|v| {
                    v.width = request.width;
                    v.height = request.height;
                    v.fps = request.fps;
                    v.bitrate_kbps = request.bitrate_kbps;
                });
                existing.keyframe.store(true, Relaxed);
            }
            return Ok(SessionCommand::Subscribed {
                id: request.id,
                audio_owner: existing
                    .audio_group
                    .and_then(|key| self.audio_groups.get(&key).map(|g| g.owner)),
                supports_input: existing.supports_input_now(),
            });
        }
        if self.subscriptions.len() >= MAX_SUBSCRIPTIONS
            && !self.subscriptions.contains_key(&request.id)
        {
            return Err("subscription limit reached".into());
        }
        // Check the capture dependency before acquiring microphones, audio groups,
        // or subscription tasks. A permanent packaging failure must remain inert.
        if let Err(reason) = preflight() {
            eprintln!(
                "Capture preflight rejected subscription {}: {reason}",
                request.id
            );
            return Err(reason);
        }
        let sources = if request.source == CaptureSource::MainDisplay {
            Vec::new()
        } else {
            #[cfg(all(unix, any(test, target_os = "linux")))]
            if let Some(capture) = self
                .native_capture
                .values()
                .find(|capture| capture.source_info().source == request.source)
            {
                vec![capture.source_info()]
            } else {
                self.capture_sources().map_err(|e| e.to_string())?
            }
            #[cfg(not(all(unix, any(test, target_os = "linux"))))]
            self.capture_sources().map_err(|e| e.to_string())?
        };
        #[cfg(all(unix, any(test, target_os = "linux")))]
        let native_capture = self
            .native_capture
            .iter()
            .find_map(|(id, capture)| {
                (capture.source_info().source == request.source).then_some((*id, capture))
            })
            .map(|(id, capture)| {
                if id != request.id || capture.is_revoked() {
                    Err("native share belongs to another or revoked subscription".to_string())
                } else {
                    Ok(capture.clone())
                }
            })
            .transpose()?;
        #[cfg(all(unix, any(test, target_os = "linux")))]
        if let Some(capture) = &native_capture {
            let info = capture.source_info();
            crate::linux_raw_encode::RawSettings {
                width: request.width,
                height: request.height,
                fps: request.fps,
                bitrate_kbps: request.bitrate_kbps,
            }
            .validate_for(info.width, info.height)
            .map_err(|error| error.to_string())?;
        }
        let selected = sources.iter().find(|s| s.source == request.source);
        if request.source != CaptureSource::MainDisplay && selected.is_none() {
            return Err("selected capture source is unavailable".into());
        }
        let supports_input = selected.map_or(request.source == CaptureSource::MainDisplay, |s| {
            s.supports_input
        });
        let input_target = input_target_for(
            request.source,
            selected.and_then(|s| s.process_id),
            supports_input,
        );
        #[cfg(all(unix, any(test, target_os = "linux")))]
        let input_target = if native_capture.is_some() {
            Some(InputTarget::Portal)
        } else {
            input_target
        };
        let supports_input = supports_input && input_target.is_some();
        let mut initial_settings = stream_settings(&request);
        let mut activity_revision = 0;
        let mut settings_revision = 0;
        let mut source_revision = 0;
        let mut audio_active = true;
        let mut video_sequence = Arc::new(std::sync::atomic::AtomicU16::new(0));
        if let Some(existing) = self.subscriptions.get_mut(&request.id) {
            initial_settings.paused = existing.settings.borrow().paused;
            activity_revision = existing.revision;
            settings_revision = existing.settings_revision;
            source_revision = existing.source_revision;
            audio_active = existing.audio_active;
            video_sequence = existing.video_sequence.clone();
            // abort() alone only requests cancellation. Wait for actual teardown
            // before a replacement can touch capture/encoder resources.
            let _ = existing.cancel.send(());
            existing.task.abort();
            if !existing.task_joined {
                let result =
                    tokio::time::timeout(crate::lifecycle::CLEANUP_WAIT, &mut existing.task)
                        .await
                        .map_err(|_| {
                            "previous capture did not stop; replacement was not started".to_string()
                        })?;
                existing.task_joined = true;
                if let Err(error) = joined(result) {
                    self.worker_failure = true;
                    return Err(error);
                }
            }
            tokio::time::timeout(
                crate::lifecycle::CLEANUP_WAIT,
                crate::lifecycle::wait_native_stops(&self.resources),
            )
            .await
            .map_err(|_| {
                "previous native stop is unknown; replacement was not started".to_string()
            })?;
            if self
                .resources
                .native_stop_errors
                .load(std::sync::atomic::Ordering::SeqCst)
                != 0
            {
                return Err("previous native stop failed; replacement was not started".into());
            }
            #[cfg(all(unix, any(test, target_os = "linux")))]
            if let Some(previous) = &existing.native_capture {
                previous.release_input().await.map_err(|e| e.to_string())?;
                if native_capture
                    .as_ref()
                    .is_some_and(|next| next.generation() == previous.generation())
                {
                    // A settings/resubscribe update reuses this same consented
                    // source. Old Subscription::drop must not revoke its lease.
                    existing.native_capture = None;
                }
            }
        }
        self.subscriptions.remove(&request.id);
        // Replacement subscriptions must release the previous Windows capture
        // reservation before acquiring the next one. Otherwise same-session
        // display/source switches fail against their own reservation.
        #[cfg(target_os = "windows")]
        let capture_reservation = Arc::new(WindowsVideoCaptureReservation::acquire()?);
        #[cfg(target_os = "windows")]
        let task_reservation = capture_reservation.clone();
        #[cfg(target_os = "windows")]
        let wants_audio_group = legacy && env_flag_enabled("REMOTE_PLAY_MICROPHONE_CAPTURE");
        #[cfg(not(target_os = "windows"))]
        let wants_audio_group = request.audio || legacy;
        let audio_key = if wants_audio_group {
            Some(
                selected
                    .and_then(|s| s.process_id)
                    .map_or(AudioGroupKey::System, AudioGroupKey::Application),
            )
        } else {
            None
        };
        if let Some(key) = audio_key {
            self.audio_groups.entry(key).or_insert_with(|| {
                let (cancel, cancel_rx) = broadcast::channel(4);
                let (settings, rx) = watch::channel(stream_settings(&request));
                let tasks = start_subscription_audio(SubscriptionAudioConfig {
                    resources: self.resources.clone(),
                    session_id: request.id,
                    source: request.source,
                    client_addr: addr,
                    udp_sender: udp.clone(),
                    scheduled_sender: self.sender.clone(),
                    stats: stats.clone(),
                    cancel_rx,
                    stream_settings_rx: Some(rx),
                    include_audio: request.audio,
                    // Legacy screen viewing is not consent to capture the host microphone.
                    include_microphone: legacy
                        && env_flag_enabled("REMOTE_PLAY_MICROPHONE_CAPTURE"),
                });
                AudioGroup {
                    owner: request.id,
                    settings,
                    cancel,
                    _tasks: tasks,
                }
            });
        }
        let (cancel, cancel_rx) = broadcast::channel(4);
        let (settings, rx) = watch::channel(initial_settings);
        let keyframe = Arc::new(AtomicBool::new(true));
        let stream_config = StreamingRunConfig {
            session_id: request.id,
            client_addr: addr,
            width: request.width,
            height: request.height,
            fps: request.fps,
            bitrate_kbps: request.bitrate_kbps,
            udp_sender: udp.clone(),
            stats,
            resources: self.resources.clone(),
            cancel_rx,
            scheduled_sender: self.sender.clone(),
            source: request.source,
            stream_settings_rx: Some(rx),
            keyframe_requested: keyframe.clone(),
            video_sequence: video_sequence.clone(),
            #[cfg(all(unix, any(test, target_os = "linux")))]
            native_capture: native_capture.clone(),
        };
        let id = request.id;
        let task = tokio::spawn(async move {
            // Stop/drop cannot release the global Windows slot until FFmpeg's
            // destructor has actually completed, even if the task is aborted.
            #[cfg(target_os = "windows")]
            let _reservation = task_reservation;
            if let Err(error) = run_streaming(stream_config).await {
                eprintln!("Capture subscription {id} failed: {error}");
                send_session(
                    &udp,
                    addr,
                    SessionCommand::Error {
                        request_id: id,
                        reason: error.to_string(),
                    },
                )
                .await;
            }
        });
        let audio_owner = audio_key.and_then(|key| self.audio_groups.get(&key).map(|g| g.owner));
        self.subscriptions.insert(
            id,
            Subscription {
                request,
                settings,
                revision: activity_revision,
                settings_revision,
                keyframe,
                cancel,
                task,
                task_joined: false,
                audio_group: audio_key,
                audio_active,
                supports_input,
                input_target,
                #[cfg(target_os = "macos")]
                input_probe: mac_input_status,
                last_input_error: None,
                source_revision,
                video_sequence,
                #[cfg(target_os = "windows")]
                _capture_reservation: capture_reservation,
                #[cfg(all(unix, any(test, target_os = "linux")))]
                native_capture,
            },
        );
        self.refresh_audio();
        Ok(SessionCommand::Subscribed {
            id,
            audio_owner,
            supports_input: self.subscriptions[&id].supports_input_now(),
        })
    }

    async fn switch_source(
        &mut self,
        id: u32,
        request_id: u32,
        source: CaptureSource,
        addr: SocketAddr,
        udp: UdpSender,
        stats: Arc<Statistics>,
    ) -> Result<SessionCommand, String> {
        let current = self
            .subscriptions
            .get(&id)
            .ok_or("subscription is unavailable")?;
        if request_id == 0 || request_id < current.source_revision {
            return Err("stale capture-source request".into());
        }
        if request_id == current.source_revision {
            if source != current.request.source {
                return Err("source request ID reused".into());
            }
            return Ok(SessionCommand::SourceSwitched {
                id,
                request_id,
                source,
                supports_input: current.supports_input_now(),
            });
        }
        let mut request = current.request.clone();
        request.source = source;
        self.subscribe(request, addr, udp, stats, false).await?;
        let current = self
            .subscriptions
            .get_mut(&id)
            .ok_or("subscription disappeared")?;
        current.source_revision = request_id;
        Ok(SessionCommand::SourceSwitched {
            id,
            request_id,
            source,
            supports_input: current.supports_input_now(),
        })
    }

    fn set_activity(
        &mut self,
        id: u32,
        revision: u64,
        video: bool,
        audio: bool,
    ) -> Option<SessionCommand> {
        let subscription = self.subscriptions.get_mut(&id)?;
        if revision > subscription.revision {
            subscription.revision = revision;
            subscription.audio_active = audio;
            subscription.settings.send_modify(|s| s.paused = !video);
            if video {
                subscription.keyframe.store(true, Relaxed);
            }
        }
        let reply = SessionCommand::Activity {
            id,
            revision: subscription.revision,
            video: !subscription.settings.borrow().paused,
            audio: subscription.audio_active,
        };
        self.refresh_audio();
        Some(reply)
    }

    fn can_legacy_input(&self, id: u32) -> bool {
        self.can_input(id)
            && self
                .subscriptions
                .get(&id)
                .is_some_and(|s| s.input_target == Some(InputTarget::Desktop))
    }
    fn can_scoped_input(&self, id: u32, revision: u32) -> bool {
        self.can_input(id) && self.accepts_input_revision(id, revision)
    }
    fn accepts_input_revision(&self, id: u32, revision: u32) -> bool {
        self.subscriptions.get(&id).is_some_and(|s| {
            s.source_revision == revision && !s.task.is_finished() && !s.settings.borrow().paused
        })
    }

    fn can_input(&self, id: u32) -> bool {
        self.subscriptions.get(&id).is_some_and(|s| {
            s.supports_input_now() && !s.task.is_finished() && !s.settings.borrow().paused
        })
    }
}

/// Holds the actual session resource as well as its authenticated subscription.
/// Removal of a peer cannot lose the resource needed to release pressed input.
#[derive(Default)]
struct ActiveInput {
    owner: Option<(SocketAddr, u32)>,
    binding: Option<InputBinding>,
    #[cfg(all(unix, any(test, target_os = "linux")))]
    capture: Option<PreparedPortalCapture>,
}
impl ActiveInput {
    async fn release(
        &mut self,
        injector: &LazyInputInjector,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.owner = None;
        self.binding = None;
        injector.release_all_input();
        #[cfg(all(unix, any(test, target_os = "linux")))]
        if let Some(capture) = self.capture.take()
            && let Err(error) = capture.release_input().await
        {
            // Closing the granted session is the bounded recovery if a
            // compositor cannot confirm key-up; no global input fallback.
            capture.revoke();
            return Err(error.into());
        }
        Ok(())
    }
}

async fn retire_input_before_control(peer:&mut Connection,owner:&mut ActiveInput,injector:&LazyInputInjector,addr:SocketAddr,command:&SessionCommand) {
    let Some(id)=peer.retires_input(command) else{return;};
    peer.input_leases.remove(&id);
    if owner.owner==Some((addr,id)) {let _=owner.release(injector).await;}
}

fn input_deadline(
    created: u64,
    now_ms: u64,
    now: std::time::Instant,
) -> Option<std::time::Instant> {
    let age = now_ms.checked_sub(created)?;
    let remaining = protocol::realtime_input::MAX_INPUT_AGE_MS.checked_sub(age)?;
    Some(now + Duration::from_millis(remaining))
}

async fn dispatch_realtime_input(
    peer: &mut Connection,
    owner: &mut ActiveInput,
    injector: &LazyInputInjector,
    udp: &UdpSender,
    addr: SocketAddr,
    packet: RealtimeInput,
) {
    // Convert once, before any cleanup/await. Every item in a held-state batch
    // keeps this deadline; later wall-clock reads cannot renew its budget.
    let deadline = input_deadline(
        packet.created_host_ms,
        now_unix_ms(),
        std::time::Instant::now(),
    );
    let Some(events) = peer.accept_input(&packet) else {
        return;
    };
    let Some(deadline) = deadline else {
        return;
    };
    let id = packet.binding.subscription_id;
    if owner.owner == Some((addr, id)) && owner.binding != Some(packet.binding) {
        if owner.release(injector).await.is_err() {
            peer.input_leases.remove(&id);
            return;
        }
    }
    for event in events {
        if !inject_fresh(injector, owner, peer, udp, addr, id, event, Some(deadline)).await {
            peer.input_leases.remove(&id);
            return;
        }
        owner.binding = Some(packet.binding);
    }
    if matches!(packet.payload, InputPayload::Release) {
        send_session(
            udp,
            addr,
            SessionCommand::InputHandled {
                binding: packet.binding,
                sequence: packet.sequence,
            },
        )
        .await;
    }
}

async fn expire_input_owners(
    peers: &mut HashMap<SocketAddr, Connection>,
    owner: &mut ActiveInput,
    injector: &LazyInputInjector,
) {
    for peer in peers.values_mut() {
        let expired = peer
            .input_leases
            .iter()
            .filter(|(_, slot)| slot.lease.expired(now_unix_ms()))
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in expired {
            peer.input_leases.remove(&id);
        }
    }
    if let Some(binding) = owner.binding {
        let current = owner
            .owner
            .and_then(|(addr, id)| peers.get(&addr)?.input_leases.get(&id));
        if current.is_none_or(|slot| slot.lease.binding != binding) {
            let _ = owner.release(injector).await;
        }
    }
}

fn channel_if(
    enabled: bool,
) -> (
    Option<mpsc::Sender<DataEnvelope>>,
    Option<mpsc::Receiver<DataEnvelope>>,
) {
    if enabled {
        let (tx, rx) = mpsc::channel(256);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    }
}
async fn send_session(sender: &UdpSender, addr: SocketAddr, command: SessionCommand) {
    let _ = sender
        .send_control(&ControlMessage::Session(Box::new(command)), addr)
        .await;
}
async fn inject_fresh(
    injector: &LazyInputInjector,
    owner: &mut ActiveInput,
    peer: &mut Connection,
    udp: &UdpSender,
    addr: SocketAddr,
    id: u32,
    event: InputEvent,
    deadline: Option<std::time::Instant>,
) -> bool {
    if !peer.can_input(id) {
        let reason = peer
            .subscriptions
            .get(&id)
            .and_then(|s| s.input_status().err())
            .unwrap_or_else(|| {
                "Selected source is inactive; resume its video before enabling control".into()
            });
        input_error(peer, udp, addr, id, &reason).await;
        return false;
    }
    let Some(target) = peer.subscriptions.get(&id).and_then(|s| s.input_target) else {
        return false;
    };
    #[cfg(all(unix, any(test, target_os = "linux")))]
    let capture = peer
        .subscriptions
        .get(&id)
        .and_then(|s| s.native_capture.clone());
    let mut changed = owner.owner != Some((addr, id));
    #[cfg(all(unix, any(test, target_os = "linux")))]
    {
        changed |= owner
            .capture
            .as_ref()
            .map(PreparedPortalCapture::generation)
            != capture.as_ref().map(PreparedPortalCapture::generation);
    }
    if changed {
        if let Err(error) = owner.release(injector).await {
            input_error(peer, udp, addr, id, &error.to_string()).await;
            return false;
        }
        owner.owner = Some((addr, id));
        #[cfg(all(unix, any(test, target_os = "linux")))]
        {
            owner.capture = capture.clone();
        }
    }
    if deadline.is_some_and(|deadline|std::time::Instant::now()>=deadline) {return false;}
    #[cfg(all(unix, any(test, target_os = "linux")))]
    let result = match capture {
        Some(capture) => {
            let result=capture.inject_input_before(event,deadline).await;
            if result==Err(crate::linux_portal::runtime::PortalInputError::Transport) {
                // An unknown up outcome cannot be retried. Close this exact
                // compositor lease to clear its state without a second up RPC.
                capture.revoke();
            }
            result.map_err(|e|Box::new(e) as Box<dyn Error+Send+Sync>)
        },
        None => injector.inject_target_before(target, event,deadline),
    };
    #[cfg(not(all(unix, any(test, target_os = "linux"))))]
    let result = injector.inject_target_before(target, event,deadline);
    if let Err(error) = result {
        let _ = owner.release(injector).await;
        input_error(peer, udp, addr, id, &error.to_string()).await;
        return false;
    }
    true
}
#[cfg(test)]
async fn inject(injector:&LazyInputInjector,owner:&mut ActiveInput,peer:&mut Connection,udp:&UdpSender,addr:SocketAddr,id:u32,event:InputEvent) -> bool {
    inject_fresh(injector,owner,peer,udp,addr,id,event,None).await
}

async fn input_error(
    peer: &mut Connection,
    udp: &UdpSender,
    addr: SocketAddr,
    id: u32,
    reason: &str,
) {
    if let Some(s) = peer.subscriptions.get_mut(&id)
        && s.last_input_error
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(2))
    {
        s.last_input_error = Some(Instant::now());
        send_session(
            udp,
            addr,
            SessionCommand::Error {
                request_id: id,
                reason: format!("INPUT_UNAVAILABLE: {reason}"),
            },
        )
        .await;
    }
}

async fn report_input_readiness(peer: &mut Connection, udp: &UdpSender, addr: SocketAddr, id: u32) {
    let reason = peer
        .subscriptions
        .get(&id)
        .filter(|s| s.supports_input)
        .and_then(|s| s.input_status().err());
    if let Some(reason) = reason {
        input_error(peer, udp, addr, id, &reason).await;
    }
}

type BoundInjector = (InputTarget, Box<dyn InputInjector + Send + Sync>);
#[derive(Default)]
struct LazyInputInjector(std::sync::Mutex<Option<BoundInjector>>);
impl LazyInputInjector {
    fn inject_target(
        &self,
        target: InputTarget,
        event: InputEvent,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.inject_target_before(target,event,None)
    }
    fn inject_target_before(&self,target:InputTarget,event:InputEvent,deadline:Option<std::time::Instant>) -> Result<(),Box<dyn Error+Send+Sync>> {
        let mut current = self.0.lock().map_err(|_| "Input scope lock poisoned")?;
        if current
            .as_ref()
            .is_none_or(|(previous, _)| *previous != target)
        {
            if let Some((_, previous)) = current.take() {
                previous.release_all_input();
            }
            let created: Box<dyn InputInjector + Send + Sync> = match target {
                InputTarget::Desktop => {
                    #[cfg(target_os = "macos")]
                    {
                        Box::new(crate::input_injector::MacInputInjector::new()?)
                    }
                    #[cfg(target_os = "linux")]
                    {
                        Box::new(crate::linux_input::LinuxUinputInjector::new()?)
                    }
                    #[cfg(target_os = "windows")]
                    {
                        Box::new(crate::windows_input::WindowsInputInjector::new()?)
                    }
                }
                #[cfg(target_os = "macos")]
                InputTarget::Display { id } => {
                    Box::new(crate::input_injector::MacInputInjector::for_display(id)?)
                }
                InputTarget::Window { id, pid } => {
                    #[cfg(target_os = "macos")]
                    {
                        Box::new(crate::input_injector::MacInputInjector::for_window(
                            id, pid,
                        )?)
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        let _ = (id, pid);
                        return Err("This host does not implement window-scoped input; no desktop fallback was used".into());
                    }
                }
                #[cfg(all(unix, any(test, target_os = "linux")))]
                InputTarget::Portal => {
                    return Err("Portal input requires the exact consented session resource".into());
                }
            };
            *current = Some((target, created));
        }
        let injector=&current.as_ref().ok_or("Input injector unavailable")?.1;
        match deadline {Some(deadline)=>injector.inject_input_before(event,deadline),None=>injector.inject_input(event)}
    }
}
impl InputInjector for LazyInputInjector {
    fn inject_input(&self, event: InputEvent) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.inject_target(InputTarget::Desktop, event)
    }
    fn release_all_input(&self) {
        if let Ok(current) = self.0.lock()
            && let Some((_, injector)) = current.as_ref()
        {
            injector.release_all_input();
        }
    }
}

pub async fn run_host_service(
    config: HostServiceConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    run_host_service_inner(
        config,
        #[cfg(all(unix, any(test, target_os = "linux")))]
        None,
    )
    .await
}

/// Optional local GUI/CLI integration. Incoming wire commands cannot construct
/// this channel or trigger a picker. The native worker supplies a prepared grant.
#[cfg(target_os = "linux")]
pub async fn run_host_service_with_portal_shares(
    config: HostServiceConfig,
    controls: mpsc::Receiver<LocalPortalShareCommand>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    run_host_service_inner(config, Some(controls)).await
}

async fn run_host_service_inner(
    config: HostServiceConfig,
    #[cfg(all(unix, any(test, target_os = "linux")))] mut local_shares: Option<
        mpsc::Receiver<LocalPortalShareCommand>,
    >,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let multiplexer = UdpMultiplexer::bind(&config.bind_addr.to_string()).await?;
    let (udp_sender, udp_receiver) = multiplexer.split();
    let injector = LazyInputInjector::default();
    let mut peers = HashMap::<SocketAddr, Connection>::new();
    let mut closing = HashMap::<SocketAddr, ClosingTask>::new();
    let mut closed = HashMap::<SocketAddr, ClosedReceipt>::new();
    let mut authenticated = HashMap::<SocketAddr, ([u8; 16], [u8; 16], u64)>::new();
    let mut psk = load_session_psk();
    let mut require_auth = require_session_auth() || psk.is_some();
    let mut input_owner = ActiveInput::default();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            local = async {
                #[cfg(all(unix, any(test, target_os = "linux")))]
                if let Some(receiver) = &mut local_shares { return receiver.recv().await; }
                #[cfg(all(unix, any(test, target_os = "linux")))]
                return std::future::pending::<Option<LocalPortalShareCommand>>().await;
                #[cfg(not(all(unix, any(test, target_os = "linux"))))]
                std::future::pending::<Option<()>>().await
            } => {
                #[cfg(all(unix, any(test, target_os = "linux")))]
                {
                    let scope = remote_core::shared_files::current_share_scope();
                    let key_unchanged = load_session_psk() == psk;
                    match local {
                        Some(LocalPortalShareCommand::Targets { reply }) => {
                            let targets = peers.iter().filter(|_| key_unchanged).flat_map(|(addr,peer)| {
                                let owner = authenticated.get(addr).and_then(|(nonce,_,_)| PeerOwner::from_authenticated_connection(peer.id, *nonce, scope).ok());
                                peer.subscriptions.iter().filter_map(move |(id,subscription)| owner.map(|owner| LocalPortalShareTarget { addr:*addr, owner, source_revision:subscription.source_revision, subscription_id:*id }))
                            }).collect();
                            let _ = reply.send(targets);
                        }
                        Some(LocalPortalShareCommand::Commit { target, capture, reply }) => {
                            let owner = if key_unchanged { peers.get(&target.addr).and_then(|peer| authenticated.get(&target.addr).and_then(|(nonce,_,_)| PeerOwner::from_authenticated_connection(peer.id, *nonce, scope).ok())) } else { None };
                            let result = if let Some(owner) = owner {
                                peers.get_mut(&target.addr).unwrap().commit_native_capture(target, owner, capture)
                            } else { capture.revoke(); Err("local share requires the same authenticated connection and scope".into()) };
                            let _ = reply.send(result);
                        }
                        None => {
                            local_shares = None;
                            for peer in peers.values_mut() { for (_,capture) in peer.native_capture.drain() { capture.revoke(); } }
                        }
                    }
                }
                #[cfg(not(all(unix, any(test, target_os = "linux"))))]
                let _ = local;
            }
            _ = tick.tick() => {
                expire_input_owners(&mut peers,&mut input_owner,&injector).await;
                let current_key = load_session_psk();
                if current_key != psk {
                    for addr in authenticated.keys() { udp_sender.remove_peer_crypto(*addr); }
                    for addr in peers.keys().copied().collect::<Vec<_>>(){begin_close(&mut peers,&mut closing,addr,false);}
                    closed.retain(|_,receipt|receipt.resources_live());authenticated.clear(); psk = current_key;
                    require_auth = require_session_auth() || psk.is_some();
                }
                let expired: Vec<_> = peers.iter().filter(|(_, peer)| peer.last_seen.elapsed() > Duration::from_secs(15)).map(|(addr, _)| *addr).collect();
                for addr in expired {begin_close(&mut peers,&mut closing,addr,true);}
                let finished=closing.iter().filter(|(_,c)|c.task.is_finished()).map(|(addr,_)|*addr).collect::<Vec<_>>();
                for addr in finished {
                    let mut ending=closing.remove(&addr).unwrap();
                    let result=(&mut ending.task).await.unwrap_or_else(|_|Err("connection teardown task failed".into()));
                    if ending.notify&&authenticated.contains_key(&addr){send_session(&udp_sender,addr,terminal_reply(ending.id,&result)).await;}
                    closed.insert(addr,ClosedReceipt{resources:ending.resources.clone(),id:ending.id,result,at:Instant::now()});
                }
                closed.retain(|_,receipt|receipt.resources_live() || receipt.at.elapsed()<Duration::from_secs(30));
                authenticated.retain(|addr, (_, _, ts)| {
                    let keep = peers.contains_key(addr)||closing.contains_key(addr)||closed.contains_key(addr)|| now_unix_ms().saturating_sub(*ts) < 30_000;
                    if !keep { udp_sender.remove_peer_crypto(*addr); }
                    keep
                });
                #[cfg(target_os = "linux")]
                let retain_for_local_share = local_shares.is_some();
                #[cfg(not(target_os = "linux"))]
                let retain_for_local_share = false;
                for peer in peers.values_mut() { peer.prune_finished_subscriptions(retain_for_local_share); }
                if input_owner.owner.is_some_and(|(addr, id)| !peers.get(&addr).is_some_and(|p| p.can_input(id))) {
                    let _ = input_owner.release(&injector).await;
                }
            }
            received = udp_receiver.recv() => {
                let packet = match received { Ok(packet) => packet, Err(error) => { eprintln!("UDP receive: {error}"); continue; } };
                match packet {
                    MultiplexedPacket::Control(ControlMessage::SessionHello { nonce, timestamp_ms, mac }, addr) => {
                        eprintln!("RP_SESSION_AUTH t_ms={} stage=hello_received local_peer_port={}",now_unix_ms(),addr.port());
                        let result = accept_session_hello(
                            psk.as_deref(), (nonce, timestamp_ms, mac), addr, &udp_sender,
                            &peers, &closing, &mut closed, &mut authenticated,
                        );
                        eprintln!("RP_SESSION_AUTH t_ms={} stage={} local_peer_port={}",now_unix_ms(),if result.is_ok(){"accepted"}else{"rejected"},addr.port());
                        let reply = match result { Ok((salt, timestamp_ms)) => ControlMessage::SessionAccept { salt, timestamp_ms, mac: mac_session_accept(psk.as_ref().unwrap(), &salt, timestamp_ms) }, Err(reason) => ControlMessage::SessionReject { reason } };
                        let _ = udp_sender.send_control(&reply, addr).await;
                    }
                    MultiplexedPacket::Control(message, addr) => {
                        if require_auth && !authenticated.contains_key(&addr) {
                            let _ = udp_sender.send_control(&ControlMessage::SessionReject { reason: "authenticate before opening a connection".into() }, addr).await;
                            continue;
                        }
                        if let Some(peer) = peers.get_mut(&addr) { peer.last_seen = Instant::now(); }
                        if closing.contains_key(&addr){
                            if let ControlMessage::Session(command)=message {
                                if let SessionCommand::Close{connection_id}=*command {
                                    if let Some(reply)=request_close(&mut peers,&mut closing,&closed,addr,connection_id){send_session(&udp_sender,addr,reply).await;}
                                }
                            }
                            // Closing revokes all other command ingress.
                            continue;
                        }
                        match message {
                            ControlMessage::StartStream { width, height, fps, bitrate_kbps, session_id } => {
                                if peers.get(&addr).is_some_and(|peer|!peer.accepts_legacy_control()) {continue;}
                                if validate_host_video_settings(width, height, fps, bitrate_kbps).is_err() { continue; }
                                if connection_admission_error(addr, &peers, &closing, &closed).is_some() { continue; }
                                let peer = peers.entry(addr).or_insert_with(|| Connection::new(session_id, addr, udp_sender.clone(), &config));
                                peer.set_clipboard(config.enable_clipboard_sync);
                                if !peer.subscriptions.contains_key(&session_id) {
                                    if input_owner.owner.is_some_and(|(owner,_)| owner == addr) { let _ = input_owner.release(&injector).await; }
                                    peer.subscriptions.clear(); peer.refresh_audio();
                                }
                                if let Err(reason) = peer.subscribe(SubscriptionRequest { id: session_id, source: CaptureSource::MainDisplay, width, height, fps, bitrate_kbps, audio: env_flag_enabled("REMOTE_PLAY_SYSTEM_AUDIO") }, addr, udp_sender.clone(), config.stats.clone(), true).await {
                                    send_session(&udp_sender, addr, SessionCommand::Error { request_id: session_id, reason }).await;
                                }
                            }
                            ControlMessage::Session(command) => match *command {
                                SessionCommand::ReleaseInput { id } => {
                                    // Old unsequenced release cannot release a newer owner.
                                    send_session(&udp_sender,addr,SessionCommand::Error{request_id:id,reason:"INPUT_UNAVAILABLE: protected input binding required".into()}).await;
                                }
                                SessionCommand::RequestInputBinding{connection_id,id,source_revision,request} => {
                                    if let Some(reply)=peers.get_mut(&addr).and_then(|peer|peer.input_binding(connection_id,id,source_revision,request)) {
                                        send_session(&udp_sender,addr,reply).await;
                                    }
                                }
                                SessionCommand::RealtimeInput(packet) => {
                                    if let Some(peer)=peers.get_mut(&addr) {
                                        dispatch_realtime_input(peer,&mut input_owner,&injector,&udp_sender,addr,packet).await;
                                    }
                                }
                                SessionCommand::Configure { id, revision, width, height, fps, bitrate_kbps } => {
                                    let reply = if validate_host_video_settings(width, height, fps, bitrate_kbps).is_err() || width > 8192 || height > 8192 || u64::from(width) * u64::from(height) > 33_554_432 {
                                        SessionCommand::Error { request_id:id, reason:"unsupported capture settings".into() }
                                    } else if let Some(s) = peers.get_mut(&addr).and_then(|p| p.subscriptions.get_mut(&id)) {
                                        if revision > s.settings_revision {
                                            s.settings_revision = revision;
                                            s.request.width = width; s.request.height = height; s.request.fps = fps; s.request.bitrate_kbps = bitrate_kbps;
                                            s.settings.send_modify(|v| { v.width=width; v.height=height; v.fps=fps; v.bitrate_kbps=bitrate_kbps; });
                                        }
                                        SessionCommand::Configured { id, revision:s.settings_revision }
                                    } else { SessionCommand::Error { request_id:id, reason:"subscription is unavailable".into() } };
                                    send_session(&udp_sender, addr, reply).await;
                                }
                                SessionCommand::SetClipboard { enabled } if peers.contains_key(&addr) => {
                                    let enabled = enabled && config.enable_clipboard_sync;
                                    if enabled { for (other, peer) in &mut peers { if *other != addr { peer.set_clipboard(false); } } }
                                    peers.get_mut(&addr).unwrap().set_clipboard(enabled);
                                    send_session(&udp_sender, addr, SessionCommand::ClipboardState { enabled }).await;
                                }
                                SessionCommand::Open { connection_id, version } => {
                                    #[cfg(target_os = "linux")]
                                    let window_capture = local_shares.is_some();
                                    #[cfg(not(target_os = "linux"))]
                                    let window_capture = cfg!(target_os = "macos");
                                    let reply = if version != SESSION_VERSION || connection_id == 0 { SessionCommand::Error { request_id: connection_id, reason: "unsupported connection version or ID".into() } }
                                    else if let Some(reason) = connection_admission_error(addr, &peers, &closing, &closed) { SessionCommand::Error { request_id: connection_id, reason: reason.into() } }
                                    else if peers.get(&addr).is_some_and(|p| p.id != connection_id) { SessionCommand::Error { request_id: connection_id, reason: "close the existing connection first".into() } }
                                    else { closed.remove(&addr); let peer = peers.entry(addr).or_insert_with(|| Connection::new(connection_id, addr, udp_sender.clone(), &config)); peer.versioned_control=true; SessionCommand::Opened { connection_id, version: SESSION_VERSION, max_subscriptions: MAX_SUBSCRIPTIONS as u32, files: peer.files.is_some(), clipboard: config.enable_clipboard_sync, window_capture } };
                                    send_session(&udp_sender, addr, reply).await;
                                }
                                SessionCommand::ListSources { request_id } if peers.contains_key(&addr) => {
                                    let reply = match peers.get(&addr).unwrap().capture_sources() { Ok(sources) => SessionCommand::Sources { request_id, sources }, Err(e) => SessionCommand::Error { request_id, reason: e.to_string() } };
                                    send_session(&udp_sender, addr, reply).await;
                                }
                                SessionCommand::Subscribe(request) => {
                                    let id = request.id;
                                    if let Some(peer)=peers.get_mut(&addr) {
                                        if !peer.accepts_initial_subscribe(&request) {
                                            send_session(&udp_sender,addr,SessionCommand::Error{request_id:id,reason:"use SwitchSource or a new subscription ID to replace a capture".into()}).await;
                                            continue;
                                        }
                                        retire_input_before_control(peer,&mut input_owner,&injector,addr,&SessionCommand::Subscribe(request.clone())).await;
                                    }
                                    let result = match peers.get_mut(&addr) {
                                        Some(p) => p.subscribe(request, addr, udp_sender.clone(), config.stats.clone(), false).await,
                                        None => Err("open a connection first".to_owned()),
                                    };
                                    let accepted = result.is_ok();
                                    let reply = result.unwrap_or_else(|reason| SessionCommand::Error { request_id: id, reason });
                                    send_session(&udp_sender, addr, reply).await;
                                    if accepted && let Some(peer) = peers.get_mut(&addr) {
                                        report_input_readiness(peer, &udp_sender, addr, id).await;
                                    }
                                }
                                SessionCommand::SwitchSource { id, request_id, source } => {
                                    if let Some(peer)=peers.get_mut(&addr) {
                                        retire_input_before_control(peer,&mut input_owner,&injector,addr,&SessionCommand::SwitchSource{id,request_id,source}).await;
                                    }
                                    let result = match peers.get_mut(&addr) {
                                        Some(p) => p.switch_source(id, request_id, source, addr, udp_sender.clone(), config.stats.clone()).await,
                                        None => Err("open a connection first".to_owned()),
                                    };
                                    let accepted = result.is_ok();
                                    send_session(&udp_sender, addr, result.unwrap_or_else(|reason| SessionCommand::Error { request_id, reason })).await;
                                    if accepted && let Some(peer) = peers.get_mut(&addr) {
                                        report_input_readiness(peer, &udp_sender, addr, id).await;
                                    }
                                }
                                SessionCommand::Unsubscribe { id } => {
                                    if let Some(peer)=peers.get_mut(&addr) {peer.input_leases.remove(&id);}
                                    if input_owner.owner == Some((addr,id)) { let _ = input_owner.release(&injector).await; }
                                    let result=match peers.get_mut(&addr){Some(peer)=>peer.unsubscribe(id).await,None=>Err("connection is unavailable".into())};
                                    send_session(&udp_sender,addr,match result{Ok(())=>SessionCommand::Unsubscribed{id},Err(reason)=>SessionCommand::Error{request_id:id,reason}}).await;
                                }
                                SessionCommand::Close { connection_id } => {
                                    if peers.get(&addr).is_some_and(|p|p.id==connection_id)&&input_owner.owner.is_some_and(|(owner,_)|owner==addr){let _=input_owner.release(&injector).await;}
                                    if let Some(reply)=request_close(&mut peers,&mut closing,&closed,addr,connection_id){send_session(&udp_sender,addr,reply).await;}
                                }
                                SessionCommand::Input { .. } | SessionCommand::SourceInput { .. } => {
                                    // No fallback: these packets cannot prove epoch, order or age.
                                }
                                SessionCommand::SetActivity { id, revision, video, audio } => {
                                    if let Some(peer)=peers.get_mut(&addr) {
                                        retire_input_before_control(peer,&mut input_owner,&injector,addr,&SessionCommand::SetActivity{id,revision,video,audio}).await;
                                    }
                                    if let Some(reply) = peers.get_mut(&addr).and_then(|p| p.set_activity(id, revision, video, audio)) { send_session(&udp_sender, addr, reply).await; }
                                }
                                _ => {}
                            },
                            ControlMessage::StopStream => {
                                if peers.get(&addr).is_some_and(|peer|!peer.accepts_legacy_control()) {continue;}
                                if input_owner.owner.is_some_and(|(owner,_)| owner == addr) { let _ = input_owner.release(&injector).await; }
                                begin_close(&mut peers,&mut closing,addr,false);
                            }
                            ControlMessage::Input(_) => {},
                            ControlMessage::Ping { client_send_ts } if peers.contains_key(&addr) => { let now = now_unix_ms(); let _ = udp_sender.send_control(&ControlMessage::Pong { client_send_ts, host_recv_ts: now, host_send_ts: now }, addr).await; }
                            ControlMessage::SetMediaPaused { session_id, revision, paused } => {
                                if peers.get(&addr).is_some_and(|peer|!peer.accepts_legacy_control()) {continue;}
                                if let Some(peer) = peers.get_mut(&addr) { peer.set_activity(session_id, revision, !paused, !paused);
                                    if let Some(s) = peer.subscriptions.get(&session_id) {
                                        let paused = s.settings.borrow().paused;
                                        let reply = ControlMessage::MediaPauseState { session_id, revision: s.revision, paused };
                                        let _ = udp_sender.send_control(&reply, addr).await;
                                    }
                                }
                            }
                            ControlMessage::UpdateStreamSettings { width, height, fps, bitrate_kbps, session_id } => {
                                if peers.get(&addr).is_some_and(|peer|!peer.accepts_legacy_control()) {continue;}
                                if validate_host_video_settings(width, height, fps, bitrate_kbps).is_ok()
                                    && let Some(s) = peers.get_mut(&addr).and_then(|p| p.subscriptions.get_mut(&session_id)) {
                                    s.settings.send_modify(|value| { value.width = width; value.height = height; value.fps = fps; value.bitrate_kbps = bitrate_kbps; });
                                }
                            }
                            ControlMessage::RequestKeyframe { session_id } => { if let Some(s) = peers.get(&addr).and_then(|p| p.subscriptions.get(&session_id)) { s.keyframe.store(true, Relaxed); } }
                            #[cfg(target_os = "macos")]
                            ControlMessage::AudioControl { session_id, target: AudioControlTarget::ViewerTalkbackPlayback, muted, volume_percent } => {
                                if let Some(peer) = peers.get(&addr) && peer.id == session_id && let Some(tx) = &peer.talkback_settings {
                                    tx.send_modify(|s| { s.muted = muted; s.volume_percent = volume_percent.min(200); });
                                }
                            }
                            _ => {}
                        }
                        if input_owner.owner.is_some_and(|(addr, id)| !peers.get(&addr).is_some_and(|p| p.can_input(id))) { let _ = input_owner.release(&injector).await; }
                    }
                    MultiplexedPacket::Data(envelope, addr) | MultiplexedPacket::DataWithTiming(envelope, _, addr) => {
                        let Some(peer) = peers.get_mut(&addr) else { continue; };
                        peer.last_seen = Instant::now();
                        let target = match envelope.header.kind {
                            ContentKind::ClipboardBundle | ContentKind::ClipboardControl => peer.clipboard.as_ref(),
                            ContentKind::FileManifest | ContentKind::FileChunk | ContentKind::FileControl => peer.files.as_ref(),
                            ContentKind::AudioStreamConfig | ContentKind::AudioOpus => peer.talkback.as_ref(),
                            _ => None,
                        };
                        if let Some(tx) = target { let _ = tx.try_send(envelope); }
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod failure_admission_tests {
    use super::*;

    #[test]
    fn receive_deadline_preserves_age_and_is_independent_of_later_wall_clock_reads() {
        let now = std::time::Instant::now();
        let deadline = input_deadline(1000, 1200, now).unwrap();
        assert_eq!(deadline.duration_since(now), Duration::from_millis(50));
        // The downstream interface carries only Instant, never wall time.
        assert!(input_deadline(1000, 1251, now).is_none());
        assert!(input_deadline(1000, 999, now).is_none());
    }

    struct OwnershipProbe {
        held: Arc<std::sync::atomic::AtomicBool>,
        releases: Arc<std::sync::atomic::AtomicUsize>,
        delay: Duration,
    }
    impl InputInjector for OwnershipProbe {
        fn inject_input(&self, event: InputEvent) -> Result<(), Box<dyn Error + Send + Sync>> {
            if matches!(event, InputEvent::MouseDown(0)) {
                self.held.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
        fn release_all_input(&self) {
            if self.held.swap(false, std::sync::atomic::Ordering::SeqCst) {
                self.releases
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            std::thread::sleep(self.delay);
        }
    }
    #[tokio::test]
    async fn cleanup_wait_cannot_renew_an_initially_valid_input_deadline() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        let (mut peer, addr) = fixture_peer(31).await;
        let (sub, _) = mock_subscription(CaptureSource::MainDisplay);
        sub.settings.send_modify(|s| s.paused = false);
        peer.subscriptions.insert(100, sub);
        let held = Arc::new(AtomicBool::new(true));
        let releases = Arc::new(AtomicUsize::new(0));
        let injector = LazyInputInjector(std::sync::Mutex::new(Some((
            InputTarget::Desktop,
            Box::new(OwnershipProbe {
                held: held.clone(),
                releases: releases.clone(),
                delay: Duration::from_millis(40),
            }),
        ))));
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (udp, _) = mux.split();
        let mut owner = ActiveInput::default();
        owner.owner = Some(("127.0.0.1:29999".parse().unwrap(), 100));
        let deadline = std::time::Instant::now() + Duration::from_millis(20);
        assert!(
            !inject_fresh(
                &injector,
                &mut owner,
                &mut peer,
                &udp,
                addr,
                100,
                InputEvent::MouseDown(0),
                Some(deadline)
            )
            .await
        );
        assert!(!held.load(SeqCst));
        assert_eq!(releases.load(SeqCst), 1);
        owner.release(&injector).await.unwrap();
        assert_eq!(releases.load(SeqCst), 1);
    }
    #[tokio::test]
    async fn event_loop_old_a_timeout_after_b_takeover_preserves_b_held_input() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        let (mut a, addr_a) = fixture_peer(31).await;
        let (mut b, _) = fixture_peer(32).await;
        let addr_b = "127.0.0.1:39982".parse().unwrap();
        for peer in [&mut a, &mut b] {
            let (sub, _) = mock_subscription(CaptureSource::MainDisplay);
            sub.settings.send_modify(|s| s.paused = false);
            peer.subscriptions.insert(100, sub);
        }
        let SessionCommand::InputBinding {
            binding: binding_a, ..
        } = a.input_binding(31, 100, 10, 1).unwrap()
        else {
            panic!()
        };
        let SessionCommand::InputBinding {
            binding: binding_b, ..
        } = b.input_binding(32, 100, 10, 1).unwrap()
        else {
            panic!()
        };
        let held = Arc::new(AtomicBool::new(false));
        let releases = Arc::new(AtomicUsize::new(0));
        let injector = LazyInputInjector(std::sync::Mutex::new(Some((
            InputTarget::Desktop,
            Box::new(OwnershipProbe {
                held: held.clone(),
                releases: releases.clone(),
                delay: Duration::ZERO,
            }),
        ))));
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (udp, _) = mux.split();
        let mut owner = ActiveInput::default();
        for (peer, addr, binding) in [(&mut a, addr_a, binding_a), (&mut b, addr_b, binding_b)] {
            dispatch_realtime_input(
                peer,
                &mut owner,
                &injector,
                &udp,
                addr,
                RealtimeInput {
                    binding,
                    sequence: 1,
                    created_host_ms: now_unix_ms(),
                    payload: InputPayload::Event(InputEvent::MouseDown(0)),
                },
            )
            .await;
        }
        assert_eq!(releases.load(SeqCst), 1);
        assert_eq!(owner.binding, Some(binding_b));
        // These are the same dispatch and timer bodies called by the production
        // event loop. Refresh only B while A's original held lease times out.
        tokio::time::sleep(Duration::from_millis(400)).await;
        dispatch_realtime_input(
            &mut b,
            &mut owner,
            &injector,
            &udp,
            addr_b,
            RealtimeInput {
                binding: binding_b,
                sequence: 2,
                created_host_ms: now_unix_ms(),
                payload: InputPayload::Event(InputEvent::MouseMove { dx: 1, dy: 0 }),
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let mut peers = HashMap::from([(addr_a, a), (addr_b, b)]);
        expire_input_owners(&mut peers, &mut owner, &injector).await;
        assert!(!peers[&addr_a].input_leases.contains_key(&100));
        assert!(peers[&addr_b].input_leases.contains_key(&100));
        assert!(held.load(SeqCst));
        assert_eq!(releases.load(SeqCst), 1);
        assert_eq!(owner.owner, Some((addr_b, 100)));
        assert_eq!(owner.binding, Some(binding_b));
    }

    struct HeldButtonProbe(Arc<std::sync::atomic::AtomicUsize>);
    impl InputInjector for HeldButtonProbe {
        fn inject_input(&self,event:InputEvent)->Result<(),Box<dyn Error+Send+Sync>> {
            if matches!(event,InputEvent::MouseDown(0)) {self.0.store(1, std::sync::atomic::Ordering::SeqCst);}
            Ok(())
        }
        fn release_all_input(&self) {self.0.store(0,std::sync::atomic::Ordering::SeqCst);}
    }
    #[tokio::test]
    async fn duplicate_or_stale_capture_controls_cannot_release_the_new_target() {
        let (mut peer,addr)=fixture_peer(31).await;
        let (sub,_)=mock_subscription(CaptureSource::MainDisplay);
        sub.settings.send_modify(|s|s.paused=false);
        let request=sub.request.clone();peer.subscriptions.insert(100,sub);
        let SessionCommand::InputBinding{binding,..}=peer.input_binding(31,100,10,1).unwrap() else{panic!()};
        let held=Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let injector=LazyInputInjector(std::sync::Mutex::new(Some((InputTarget::Desktop,Box::new(HeldButtonProbe(held.clone()))))));
        let mux=UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();let (udp,_)=mux.split();let mut owner=ActiveInput::default();
        assert!(inject_fresh(&injector,&mut owner,&mut peer,&udp,addr,100,InputEvent::MouseDown(0),Some(std::time::Instant::now()+Duration::from_millis(250))).await);
        owner.binding=Some(binding);
        for command in [
            SessionCommand::Subscribe(request),
            SessionCommand::SwitchSource{id:100,request_id:10,source:CaptureSource::MainDisplay},
            SessionCommand::SwitchSource{id:100,request_id:9,source:CaptureSource::Window(7)},
            SessionCommand::SetActivity{id:100,revision:6,video:false,audio:false},
            SessionCommand::SetActivity{id:100,revision:7,video:true,audio:false},
            SessionCommand::SetActivity{id:100,revision:8,video:true,audio:false},
        ] {
            // The actual pre-dispatch cleanup function, with a fake native sink.
            retire_input_before_control(&mut peer,&mut owner,&injector,addr,&command).await;
            assert_eq!(held.load(std::sync::atomic::Ordering::SeqCst),1);
            assert_eq!(owner.binding,Some(binding));
            assert_eq!(peer.input_leases.get(&100).unwrap().lease.binding,binding);
        }
        retire_input_before_control(&mut peer,&mut owner,&injector,addr,&SessionCommand::SwitchSource{id:100,request_id:11,source:CaptureSource::Window(7)}).await;
        assert_eq!(held.load(std::sync::atomic::Ordering::SeqCst),0);
        assert!(owner.binding.is_none() && !peer.input_leases.contains_key(&100));
    }
    #[tokio::test]
    async fn versioned_capture_cannot_be_replaced_by_initial_or_legacy_controls() {
        let (mut peer,_)=fixture_peer(31).await;
        assert!(peer.accepts_legacy_control());
        let (sub,_)=mock_subscription(CaptureSource::MainDisplay);
        sub.settings.send_modify(|s|s.paused=false);
        let mut request=sub.request.clone();peer.subscriptions.insert(100,sub);
        peer.input_binding(31,100,10,1).unwrap();
        assert!(!peer.accepts_legacy_control());
        assert!(peer.accepts_initial_subscribe(&request));
        request.source=CaptureSource::Window(9);
        assert!(!peer.accepts_initial_subscribe(&request));
        request.source=CaptureSource::MainDisplay;request.audio=!request.audio;
        assert!(!peer.accepts_initial_subscribe(&request));
    }

    #[tokio::test]
    async fn stale_event_is_rejected_before_lazy_native_injector_creation() {
        let (mut peer,addr)=fixture_peer(31).await;
        let (sub,_)=mock_subscription(CaptureSource::MainDisplay);
        sub.settings.send_modify(|s|s.paused=false);
        peer.subscriptions.insert(100,sub);
        let mux=UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (udp,_)=mux.split();let injector=LazyInputInjector::default();let mut owner=ActiveInput::default();
        assert!(!inject_fresh(&injector,&mut owner,&mut peer,&udp,addr,100,InputEvent::MouseDown(0),Some(std::time::Instant::now()-Duration::from_millis(1))).await);
        assert!(injector.0.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn realtime_ingress_consumes_before_native_execution_and_rejects_old_owner() {
        let (mut peer,_)=fixture_peer(31).await;
        let (sub,_)=mock_subscription(CaptureSource::MainDisplay);
        sub.settings.send_modify(|s|s.paused=false);
        peer.subscriptions.insert(100,sub);
        let SessionCommand::InputBinding{binding,..}=peer.input_binding(31,100,10,1).unwrap() else{panic!()};
        let packet=RealtimeInput{binding,sequence:1,created_host_ms:now_unix_ms(),payload:InputPayload::Event(InputEvent::MouseDown(0))};
        // This is the production ingress method, before OS APIs or await points.
        // A fake native call with unknown outcome cannot undo its consumption.
        let events=peer.accept_input(&packet).unwrap();
        assert_eq!(events,vec![InputEvent::MouseDown(0)]);
        let mut calls=0;
        for _ in events {calls+=1;let _unknown:Result<(),&str>=Err("native reply lost");}
        assert!(peer.accept_input(&packet).is_none());assert_eq!(calls,1);
        let mut stale=packet.clone();stale.sequence=2;stale.created_host_ms=now_unix_ms()-251;
        assert!(peer.accept_input(&stale).is_none());
        let mut old_release=packet.clone();old_release.sequence=3;old_release.payload=InputPayload::Release;
        // Same tuple/source revision, new lease: delayed release remains old.
        peer.input_leases.remove(&100);
        let SessionCommand::InputBinding{binding:new,..}=peer.input_binding(31,100,10,2).unwrap() else{panic!()};
        assert_ne!(binding.epoch,new.epoch);
        let mut new_down=packet.clone();new_down.binding=new;new_down.created_host_ms=now_unix_ms();
        assert_eq!(peer.accept_input(&new_down).unwrap(),vec![InputEvent::MouseDown(0)]);
        assert!(peer.accept_input(&old_release).is_none());
        peer.subscriptions.get_mut(&100).unwrap().source_revision=11;
        assert!(peer.accept_input(&new_down).is_none());
    }

    #[tokio::test]
    async fn realtime_binding_requires_exact_live_target_and_connection() {
        let (mut peer,_)=fixture_peer(31).await;
        let (sub,_)=mock_subscription(CaptureSource::MainDisplay);
        sub.settings.send_modify(|s|s.paused=false);
        peer.subscriptions.insert(100,sub);
        assert!(peer.input_binding(32,100,10,1).is_none());
        assert!(peer.input_binding(31,100,9,1).is_none());
        let SessionCommand::InputBinding{binding,..}=peer.input_binding(31,100,10,1).unwrap() else{panic!()};
        let packet=RealtimeInput{binding,sequence:1,created_host_ms:now_unix_ms(),payload:InputPayload::Event(InputEvent::MouseDown(0))};
        // Actual target identity changed without a revision acknowledgement.
        peer.subscriptions.get_mut(&100).unwrap().input_target=Some(InputTarget::Display{id:77});
        assert!(peer.accept_input(&packet).is_none());
        peer.subscriptions.get_mut(&100).unwrap().settings.send_modify(|s|s.paused=true);
        assert!(peer.input_binding(31,100,10,2).is_none());
    }

    async fn fixture_peer(id: u32) -> (Connection, SocketAddr) {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = mux.split();
        let addr = "127.0.0.1:39981".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        (Connection::new(id, addr, sender, &config), addr)
    }
    struct NativeStopFixture(Arc<crate::lifecycle::Resources>);
    impl Drop for NativeStopFixture {
        fn drop(&mut self) {
            use std::sync::atomic::Ordering::SeqCst;
            self.0.native_stops_completed.fetch_add(1, SeqCst);
            self.0.native_streams.fetch_sub(1, SeqCst);
        }
    }
    async fn install_capture_fixture(peer: &mut Connection) {
        use std::sync::atomic::Ordering::SeqCst;
        let (mut sub, _) = mock_subscription(CaptureSource::MainDisplay);
        sub.task.abort();
        let _ = (&mut sub.task).await;
        let resources = peer.resources.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        sub.task = tokio::spawn(async move {
            let _worker = crate::lifecycle::WorkerGuard::new(resources.clone());
            resources.native_streams.fetch_add(1, SeqCst);
            let _stream = NativeStopFixture(resources);
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        peer.subscriptions.insert(100, sub);
    }
    struct WithheldStopFixture {
        resources: Arc<crate::lifecycle::Resources>,
        release: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
        owner_dropped: Arc<AtomicBool>,
    }
    struct NativeOwnerFixture(Arc<AtomicBool>);
    impl Drop for NativeOwnerFixture {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl Drop for WithheldStopFixture {
        fn drop(&mut self) {
            let (_, completion) =
                crate::lifecycle::NativeStopCompletion::new(Some(self.resources.clone()));
            let release = self.release.take().unwrap();
            let owner = NativeOwnerFixture(self.owner_dropped.clone());
            // Model the native callback context, with no real capture or consent.
            tokio::spawn(async move {
                let result = match release.await {
                    Ok(result) => result,
                    Err(_) => std::future::pending().await,
                };
                if result.is_ok() {
                    drop(owner);
                } else {
                    std::mem::forget(owner);
                }
                completion.complete(result);
            });
        }
    }
    async fn install_withheld_stop_fixture(
        peer: &mut Connection,
    ) -> (
        tokio::sync::oneshot::Sender<Result<(), String>>,
        Arc<AtomicBool>,
    ) {
        use std::sync::atomic::Ordering::SeqCst;
        let (mut sub, _) = mock_subscription(CaptureSource::MainDisplay);
        sub.task.abort();
        let _ = (&mut sub.task).await;
        let resources = peer.resources.clone();
        let owner_dropped = Arc::new(AtomicBool::new(false));
        let (release, released) = tokio::sync::oneshot::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let fixture = WithheldStopFixture {
            resources: resources.clone(),
            release: Some(released),
            owner_dropped: owner_dropped.clone(),
        };
        sub.task = tokio::spawn(async move {
            let _worker = crate::lifecycle::WorkerGuard::new(resources.clone());
            resources.native_streams.fetch_add(1, SeqCst);
            let _native = fixture;
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        peer.subscriptions.insert(100, sub);
        (release, owner_dropped)
    }
    #[tokio::test]
    async fn key_rotation_same_address_hello_keeps_pending_native_budget_until_release() {
        use std::sync::atomic::Ordering::SeqCst;
        // All secrets/owners are synthetic; no profile, capture or device key changes.
        let old_key = [0x41; 32];
        let new_key = [0x42; 32];
        let nonce = [0x51; 16];
        let (mut peer, addr) = fixture_peer(90020).await;
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = mux.split();
        let (release, dropped) = install_withheld_stop_fixture(&mut peer).await;
        let resources = peer.resources.clone();
        let result = peer.shutdown().await;
        assert!(result.is_err());
        assert_eq!(resources.workers.load(SeqCst), 0);
        assert_eq!(resources.native_stops_pending.load(SeqCst), 1);
        assert!(!dropped.load(SeqCst));
        let mut closed = HashMap::from([(
            addr,
            ClosedReceipt {
                resources: resources.clone(),
                id: 90020,
                result,
                at: Instant::now(),
            },
        )]);
        let mut peers = HashMap::new();
        for index in 0..7 {
            let (other, _) = fixture_peer(90021 + index).await;
            peers.insert(
                SocketAddr::from(([127, 0, 0, 1], 40000 + index as u16)),
                other,
            );
        }
        let closing = HashMap::new();
        let mut authenticated = HashMap::from([(addr, (nonce, [0x61; 16], now_unix_ms()))]);
        let timestamp = now_unix_ms();
        let replay = accept_session_hello(
            Some(&old_key),
            (
                nonce,
                timestamp,
                mac_session_hello(&old_key, &nonce, timestamp),
            ),
            addr,
            &sender,
            &peers,
            &closing,
            &mut closed,
            &mut authenticated,
        );
        assert_eq!(
            replay.unwrap(),
            (authenticated[&addr].1, authenticated[&addr].2)
        );
        let changed_nonce = [0x52; 16];
        assert!(
            accept_session_hello(
                Some(&old_key),
                (
                    changed_nonce,
                    timestamp,
                    mac_session_hello(&old_key, &changed_nonce, timestamp)
                ),
                addr,
                &sender,
                &peers,
                &closing,
                &mut closed,
                &mut authenticated
            )
            .is_err()
        );
        // Mirror the key-rotation tick: remove old crypto/auth, retain live receipts.
        sender.remove_peer_crypto(addr);
        authenticated.clear();
        closed.retain(|_, receipt| receipt.resources_live());
        assert_eq!(pending_closed_count(&closed), 1);
        // A valid MAC under the revoked key cannot authenticate with the new key.
        assert!(
            accept_session_hello(
                Some(&new_key),
                (
                    nonce,
                    timestamp,
                    mac_session_hello(&old_key, &nonce, timestamp)
                ),
                addr,
                &sender,
                &peers,
                &closing,
                &mut closed,
                &mut authenticated
            )
            .is_err()
        );
        assert!(authenticated.is_empty());
        let stale = timestamp.saturating_sub(61_000);
        assert!(
            accept_session_hello(
                Some(&new_key),
                (nonce, stale, mac_session_hello(&new_key, &nonce, stale)),
                addr,
                &sender,
                &peers,
                &closing,
                &mut closed,
                &mut authenticated
            )
            .is_err()
        );
        let hello = (
            nonce,
            timestamp,
            mac_session_hello(&new_key, &nonce, timestamp),
        );
        let admission = accept_session_hello(
            Some(&new_key),
            hello,
            addr,
            &sender,
            &peers,
            &closing,
            &mut closed,
            &mut authenticated,
        );
        assert!(
            admission.is_err(),
            "valid same-address Hello after key rotation must not erase live native ownership: {admission:?}"
        );
        assert!(authenticated.is_empty());
        assert!(Arc::ptr_eq(&closed[&addr].resources, &resources));
        assert_eq!(pending_closed_count(&closed), 1);
        assert_eq!(
            connection_admission_error(addr, &peers, &closing, &closed),
            Some("previous native teardown remains unknown")
        );
        let other_addr = SocketAddr::from(([127, 0, 0, 1], 41000));
        assert_eq!(
            connection_admission_error(other_addr, &peers, &closing, &closed),
            Some("connection limit reached")
        );
        // Only the delayed successful completion destroys the owner and frees the slot.
        release.send(Ok(())).unwrap();
        tokio::time::timeout(
            Duration::from_millis(500),
            crate::lifecycle::wait_native_stops(&resources),
        )
        .await
        .unwrap();
        assert!(dropped.load(SeqCst));
        assert_eq!(resources.native_streams.load(SeqCst), 0);
        assert_eq!(pending_closed_count(&closed), 0);
        assert!(
            accept_session_hello(
                Some(&new_key),
                hello,
                addr,
                &sender,
                &peers,
                &closing,
                &mut closed,
                &mut authenticated
            )
            .is_ok()
        );
        assert!(!closed.contains_key(&addr));
        assert_eq!(
            connection_admission_error(addr, &peers, &closing, &closed),
            None
        );
        assert_eq!(
            connection_admission_error(other_addr, &peers, &closing, &closed),
            None
        );
        let (replacement, _) = fixture_peer(90028).await;
        peers.insert(addr, replacement);
        assert_eq!(peers.len(), 8);
        assert_eq!(
            connection_admission_error(other_addr, &peers, &closing, &closed),
            Some("connection limit reached")
        );
        for (_, peer) in peers {
            peer.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn cleared_auth_cannot_replace_closing_or_failed_native_owner() {
        use std::sync::atomic::Ordering::SeqCst;
        let (mut peer, addr) = fixture_peer(90029).await;
        let (release, dropped) = install_withheld_stop_fixture(&mut peer).await;
        let resources = peer.resources.clone();
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = mux.split();
        let key = [0x71; 32];
        let nonce = [0x72; 16];
        let ts = now_unix_ms();
        let hello = (nonce, ts, mac_session_hello(&key, &nonce, ts));
        let mut peers = HashMap::from([(addr, peer)]);
        let mut closing = HashMap::new();
        let mut closed = HashMap::new();
        // Key rotation removes authentication before the close worker finishes.
        let mut authenticated = HashMap::new();
        begin_close(&mut peers, &mut closing, addr, false);
        assert!(
            accept_session_hello(
                Some(&key),
                hello,
                addr,
                &sender,
                &peers,
                &closing,
                &mut closed,
                &mut authenticated
            )
            .is_err()
        );
        assert!(authenticated.is_empty());
        assert!(Arc::ptr_eq(&closing[&addr].resources, &resources));
        let mut ending = closing.remove(&addr).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), &mut ending.task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert_eq!(resources.native_stops_pending.load(SeqCst), 1);
        closed.insert(
            addr,
            ClosedReceipt {
                resources: resources.clone(),
                id: ending.id,
                result,
                at: Instant::now(),
            },
        );
        release
            .send(Err("native owner release failed".into()))
            .unwrap();
        tokio::time::timeout(
            Duration::from_millis(500),
            crate::lifecycle::wait_native_stops(&resources),
        )
        .await
        .unwrap();
        assert_eq!(resources.native_stops_pending.load(SeqCst), 0);
        assert_eq!(resources.native_stop_errors.load(SeqCst), 1);
        assert_eq!(resources.native_streams.load(SeqCst), 1);
        assert!(!dropped.load(SeqCst));
        // Pending reaching zero after failure does not mean the native owner is gone.
        for _ in 0..2 {
            authenticated.clear();
            closed.retain(|_, receipt| receipt.resources_live());
            assert!(
                accept_session_hello(
                    Some(&key),
                    hello,
                    addr,
                    &sender,
                    &peers,
                    &closing,
                    &mut closed,
                    &mut authenticated
                )
                .is_err()
            );
            assert!(authenticated.is_empty());
            assert_eq!(pending_closed_count(&closed), 1);
            assert_eq!(
                connection_admission_error(addr, &peers, &closing, &closed),
                Some("previous native teardown remains unknown")
            );
        }
    }

    #[tokio::test]
    async fn cleared_auth_keeps_worker_occupancy_until_worker_releases() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = mux.split();
        let addr = SocketAddr::from(([127, 0, 0, 1], 42000));
        let resources = Arc::new(crate::lifecycle::Resources::default());
        let worker = crate::lifecycle::WorkerGuard::new(resources.clone());
        let mut closed = HashMap::from([(
            addr,
            ClosedReceipt {
                resources,
                id: 90030,
                result: Err("cleanup worker remains unknown".into()),
                at: Instant::now(),
            },
        )]);
        let peers = HashMap::new();
        let closing = HashMap::new();
        let mut authenticated = HashMap::new();
        let key = [0x73; 32];
        let nonce = [0x74; 16];
        let ts = now_unix_ms();
        let hello = (nonce, ts, mac_session_hello(&key, &nonce, ts));
        assert!(
            accept_session_hello(
                Some(&key),
                hello,
                addr,
                &sender,
                &peers,
                &closing,
                &mut closed,
                &mut authenticated
            )
            .is_err()
        );
        assert!(authenticated.is_empty());
        assert_eq!(pending_closed_count(&closed), 1);
        drop(worker);
        assert_eq!(pending_closed_count(&closed), 0);
        assert!(
            accept_session_hello(
                Some(&key),
                hello,
                addr,
                &sender,
                &peers,
                &closing,
                &mut closed,
                &mut authenticated
            )
            .is_ok()
        );
        assert!(!closed.contains_key(&addr));
    }

    #[tokio::test]
    async fn duplicate_unsubscribe_replays_native_stop_failure() {
        let (mut peer, _) = fixture_peer(90010).await;
        let (release, dropped) = install_withheld_stop_fixture(&mut peer).await;
        release
            .send(Err("withheld fixture native stop failed".into()))
            .unwrap();
        let first = peer.unsubscribe(100).await;
        assert!(first.is_err());
        assert_eq!(peer.unsubscribe(100).await, first);
        assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            peer.resources
                .native_stop_errors
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(peer.shutdown().await.is_err());
    }
    #[tokio::test]
    async fn duplicate_unsubscribe_keeps_unknown_audio_cleanup_result_after_completion() {
        let (mut peer, _) = fixture_peer(90011).await;
        install_capture_fixture(&mut peer).await;
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (started, ready) = tokio::sync::oneshot::channel();
        peer.retiring_tasks.push(tokio::spawn(async move {
            let _ = started.send(());
            let _ = released.await;
        }));
        ready.await.unwrap();
        let first = peer.unsubscribe(100).await;
        assert!(matches!(&first, Err(error) if error.contains("still pending")));
        assert!(!peer.retiring_tasks[0].is_finished());
        assert_eq!(peer.unsubscribe(100).await, first);
        release.send(()).unwrap();
        tokio::task::yield_now().await;
        assert_eq!(peer.unsubscribe(100).await, first);
        peer.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn withheld_native_completion_cannot_deadlock_runtime_or_acknowledge_cleanup() {
        use std::sync::atomic::Ordering::SeqCst;
        let (mut peer, _) = fixture_peer(90012).await;
        let (release, dropped) = install_withheld_stop_fixture(&mut peer).await;
        let resources = peer.resources.clone();
        let first = peer.unsubscribe(100).await;
        assert!(matches!(&first, Err(error) if error.contains("unknown")));
        assert!(!dropped.load(SeqCst));
        assert_eq!(resources.native_streams.load(SeqCst), 1);
        assert_eq!(resources.native_stops_pending.load(SeqCst), 1);
        assert_eq!(resources.native_stops_completed.load(SeqCst), 0);
        let (other, _) = fixture_peer(90013).await;
        tokio::time::timeout(Duration::from_millis(500), other.shutdown())
            .await
            .unwrap()
            .unwrap();
        let result = peer.shutdown().await;
        assert!(result.is_err());
        assert_eq!(
            crate::lifecycle::inspect_connection(90012)[0].state,
            "teardown_unknown"
        );
        assert!(!matches!(
            terminal_reply(90012, &result),
            SessionCommand::Closed { .. }
        ));
        assert!(!dropped.load(SeqCst));
        let receipt = ClosedReceipt {
            id: 90012,
            resources: resources.clone(),
            result: result.clone(),
            at: Instant::now(),
        };
        assert!(receipt.resources_live());
        assert_eq!(
            pending_closed_count(&HashMap::from([(
                "127.0.0.1:39981".parse().unwrap(),
                receipt
            )])),
            1
        );
        release.send(Ok(())).unwrap();
        tokio::time::timeout(
            Duration::from_millis(500),
            crate::lifecycle::wait_native_stops(&resources),
        )
        .await
        .unwrap();
        assert!(dropped.load(SeqCst));
        assert_eq!(resources.native_streams.load(SeqCst), 0);
        assert_eq!(resources.native_stops_completed.load(SeqCst), 1);
        assert_eq!(
            crate::lifecycle::inspect_connection(90012)[0].state,
            "teardown_unknown"
        );
        // A previously returned unknown connection result never becomes Closed.
        assert!(matches!(
            terminal_reply(90012, &result),
            SessionCommand::Error { .. }
        ));
    }
    #[tokio::test]
    async fn disconnect_with_withheld_stop_returns_bounded_error_and_keeps_native_owner() {
        use std::sync::atomic::Ordering::SeqCst;
        let (mut peer, addr) = fixture_peer(90014).await;
        let (release, dropped) = install_withheld_stop_fixture(&mut peer).await;
        let resources = peer.resources.clone();
        let mut peers = HashMap::from([(addr, peer)]);
        let mut closing = HashMap::new();
        begin_close(&mut peers, &mut closing, addr, false);
        let mut ending = closing.remove(&addr).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), &mut ending.task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert!(!ending.notify);
        assert_eq!(resources.native_stops_pending.load(SeqCst), 1);
        assert!(!dropped.load(SeqCst));
        // Simulated IPC on this same single runtime thread remains responsive.
        assert_eq!(
            crate::lifecycle::inspect_connection(90014)[0].native_streams,
            1
        );
        tokio::time::timeout(
            Duration::from_millis(100),
            tokio::time::sleep(Duration::from_millis(5)),
        )
        .await
        .unwrap();
        release.send(Ok(())).unwrap();
        tokio::time::timeout(
            Duration::from_millis(500),
            crate::lifecycle::wait_native_stops(&resources),
        )
        .await
        .unwrap();
        assert!(dropped.load(SeqCst));
    }
    #[tokio::test]
    async fn duplicate_unsubscribe_keeps_capture_join_panic() {
        let (mut peer, _) = fixture_peer(90015).await;
        let (mut sub, _) = mock_subscription(CaptureSource::MainDisplay);
        sub.task.abort();
        let _ = (&mut sub.task).await;
        sub.task = tokio::spawn(async { panic!("capture fixture failure") });
        tokio::task::yield_now().await;
        peer.subscriptions.insert(100, sub);
        let first = peer.unsubscribe(100).await;
        assert!(first.is_err());
        assert_eq!(peer.unsubscribe(100).await, first);
        assert!(peer.shutdown().await.is_err());
    }
    #[tokio::test]
    async fn pruning_finished_capture_keeps_join_failure_for_close() {
        let (mut peer, _) = fixture_peer(90016).await;
        let (mut sub, _) = mock_subscription(CaptureSource::MainDisplay);
        sub.task.abort();
        let _ = (&mut sub.task).await;
        sub.task = tokio::spawn(async { panic!("pruned capture fixture failure") });
        tokio::task::yield_now().await;
        peer.subscriptions.insert(100, sub);
        peer.prune_finished_subscriptions(false);
        assert!(peer.unsubscribe(100).await.is_err());
        let result = peer.shutdown().await;
        assert!(matches!(
            terminal_reply(90016, &result),
            SessionCommand::Error { .. }
        ));
    }
    #[tokio::test]
    async fn close_receipt_waits_for_native_drop_and_deduplicates_owned_id() {
        let (mut peer, addr) = fixture_peer(90001).await;
        install_capture_fixture(&mut peer).await;
        let resources = peer.resources.clone();
        let mut peers = HashMap::from([(addr, peer)]);
        let mut closing = HashMap::new();
        let mut closed = HashMap::new();
        assert!(matches!(
            request_close(&mut peers, &mut closing, &closed, addr, 999),
            Some(SessionCommand::Error {
                request_id: 999,
                ..
            })
        ));
        assert!(peers.contains_key(&addr));
        assert!(closing.is_empty());
        assert!(request_close(&mut peers, &mut closing, &closed, addr, 90001).is_none());
        assert!(request_close(&mut peers, &mut closing, &closed, addr, 90001).is_none());
        assert_eq!(closing.len(), 1);
        assert_eq!(
            crate::lifecycle::inspect_connection(90001)[0].state,
            "closing"
        );
        let mut ending = closing.remove(&addr).unwrap();
        let result = (&mut ending.task).await.unwrap();
        assert!(result.is_ok());
        assert_eq!(
            resources.workers.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        let snapshot = crate::lifecycle::inspect_connection(90001).remove(0);
        assert_eq!(snapshot.state, "closed");
        assert_eq!(snapshot.native_streams, 0);
        assert_eq!(snapshot.native_stops_completed, 1);
        closed.insert(
            addr,
            ClosedReceipt {
                resources: resources.clone(),
                id: 90001,
                result,
                at: Instant::now(),
            },
        );
        assert!(matches!(
            request_close(&mut peers, &mut closing, &closed, addr, 90001),
            Some(SessionCommand::Closed {
                connection_id: 90001,
                ..
            })
        ));
        let (new_peer, _) = fixture_peer(90002).await;
        peers.insert(addr, new_peer);
        assert!(matches!(
            request_close(&mut peers, &mut closing, &closed, addr, 90001),
            Some(SessionCommand::Error { .. })
        ));
        assert_eq!(peers[&addr].id, 90002);
        peers.remove(&addr).unwrap().shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn unsubscribe_is_joined_and_duplicate_safe_unknown_id_does_not_retire_live_capture() {
        let (mut peer, _) = fixture_peer(90003).await;
        install_capture_fixture(&mut peer).await;
        assert!(peer.unsubscribe(101).await.is_err());
        assert!(peer.subscriptions.contains_key(&100));
        peer.unsubscribe(100).await.unwrap();
        peer.unsubscribe(100).await.unwrap();
        assert_eq!(
            peer.resources
                .workers
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            peer.resources
                .native_streams
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        peer.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn unknown_capture_source_preserves_live_subscription_without_native_acquisition() {
        struct CatalogGuard;
        impl Drop for CatalogGuard {
            fn drop(&mut self) {
                TEST_CAPTURE_CATALOG.with(|value| *value.borrow_mut() = None);
            }
        }
        TEST_CAPTURE_CATALOG.with(|value| *value.borrow_mut() = Some(Vec::new()));
        let _catalog = CatalogGuard;
        let (mut peer, addr) = fixture_peer(90006).await;
        install_capture_fixture(&mut peer).await;
        let request = SubscriptionRequest {
            id: 100,
            source: CaptureSource::Window(u32::MAX),
            width: 640,
            height: 360,
            fps: 15,
            bitrate_kbps: 1000,
            audio: true,
        };
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = mux.split();
        let result = peer
            .subscribe_with_preflight(request, addr, sender, Statistics::new(), false, || Ok(()))
            .await;
        assert!(
            matches!(result,Err(ref reason) if reason=="selected capture source is unavailable")
        );
        assert_eq!(
            peer.subscriptions[&100].request.source,
            CaptureSource::MainDisplay
        );
        assert_eq!(
            peer.resources
                .workers
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            peer.resources
                .native_streams
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            peer.resources
                .native_stops_completed
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(peer.audio_groups.is_empty());
        assert!(!peer.retired.contains_key(&100));
        peer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn native_stop_error_never_produces_closed() {
        let (peer, _) = fixture_peer(90004).await;
        peer.resources
            .native_stop_errors
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let result = peer.shutdown().await;
        assert!(result.is_err());
        assert!(matches!(
            terminal_reply(90004, &result),
            SessionCommand::Error { .. }
        ));
        assert_eq!(
            crate::lifecycle::inspect_connection(90004)[0].state,
            "teardown_failed"
        );
    }
    #[tokio::test]
    async fn disconnect_and_cancel_close_release_fixture_without_false_receipt() {
        let (mut peer, addr) = fixture_peer(90005).await;
        install_capture_fixture(&mut peer).await;
        let resources = peer.resources.clone();
        let mut peers = HashMap::from([(addr, peer)]);
        let mut closing = HashMap::new();
        begin_close(&mut peers, &mut closing, addr, false);
        assert!(!closing[&addr].notify);
        let ending = closing.remove(&addr).unwrap();
        drop(ending);
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            resources.workers.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            resources
                .native_streams
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_ne!(
            crate::lifecycle::inspect_connection(90005)[0].state,
            "closed"
        );
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_capture_reservation_is_exclusive_and_released() {
        WINDOWS_VIDEO_CAPTURE_RESERVED.store(false, Relaxed);
        let first = WindowsVideoCaptureReservation::acquire().unwrap();
        assert!(WindowsVideoCaptureReservation::acquire().is_err());
        drop(first);
        let second = WindowsVideoCaptureReservation::acquire().unwrap();
        drop(second);
        assert!(!WINDOWS_VIDEO_CAPTURE_RESERVED.load(Relaxed));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_safe_capture_budget_rejects_resource_spikes() {
        assert!(validate_host_video_settings(2560, 1440, 60, 20_000).is_ok());
        assert!(validate_host_video_settings(3840, 2160, 30, 12_000).is_err());
        assert!(validate_host_video_settings(1920, 1080, 120, 12_000).is_err());
        assert!(validate_host_video_settings(1920, 1080, 60, 40_000).is_err());
    }

    #[tokio::test]
    async fn repeated_preflight_failure_never_acquires_audio_or_capture() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _receiver) = mux.split();
        let addr = "127.0.0.1:39571".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        for id in 1..=16 {
            let request = SubscriptionRequest {
                id,
                source: CaptureSource::MainDisplay,
                width: 1280,
                height: 720,
                fps: 30,
                bitrate_kbps: 4000,
                audio: true,
            };
            let result = peer
                .subscribe_with_preflight(
                    request,
                    addr,
                    sender.clone(),
                    config.stats.clone(),
                    true,
                    || Err("missing capture dependency".into()),
                )
                .await;
            assert!(matches!(result, Err(ref error) if error == "missing capture dependency"));
            assert!(peer.audio_groups.is_empty());
            assert!(peer.subscriptions.is_empty());
            assert!(!peer.can_input(id));
            assert_eq!(peer.id, 1);
        }
    }

    struct StopProbe(Arc<AtomicBool>);
    impl Drop for StopProbe {
        fn drop(&mut self) {
            self.0.store(true, Relaxed);
        }
    }

    fn mock_subscription(source: CaptureSource) -> (Subscription, Arc<AtomicBool>) {
        let request = SubscriptionRequest {
            id: 100,
            source,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 4000,
            audio: false,
        };
        let mut initial = stream_settings(&request);
        initial.paused = true;
        let (settings, _) = watch::channel(initial);
        let (cancel, _) = broadcast::channel(4);
        let stopped = Arc::new(AtomicBool::new(false));
        let probe = StopProbe(stopped.clone());
        let task = tokio::spawn(async move {
            let _probe = probe;
            std::future::pending::<()>().await;
        });
        (
            Subscription {
                request,
                settings,
                revision: 7,
                settings_revision: 11,
                keyframe: Arc::new(AtomicBool::new(false)),
                cancel,
                task,
                task_joined: false,
                audio_group: None,
                audio_active: false,
                supports_input: true,
                input_target: Some(InputTarget::Desktop),
                #[cfg(target_os = "macos")]
                input_probe: |_| Ok(()),
                last_input_error: None,
                source_revision: 10,
                video_sequence: Arc::new(std::sync::atomic::AtomicU16::new(37)),
                #[cfg(target_os = "windows")]
                _capture_reservation: Arc::new(WindowsVideoCaptureReservation::acquire().unwrap()),
                #[cfg(all(unix, any(test, target_os = "linux")))]
                native_capture: None,
            },
            stopped,
        )
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn geometry_capability_never_substitutes_for_actual_input_permission() {
        let (mut subscription, _) = mock_subscription(CaptureSource::Display(77));
        subscription.input_target = Some(InputTarget::Display { id: 77 });
        assert!(subscription.supports_input_now());
        subscription.input_probe = |_| Err("running host input-posting permission denied".into());
        assert!(subscription.supports_input);
        assert!(!subscription.supports_input_now());
        assert!(
            subscription
                .input_status()
                .unwrap_err()
                .contains("permission denied")
        );
        // Rechecking real readiness permits recovery, without changing the
        // catalog capability, source revision, or viewer control lock.
        subscription.input_probe = |_| Ok(());
        assert!(subscription.supports_input_now());
        assert_eq!(subscription.source_revision, 10);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn finished_viewer_intent_remains_a_bounded_local_picker_target_without_input_or_audio() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = mux.split();
        let config = HostServiceConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(31, "127.0.0.1:40123".parse().unwrap(), sender, &config);
        let (subscription, _) = mock_subscription(CaptureSource::MainDisplay);
        subscription.task.abort();
        peer.subscriptions.insert(100, subscription);
        tokio::task::yield_now().await;
        assert!(peer.subscriptions[&100].task.is_finished());
        peer.prune_finished_subscriptions(true);
        let retained = &peer.subscriptions[&100];
        assert_eq!(retained.source_revision, 10);
        assert!(!retained.supports_input);
        assert!(retained.input_target.is_none());
        assert!(retained.audio_group.is_none());
        assert!(!peer.can_input(100));
        peer.prune_finished_subscriptions(false);
        assert!(peer.subscriptions.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn two_portal_windows_remain_bound_to_independent_subscriptions() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = mux.split();
        let config = HostServiceConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let addr = "127.0.0.1:40123".parse().unwrap();
        let mut peer = Connection::new(31, addr, sender.clone(), &config);
        let (a, _) = mock_subscription(CaptureSource::MainDisplay);
        let (mut b, _) = mock_subscription(CaptureSource::MainDisplay);
        b.request.id = 101;
        peer.subscriptions.insert(100, a);
        peer.subscriptions.insert(101, b);
        let owner = PeerOwner::from_authenticated_connection(31, [1; 16], Some([2; 32])).unwrap();
        let target = |id| LocalPortalShareTarget {
            addr,
            owner,
            source_revision: 10,
            subscription_id: id,
        };
        let (_fa, a) = crate::linux_portal::runtime::tests::prepared_fixture().await;
        let (_fb, b) = crate::linux_portal::runtime::tests::prepared_fixture().await;
        peer.commit_native_capture(target(100), owner, a.clone())
            .unwrap();
        peer.commit_native_capture(target(101), owner, b.clone())
            .unwrap();
        assert_eq!(peer.native_capture.len(), 2);
        assert_ne!(a.source_info().source, b.source_info().source);
        let mut request = peer.subscriptions[&101].request.clone();
        request.source = a.source_info().source;
        let error = peer
            .subscribe_with_preflight(
                request,
                addr,
                sender,
                config.stats.clone(),
                false,
                || Ok(()),
            )
            .await
            .unwrap_err();
        assert!(error.contains("another or revoked subscription"));
        assert!(!*b.revocation().borrow());
        // Simulate the currently streamed old grant while replacing its candidate.
        peer.subscriptions.get_mut(&100).unwrap().native_capture = Some(a.clone());
        let (_fc, c) = crate::linux_portal::runtime::tests::prepared_fixture().await;
        peer.commit_native_capture(target(100), owner, c.clone())
            .unwrap();
        assert!(!*a.revocation().borrow());
        assert!(!*b.revocation().borrow());
        c.revoke();
        assert!(!*b.revocation().borrow());
        peer.subscriptions.remove(&100);
        assert!(*a.revocation().borrow());
        assert!(!*b.revocation().borrow());
        drop(peer);
        assert!(*b.revocation().borrow());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_portal_grant_commit_rechecks_owner_revision_and_preserves_old_source() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = mux.split();
        let config = HostServiceConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let addr = "127.0.0.1:40123".parse().unwrap();
        let mut peer = Connection::new(31, addr, sender, &config);
        let (subscription, _) = mock_subscription(CaptureSource::MainDisplay);
        let id = subscription.request.id;
        peer.subscriptions.insert(id, subscription);
        tokio::task::yield_now().await;
        let owner = PeerOwner::from_authenticated_connection(31, [1; 16], Some([2; 32])).unwrap();
        let other = PeerOwner::from_authenticated_connection(31, [3; 16], Some([2; 32])).unwrap();
        let target = |owner, revision| LocalPortalShareTarget {
            addr,
            owner,
            source_revision: revision,
            subscription_id: id,
        };
        let (_first_fixture, first) = crate::linux_portal::runtime::tests::prepared_fixture().await;
        let info = peer
            .commit_native_capture(target(owner, 10), owner, first.clone())
            .unwrap();
        assert!(matches!(info.source, CaptureSource::Window(_)));
        assert!(!info.supports_input);
        assert_eq!(
            peer.subscriptions[&id].request.source,
            CaptureSource::MainDisplay
        );
        let (_second_fixture, second) =
            crate::linux_portal::runtime::tests::prepared_fixture().await;
        assert!(
            peer.commit_native_capture(target(owner, 10), other, second.clone())
                .is_err()
        );
        assert!(*second.revocation().borrow());
        assert!(!*first.revocation().borrow());
        assert_eq!(
            peer.native_capture.get(&id).unwrap().source_info().source,
            info.source
        );
        let mut unsupported = peer.subscriptions[&id].request.clone();
        unsupported.source = info.source;
        unsupported.width = 1;
        unsupported.height = 1;
        let check_mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (check_sender, _) = check_mux.split();
        assert!(
            peer.subscribe_with_preflight(
                unsupported,
                addr,
                check_sender,
                config.stats.clone(),
                false,
                || Ok(())
            )
            .await
            .is_err()
        );
        assert_eq!(
            peer.subscriptions[&id].request.source,
            CaptureSource::MainDisplay
        );
        assert!(!peer.subscriptions[&id].task.is_finished());
        assert!(!*first.revocation().borrow());
        let (_third_fixture, third) = crate::linux_portal::runtime::tests::prepared_fixture().await;
        assert!(
            peer.commit_native_capture(target(owner, 9), owner, third.clone())
                .is_err()
        );
        assert!(*third.revocation().borrow());
        assert!(!peer.subscriptions[&id].task.is_finished());
        drop(peer);
        assert!(*first.revocation().borrow());
    }

    #[tokio::test]
    async fn window_input_requires_current_source_revision_and_rejects_legacy() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = mux.split();
        let addr = "127.0.0.1:39577".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender, &config);
        let (mut sub, _) = mock_subscription(CaptureSource::Window(77));
        sub.input_target = Some(InputTarget::Window { id: 77, pid: 123 });
        sub.settings.send_modify(|s| s.paused = false);
        peer.subscriptions.insert(100, sub);
        assert!(peer.can_scoped_input(100, 10));
        assert!(!peer.can_scoped_input(100, 9));
        assert!(!peer.can_scoped_input(100, 11));
        assert!(!peer.can_legacy_input(100));
        assert!(!peer.can_scoped_input(999, 10));
        peer.subscriptions.get_mut(&100).unwrap().source_revision = 11;
        assert!(!peer.can_scoped_input(100, 10));
        assert!(peer.can_scoped_input(100, 11));
        peer.subscriptions
            .get_mut(&100)
            .unwrap()
            .settings
            .send_modify(|s| s.paused = true);
        assert!(!peer.can_scoped_input(100, 11));
    }

    #[cfg(unix)]
    #[test]
    fn portal_restoration_authority_is_not_an_address_or_claimed_connection_id() {
        let make = |connection, nonce, scope, subscription| LocalPortalShareTarget {
            addr: "127.0.0.1:39577".parse().unwrap(),
            owner: PeerOwner::from_authenticated_connection(connection, nonce, Some(scope)).unwrap(),
            source_revision: 10,
            subscription_id: subscription,
        };
        let approved = make(31, [1;16], [2;32], 100);
        let mut current = approved.clone();
        current.source_revision = 11;
        assert!(approved.same_authenticated_target(&current), "commit checks the new source revision separately");
        for changed in [make(32,[1;16],[2;32],100), make(31,[3;16],[2;32],100),
                        make(31,[1;16],[4;32],100), make(31,[1;16],[2;32],101)] {
            assert!(!approved.same_authenticated_target(&changed));
        }
        current.addr = "127.0.0.1:39578".parse().unwrap();
        assert!(!approved.same_authenticated_target(&current));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn selected_display_requires_scoped_input_and_rejects_previous_source() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _receiver) = mux.split();
        let addr = "127.0.0.1:39577".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender, &config);
        let (mut sub, _) = mock_subscription(CaptureSource::Display(77));
        sub.input_target = input_target_for(sub.request.source, None, true);
        sub.source_revision = 0;
        sub.settings.send_modify(|s| s.paused = false);
        peer.subscriptions.insert(100, sub);
        assert!(
            peer.can_input(100),
            "mocked permission is available; no TCC access"
        );
        assert!(
            !peer.can_legacy_input(100),
            "the old viewer message is silently rejected"
        );
        assert!(
            peer.can_scoped_input(100, 0),
            "initial subscription also uses scoped input"
        );
        peer.subscriptions.get_mut(&100).unwrap().source_revision = 10;
        assert!(peer.can_scoped_input(100, 10));
        assert!(!peer.can_scoped_input(100, 9));
        assert!(!peer.can_scoped_input(999, 10));
        let current = peer.subscriptions.get_mut(&100).unwrap();
        current.request.source = CaptureSource::Display(1);
        current.input_target = input_target_for(current.request.source, None, true);
        current.source_revision = 11;
        assert!(
            !peer.can_scoped_input(100, 10),
            "late input must not hit another display"
        );
        assert!(peer.can_scoped_input(100, 11));
        peer.subscriptions
            .get_mut(&100)
            .unwrap()
            .settings
            .send_modify(|s| s.paused = true);
        assert!(!peer.can_scoped_input(100, 11));
    }

    #[cfg(all(unix, any(test, target_os = "linux")))]
    #[tokio::test]
    async fn portal_input_uses_exact_session_target_and_releases_without_global_injector() {
        let (fixture, capture) =
            crate::linux_portal::runtime::tests::prepared_control_fixture().await;
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _receiver) = mux.split();
        let addr = "127.0.0.1:39577".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        let (mut sub, _) = mock_subscription(capture.source_info().source);
        sub.input_target = Some(InputTarget::Portal);
        sub.native_capture = Some(capture.clone());
        sub.settings.send_modify(|s| s.paused = false);
        peer.subscriptions.insert(100, sub);
        assert!(peer.can_scoped_input(100, 10));
        assert!(!peer.accepts_input_revision(100, 9));
        assert!(!peer.can_legacy_input(100));
        let injector = LazyInputInjector::default();
        let mut active = ActiveInput::default();
        inject(
            &injector,
            &mut active,
            &mut peer,
            &sender,
            addr,
            100,
            InputEvent::Key {
                key_code: 0,
                pressed: true,
                modifiers: protocol::input_modifiers::SHIFT,
            },
        )
        .await;
        assert_eq!(fixture.target_text()[0], "A");
        assert_eq!(active.owner, Some((addr, 100)));
        assert!(
            injector.0.lock().unwrap().is_none(),
            "portal input must never create a global OS injector"
        );
        active.release(&injector).await.unwrap();
        assert!(fixture.held_input_empty());
        capture.revoke();
        assert!(!peer.can_scoped_input(100, 10));
    }

    #[cfg(all(unix, any(test, target_os = "linux")))]
    #[tokio::test]
    async fn native_resubscribe_releases_keys_without_revoking_reused_source() {
        let (fixture, capture) =
            crate::linux_portal::runtime::tests::prepared_control_fixture().await;
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _receiver) = mux.split();
        let addr = "127.0.0.1:39579".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        let source = capture.source_info().source;
        let (mut sub, _) = mock_subscription(source);
        sub.input_target = Some(InputTarget::Portal);
        sub.native_capture = Some(capture.clone());
        sub.settings.send_modify(|s| s.paused = false);
        // A completed stream must be recreated even on the same selected source;
        // unchanged active rate updates correctly use the earlier fast path.
        sub.task.abort();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !sub.task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        peer.subscriptions.insert(100, sub);
        peer.native_capture.insert(100, capture.clone());
        capture.inject_input(InputEvent::KeyDown(0)).await.unwrap();
        peer.subscribe_with_preflight(
            SubscriptionRequest {
                id: 100,
                source,
                width: 1280,
                height: 720,
                fps: 30,
                bitrate_kbps: 4000,
                audio: false,
            },
            addr,
            sender,
            config.stats.clone(),
            false,
            || Ok(()),
        )
        .await
        .unwrap();
        assert!(fixture.held_input_empty());
        assert!(
            !capture.is_revoked(),
            "reusing a selected source must preserve its session lease"
        );
        assert_eq!(peer.subscriptions[&100].source_revision, 10);
        drop(peer);
        assert!(capture.is_revoked());
    }

    #[tokio::test]
    async fn replacing_source_awaits_teardown_and_keeps_activity_and_packet_sequence() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _rx) = mux.split();
        let addr = "127.0.0.1:39574".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        let (old, stopped) = mock_subscription(CaptureSource::Display(77));
        peer.subscriptions.insert(100, old);
        let request = SubscriptionRequest {
            id: 100,
            source: CaptureSource::MainDisplay,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 4000,
            audio: false,
        };
        peer.subscribe_with_preflight(
            request,
            addr,
            sender,
            config.stats.clone(),
            false,
            || Ok(()),
        )
        .await
        .unwrap();
        assert!(
            stopped.load(Relaxed),
            "old capture must finish before replacement admission"
        );
        let new = peer.subscriptions.get(&100).unwrap();
        assert_eq!(new.request.source, CaptureSource::MainDisplay);
        assert_eq!(new.revision, 7);
        assert_eq!(new.settings_revision, 11);
        assert!(new.settings.borrow().paused);
        assert!(!new.audio_active);
        assert_eq!(new.video_sequence.load(Relaxed), 37);
        // The current-thread test never polls the replacement capture future.
        drop(peer);
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn source_switch_request_ids_are_monotonic_and_duplicate_safe() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _rx) = mux.split();
        let addr = "127.0.0.1:39575".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        let (old, stopped) = mock_subscription(CaptureSource::MainDisplay);
        peer.subscriptions.insert(100, old);
        for request_id in [11, 11] {
            let result = peer
                .switch_source(
                    100,
                    request_id,
                    CaptureSource::MainDisplay,
                    addr,
                    sender.clone(),
                    config.stats.clone(),
                )
                .await
                .unwrap();
            assert!(matches!(
                result,
                SessionCommand::SourceSwitched { request_id: 11, .. }
            ));
            assert!(
                !stopped.load(Relaxed),
                "duplicate/same-source requests must not restart capture"
            );
        }
        assert!(
            peer.switch_source(
                100,
                9,
                CaptureSource::Display(77),
                addr,
                sender,
                config.stats
            )
            .await
            .is_err()
        );
        drop(peer);
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn resubscribe_allows_capture_source_switch() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _receiver) = mux.split();
        let addr = "127.0.0.1:39572".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        let first_request = SubscriptionRequest {
            id: 100,
            source: CaptureSource::MainDisplay,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 4000,
            audio: false,
        };
        let res1 = peer
            .subscribe_with_preflight(
                first_request,
                addr,
                sender.clone(),
                config.stats.clone(),
                false,
                || Ok(()),
            )
            .await;
        assert!(res1.is_ok());
        assert!(peer.subscriptions.contains_key(&100));

        // Re-subscribing with same settings is idempotent
        let res1_repeat = peer
            .subscribe_with_preflight(
                SubscriptionRequest {
                    id: 100,
                    source: CaptureSource::MainDisplay,
                    width: 1280,
                    height: 720,
                    fps: 30,
                    bitrate_kbps: 4000,
                    audio: false,
                },
                addr,
                sender.clone(),
                config.stats.clone(),
                false,
                || Ok(()),
            )
            .await;
        assert!(res1_repeat.is_ok());

        // Re-subscribing with updated settings/audio cleanly switches the active stream
        let res2 = peer
            .subscribe_with_preflight(
                SubscriptionRequest {
                    id: 100,
                    source: CaptureSource::MainDisplay,
                    width: 1920,
                    height: 1080,
                    fps: 60,
                    bitrate_kbps: 8000,
                    audio: true,
                },
                addr,
                sender.clone(),
                config.stats.clone(),
                false,
                || Ok(()),
            )
            .await;
        assert!(res2.is_ok());
        let active = peer
            .subscriptions
            .get(&100)
            .expect("replacement subscription");
        assert_eq!(active.request.width, 1920);
        assert_eq!(active.request.height, 1080);
        assert_eq!(active.request.fps, 60);
        assert_eq!(active.request.bitrate_kbps, 8000);
        assert!(active.request.audio);
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn windows_resubscribe_releases_previous_capture_reservation_first() {
        WINDOWS_VIDEO_CAPTURE_RESERVED.store(false, Relaxed);
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _receiver) = mux.split();
        let addr = "127.0.0.1:39573".parse().unwrap();
        let config = HostServiceConfig {
            bind_addr: addr,
            stats: Statistics::new(),
            enable_clipboard_sync: false,
            enable_file_transfer: false,
            enable_talkback: false,
        };
        let mut peer = Connection::new(1, addr, sender.clone(), &config);
        let first = SubscriptionRequest {
            id: 101,
            source: CaptureSource::MainDisplay,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 4000,
            audio: false,
        };
        peer.subscribe_with_preflight(
            first,
            addr,
            sender.clone(),
            config.stats.clone(),
            false,
            || Ok(()),
        )
        .await
        .unwrap();

        let replacement = peer
            .subscribe_with_preflight(
                SubscriptionRequest {
                    id: 101,
                    source: CaptureSource::MainDisplay,
                    width: 1920,
                    height: 1080,
                    fps: 60,
                    bitrate_kbps: 8000,
                    audio: true,
                },
                addr,
                sender,
                config.stats.clone(),
                false,
                || Ok(()),
            )
            .await;
        assert!(replacement.is_ok());
        assert!(WINDOWS_VIDEO_CAPTURE_RESERVED.load(Relaxed));
        drop(peer);
        tokio::task::yield_now().await;
        assert!(!WINDOWS_VIDEO_CAPTURE_RESERVED.load(Relaxed));
    }
}

#[cfg(test)]
mod input_target_tests {
    use super::*;
    #[test]
    fn windows_are_bound_to_server_selected_owner() {
        assert_eq!(
            input_target_for(CaptureSource::Window(42), Some(123), true),
            Some(InputTarget::Window { id: 42, pid: 123 })
        );
        for pid in [None, Some(0), Some(-1)] {
            assert_eq!(input_target_for(CaptureSource::Window(42), pid, true), None);
        }
    }
    #[test]
    fn readonly_never_falls_back_to_desktop() {
        assert_eq!(
            input_target_for(CaptureSource::Window(42), Some(123), false),
            None
        );
        assert_eq!(
            input_target_for(CaptureSource::Display(77), None, false),
            None
        );
        assert_eq!(
            input_target_for(CaptureSource::MainDisplay, None, true),
            Some(InputTarget::Desktop)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn each_selected_display_keeps_its_actual_id_and_rejects_unscoped_input() {
        for id in [1, 77, u32::MAX] {
            assert_eq!(
                input_target_for(CaptureSource::Display(id), None, true),
                Some(InputTarget::Display { id })
            );
        }
        assert_ne!(InputTarget::Display { id: 77 }, InputTarget::Desktop);
        assert_eq!(
            input_target_for(CaptureSource::Display(77), None, false),
            None
        );
    }
}
