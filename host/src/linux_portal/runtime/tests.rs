// Private bus integration tests. No desktop portal or permission database used.

use super::*;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use tokio::sync::Notify;
use zbus::message::Header;

#[derive(Clone, Copy)]
enum Behavior {
    EarlyResponse,
    NoCreateResponse,
    NoStartResponse,
}

#[derive(Default)]
struct Calls {
    events: Mutex<Vec<String>>,
    requests: Mutex<Vec<(String, String)>>,
    peers: Mutex<Vec<UnixStream>>,
    sessions: Mutex<Vec<String>>,
    changed: Notify,
}
impl Calls {
    fn record(&self, name: &str) {
        self.events.lock().unwrap().push(name.into());
        self.changed.notify_one();
    }
    fn count(&self, name: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.as_str() == name)
            .count()
    }
    async fn wait(&self, name: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.count(name) == 0 {
                self.changed.notified().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("private portal did not reach {name}"));
    }
}

struct FakeRequest(Arc<Calls>);
#[zbus::interface(name = "org.freedesktop.portal.Request")]
impl FakeRequest {
    fn close(&self) {
        self.0.record("request_close");
    } // Never emits Response.
}
struct FakeSession(Arc<Calls>);
#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl FakeSession {
    fn close(&self) {
        self.0.record("session_close");
    }
}

struct FakePortal {
    calls: Arc<Calls>,
    behavior: Behavior,
    source_type: u32,
}
fn text(options: &Values, key: &str) -> String {
    <&str>::try_from(options.get(key).unwrap())
        .unwrap()
        .to_owned()
}
async fn register_request(
    connection: &Connection,
    header: Header<'_>,
    options: &Values,
    calls: &Arc<Calls>,
) -> (String, String) {
    let sender = header.sender().unwrap().as_str().to_owned();
    let component = sender.trim_start_matches(':').replace('.', "_");
    let path = format!(
        "{DESKTOP_PATH}/request/{component}/{}",
        text(options, "handle_token")
    );
    connection
        .object_server()
        .at(path.as_str(), FakeRequest(calls.clone()))
        .await
        .unwrap();
    calls
        .requests
        .lock()
        .unwrap()
        .push((path.clone(), sender.clone()));
    (path, sender)
}
async fn emit_response(connection: &Connection, path: &str, destination: &str, values: Values) {
    connection
        .emit_signal(
            Some(destination),
            path,
            REQUEST,
            "Response",
            &(0u32, values),
        )
        .await
        .unwrap();
}

#[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
impl FakePortal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        5
    }
    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        7
    }
    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        3
    }

    async fn create_session(
        &self,
        options: Values,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_eq!(options.len(), 2);
        let (request, destination) =
            register_request(connection, header, &options, &self.calls).await;
        let component = destination.trim_start_matches(':').replace('.', "_");
        let session = format!(
            "{DESKTOP_PATH}/session/{component}/{}",
            text(&options, "session_handle_token")
        );
        connection
            .object_server()
            .at(session.as_str(), FakeSession(self.calls.clone()))
            .await
            .unwrap();
        self.calls.sessions.lock().unwrap().push(session.clone());
        if !matches!(self.behavior, Behavior::NoCreateResponse) {
            emit_response(
                connection,
                &request,
                &destination,
                HashMap::from([(
                    "session_handle".into(),
                    Value::from(session.as_str()).try_to_owned().unwrap(),
                )]),
            )
            .await;
        }
        self.calls.record("create");
        Ok(OwnedObjectPath::try_from(request).unwrap())
    }
    async fn select_sources(
        &self,
        _session: OwnedObjectPath,
        options: Values,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_eq!(
            u32::try_from(options.get("persist_mode").unwrap()).unwrap(),
            0
        );
        assert!(!bool::try_from(options.get("multiple").unwrap()).unwrap());
        assert!(!options.contains_key("restore_token"));
        let types = u32::try_from(options.get("types").unwrap()).unwrap();
        assert_eq!(types & 4, 0);
        assert!(matches!(
            u32::try_from(options.get("cursor_mode").unwrap()).unwrap(),
            1 | 2
        ));
        let (request, destination) =
            register_request(connection, header, &options, &self.calls).await;
        emit_response(connection, &request, &destination, Values::new()).await;
        self.calls.record("select");
        Ok(OwnedObjectPath::try_from(request).unwrap())
    }
    async fn start(
        &self,
        _session: OwnedObjectPath,
        parent: String,
        options: Values,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_eq!(parent, "x11:2a");
        assert_eq!(options.len(), 1);
        let (request, destination) =
            register_request(connection, header, &options, &self.calls).await;
        if !matches!(self.behavior, Behavior::NoStartResponse) {
            let properties: Values = HashMap::from([
                ("source_type".into(), OwnedValue::from(self.source_type)),
                (
                    "size".into(),
                    Value::from((640i32, 360i32)).try_to_owned().unwrap(),
                ),
                (
                    "position".into(),
                    Value::from((-640i32, 0i32)).try_to_owned().unwrap(),
                ),
            ]);
            let streams = Value::from(vec![(77u32, properties)])
                .try_to_owned()
                .unwrap();
            emit_response(
                connection,
                &request,
                &destination,
                HashMap::from([("streams".into(), streams)]),
            )
            .await;
        }
        self.calls.record("start");
        Ok(OwnedObjectPath::try_from(request).unwrap())
    }
    async fn open_pipe_wire_remote(
        &self,
        _session: OwnedObjectPath,
        options: Values,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        assert!(options.is_empty());
        let (stream, peer) = UnixStream::pair().unwrap();
        self.calls.peers.lock().unwrap().push(peer);
        self.calls.record("open_remote");
        Ok(OwnedFd::from(stream).into())
    }
}

struct PrivateBus {
    child: Child,
    directory: PathBuf,
    address: String,
}
impl PrivateBus {
    fn start() -> Self {
        // Keep Unix socket paths below sockaddr_un's macOS/Linux limit.
        let directory = PathBuf::from("/tmp").join(token());
        std::fs::create_dir(&directory).unwrap();
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!("--address=unix:tmpdir={}", directory.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect(
                "private D-Bus fixture requires the installed dbus-daemon; no install attempted",
            );
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(
            address.starts_with("unix:"),
            "private dbus-daemon did not return a Unix address"
        );
        Self {
            child,
            directory,
            address: address.trim().into(),
        }
    }
}
impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
pub(crate) struct Fixture {
    _bus: PrivateBus,
    service: Connection,
    client: Connection,
    calls: Arc<Calls>,
    cancel_keepalive: Mutex<Option<watch::Sender<bool>>>,
}

pub(crate) async fn prepared_fixture() -> (Fixture, PreparedPortalCapture) {
    let fixture = Fixture::new(Behavior::EarlyResponse, 2).await;
    let (cancel, cancel_rx) = watch::channel(false);
    let mut lease = fixture.select(cancel_rx).await.unwrap();
    // The fixture sender lives for this lease; closing it must revoke the lease.
    // Keep it inside the fixture rather than leaking a watch sender.
    *fixture.cancel_keepalive.lock().unwrap() = Some(cancel);
    let (_, fd) = lease.take_pipewire_remote().unwrap();
    drop(fd);
    let mut first = crate::linux_raw_encode::tests::frame(
        crate::linux_frame::PixelFormat::Nv12,
        crate::linux_raw_encode::tests::known_color(),
        1,
    );
    first.stamp.generation = lease.selected_source().generation;
    let capture = lease.prepare_owned_capture(first).unwrap();
    (fixture, capture)
}
impl Fixture {
    async fn new(behavior: Behavior, source_type: u32) -> Self {
        let bus = PrivateBus::start();
        let calls = Arc::new(Calls::default());
        let service = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(DESKTOP)
            .unwrap()
            .serve_at(
                DESKTOP_PATH,
                FakePortal {
                    calls: calls.clone(),
                    behavior,
                    source_type,
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .method_timeout(Duration::from_secs(1))
            .build()
            .await
            .unwrap();
        Self {
            _bus: bus,
            service,
            client,
            calls,
            cancel_keepalive: Mutex::new(None),
        }
    }
    fn timeouts(&self) -> Timeouts {
        Timeouts {
            operation: Duration::from_secs(1),
            selection: Duration::from_secs(1),
            cleanup: Duration::from_millis(250),
        }
    }
    async fn select(&self, cancel: watch::Receiver<bool>) -> Result<PortalCaptureLease> {
        select_on_connection(
            self.client.clone(),
            "x11:2a",
            PickerSources::MonitorOrWindow,
            cancel,
            self.timeouts(),
        )
        .await
    }
}

#[tokio::test]
async fn actual_bus_early_response_complete_chain_and_single_fd_handoff() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 2).await;
    let (_cancel, cancel_rx) = watch::channel(false);
    let mut lease = fixture.select(cancel_rx).await.unwrap();
    let selected = lease.selected_source();
    assert_eq!(selected.kind, SourceKind::Window);
    assert_eq!(selected.node_id, 77);
    assert_eq!(selected.logical_size, Some((640, 360)));
    assert_eq!(selected.logical_position, Some((-640, 0)));
    let (node, fd) = lease.take_pipewire_remote().unwrap();
    assert_eq!(node, 77);
    drop(fd);
    assert!(lease.take_pipewire_remote().is_err());
    assert!(lease.close().await);
    assert_eq!(fixture.calls.count("create"), 1);
    assert_eq!(fixture.calls.count("select"), 1);
    assert_eq!(fixture.calls.count("start"), 1);
    assert_eq!(fixture.calls.count("open_remote"), 1);
    assert_eq!(fixture.calls.count("session_close"), 1);
}

#[tokio::test]
async fn actual_bus_cancel_without_response_closes_request_and_session() {
    let fixture = Fixture::new(Behavior::NoStartResponse, 2).await;
    let (cancel, cancel_rx) = watch::channel(false);
    let client = fixture.client.clone();
    let timeouts = fixture.timeouts();
    let selecting = tokio::spawn(select_on_connection(
        client,
        "x11:2a",
        PickerSources::Window,
        cancel_rx,
        timeouts,
    ));
    fixture.calls.wait("start").await;
    cancel.send_replace(true);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), selecting)
            .await
            .unwrap()
            .unwrap(),
        Err(PortalCallError::Cancelled)
    ));
    fixture.calls.wait("request_close").await;
    fixture.calls.wait("session_close").await;
    assert_eq!(fixture.calls.count("request_close"), 1);
    assert_eq!(fixture.calls.count("session_close"), 1);
    assert_eq!(fixture.calls.count("open_remote"), 0);
}

#[tokio::test]
async fn actual_bus_timeout_closes_request_without_waiting_for_response() {
    let fixture = Fixture::new(Behavior::NoStartResponse, 2).await;
    let (_cancel, cancel_rx) = watch::channel(false);
    let mut timeouts = fixture.timeouts();
    timeouts.selection = Duration::from_millis(100);
    let result = select_on_connection(
        fixture.client.clone(),
        "x11:2a",
        PickerSources::Window,
        cancel_rx,
        timeouts,
    )
    .await;
    assert!(matches!(result, Err(PortalCallError::Timeout)));
    fixture.calls.wait("request_close").await;
    fixture.calls.wait("session_close").await;
}

#[tokio::test]
async fn actual_bus_late_create_response_cannot_restart_cancelled_selection() {
    let fixture = Fixture::new(Behavior::NoCreateResponse, 2).await;
    let (cancel, cancel_rx) = watch::channel(false);
    let selecting = tokio::spawn(select_on_connection(
        fixture.client.clone(),
        "x11:2a",
        PickerSources::Window,
        cancel_rx,
        fixture.timeouts(),
    ));
    fixture.calls.wait("create").await;
    cancel.send_replace(true);
    assert!(matches!(
        selecting.await.unwrap(),
        Err(PortalCallError::Cancelled)
    ));
    fixture.calls.wait("session_close").await;
    let (path, sender) = fixture.calls.requests.lock().unwrap()[0].clone();
    emit_response(&fixture.service, &path, &sender, Values::new()).await;
    tokio::task::yield_now().await;
    assert_eq!(fixture.calls.count("select"), 0);
    assert_eq!(fixture.calls.count("start"), 0);
}

#[tokio::test]
async fn actual_bus_session_closed_revokes_before_fd_handoff() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 2).await;
    let (_cancel, cancel_rx) = watch::channel(false);
    let mut lease = fixture.select(cancel_rx).await.unwrap();
    let mut revoked = lease.revocation();
    let path = lease.owner.control.state.lock().unwrap().session.clone();
    fixture
        .service
        .emit_signal(
            None::<&str>,
            path.as_str(),
            SESSION,
            "Closed",
            &(Values::new(),),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), revoked.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(*revoked.borrow());
    assert!(lease.take_pipewire_remote().is_err());
    fixture.calls.wait("session_close").await;
}

#[tokio::test]
async fn actual_bus_unrequested_source_kind_never_opens_remote() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 2).await;
    let (_cancel, cancel_rx) = watch::channel(false);
    let result = select_on_connection(
        fixture.client.clone(),
        "x11:2a",
        PickerSources::Monitor,
        cancel_rx,
        fixture.timeouts(),
    )
    .await;
    assert!(matches!(result, Err(PortalCallError::InvalidSource)));
    fixture.calls.wait("session_close").await;
    assert_eq!(fixture.calls.count("open_remote"), 0);
}

#[tokio::test]
async fn actual_bus_portal_owner_loss_revokes_existing_lease() {
    let fixture = Fixture::new(Behavior::EarlyResponse, 2).await;
    let (_cancel, cancel_rx) = watch::channel(false);
    let mut lease = fixture.select(cancel_rx).await.unwrap();
    let mut revoked = lease.revocation();
    fixture.service.release_name(DESKTOP).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), revoked.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(*revoked.borrow());
    assert!(lease.take_pipewire_remote().is_err());
    fixture.calls.wait("session_close").await;
}

#[tokio::test]
async fn actual_bus_prepared_frames_use_pixel_geometry_and_stop_on_revoke() {
    let (fixture, capture) = prepared_fixture().await;
    let info = capture.source_info();
    assert_eq!((info.width, info.height), (128, 64));
    assert!(!info.supports_input);
    assert_eq!(info.process_id, None);
    assert_eq!(
        info.source,
        CaptureSource::Window(capture.generation() as u32)
    );
    let frames = capture.mailbox();
    let mut capturer =
        crate::linux_capture::LinuxVideoCapturer::from_owned_mailbox(frames.clone(), 30).unwrap();
    use remote_core::VideoCapturer;
    capturer.start().await.unwrap();
    let first = capturer.capture_frame().await.unwrap();
    assert_eq!(
        first.owned.as_ref().unwrap().stamp.generation,
        capture.generation()
    );
    let mut wrong = crate::linux_raw_encode::tests::frame(
        crate::linux_frame::PixelFormat::Nv12,
        crate::linux_raw_encode::tests::known_color(),
        2,
    );
    wrong.stamp.generation = capture.generation() + 1;
    assert!(capture.publish_owned(wrong).is_err());
    let mut revoked = capture.revocation();
    let path = capture
        .inner
        .lease
        .lock()
        .unwrap()
        .owner
        .control
        .state
        .lock()
        .unwrap()
        .session
        .clone();
    fixture
        .service
        .emit_signal(
            None::<&str>,
            path.as_str(),
            SESSION,
            "Closed",
            &(Values::new(),),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), revoked.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(frames.receive().await.is_err());
    let mut after = crate::linux_raw_encode::tests::frame(
        crate::linux_frame::PixelFormat::Nv12,
        crate::linux_raw_encode::tests::known_color(),
        3,
    );
    after.stamp.generation = capture.generation();
    assert!(capture.publish_owned(after).is_err());
}

#[tokio::test]
async fn actual_bus_response_from_another_sender_cannot_authorize_selection() {
    let fixture = Fixture::new(Behavior::NoCreateResponse, 2).await;
    let (_cancel, cancel_rx) = watch::channel(false);
    let selecting = tokio::spawn(select_on_connection(
        fixture.client.clone(),
        "x11:2a",
        PickerSources::Window,
        cancel_rx,
        fixture.timeouts(),
    ));
    fixture.calls.wait("create").await;
    let alien = zbus::connection::Builder::address(fixture._bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let (path, sender) = fixture.calls.requests.lock().unwrap()[0].clone();
    let session = fixture.calls.sessions.lock().unwrap()[0].clone();
    let response = || {
        HashMap::from([(
            "session_handle".into(),
            Value::from(session.as_str()).try_to_owned().unwrap(),
        )])
    };
    emit_response(&alien, &path, &sender, response()).await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(fixture.calls.count("select"), 0);
    assert!(!selecting.is_finished());
    emit_response(&fixture.service, &path, &sender, response()).await;
    let mut lease = selecting.await.unwrap().unwrap();
    assert!(lease.close().await);
    assert_eq!(fixture.calls.count("select"), 1);
}
