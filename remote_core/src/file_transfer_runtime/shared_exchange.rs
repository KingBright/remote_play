//! Bounded, idempotent pull controls on the existing authenticated file lane.
use super::*;
use protocol::shared_files::{
    MAX_SHARED_NAME_BYTES, SHARED_PAGE_SIZE, SharedFileRequest as Request,
    SharedFileResponse as Response,
};
use std::time::{Duration, Instant};
pub(super) struct SharedExchange {
    config: FileTransferRuntimeConfig,
    allocator: FileTransferIdAllocator,
    sender: ScheduledDataSender,
    events: mpsc::UnboundedSender<FileTransferEvent>,
    cancel: broadcast::Receiver<()>,
    cancel_state: FileTransferCancelState,
    pending: HashMap<u64, (Request, Instant)>,
    requests: HashMap<u64, Request>,
    received: HashMap<u64, Response>,
    replies: HashMap<u64, (Request, Response)>,
}
impl SharedExchange {
    pub(super) fn new(
        config: FileTransferRuntimeConfig,
        allocator: FileTransferIdAllocator,
        sender: ScheduledDataSender,
        events: mpsc::UnboundedSender<FileTransferEvent>,
        cancel: broadcast::Receiver<()>,
        cancel_state: FileTransferCancelState,
    ) -> Self {
        Self {
            config,
            allocator,
            sender,
            events,
            cancel,
            cancel_state,
            pending: HashMap::new(),
            requests: HashMap::new(),
            received: HashMap::new(),
            replies: HashMap::new(),
        }
    }
    pub(super) fn pending(&self) -> bool {
        !self.pending.is_empty()
    }
    async fn send(&self, control: FileTransferControl) -> Result<(), FileTransferRuntimeError> {
        send_file_control(
            control,
            self.config.stream_id,
            &self.sender,
            &self.allocator,
        )
        .await
    }
    fn emit(&self, id: u64, response: Response) {
        emit_event(
            &self.events,
            FileTransferEvent::SharedResponse {
                request_id: id,
                response,
            },
        );
    }
    pub(super) async fn request(
        &mut self,
        id: u64,
        request: Request,
    ) -> Result<(), FileTransferRuntimeError> {
        if id == 0
            || self.requests.contains_key(&id)
            || self.pending.len() >= 8
            || self.requests.len() >= 256
        {
            self.emit(
                id,
                rejected("Too many requests or reused request ID; reconnect before retrying"),
            );
            return Ok(());
        }
        self.requests.insert(id, request.clone());
        self.pending.insert(id, (request.clone(), Instant::now()));
        self.send(FileTransferControl::SharedRequest {
            request_id: id,
            request,
        })
        .await
    }
    pub(super) async fn tick(&mut self) -> Result<(), FileTransferRuntimeError> {
        let pending: Vec<_> = self
            .pending
            .iter()
            .map(|(id, (request, at))| (*id, request.clone(), *at))
            .collect();
        for (id, request, at) in pending {
            if at.elapsed() >= Duration::from_secs(8) {
                self.pending.remove(&id);
                let response =
                    rejected("Shared-file request timed out; the peer may need an update");
                self.received.insert(id, response.clone());
                self.emit(id, response);
            } else {
                self.send(FileTransferControl::SharedRequest {
                    request_id: id,
                    request,
                })
                .await?;
            }
        }
        Ok(())
    }
    pub(super) async fn handle(
        &mut self,
        envelope: &DataEnvelope,
        tasks: &mut tokio::task::JoinSet<()>,
    ) -> Result<bool, FileTransferRuntimeError> {
        if envelope.header.kind != ContentKind::FileControl || envelope.payload.len() > 8192 {
            return Ok(false);
        }
        let Ok(control) = control_from_envelope(envelope) else {
            return Ok(false);
        };
        match control {
            FileTransferControl::SharedResponse {
                request_id,
                response,
            } => {
                let Some(request) = self.requests.get(&request_id) else {
                    return Ok(true);
                };
                let valid = match (request, &response) {
                    (_, Response::Rejected { message }) => message.len() <= 512,
                    (Request::Fetch { .. }, Response::Queued { transfer_id }) => *transfer_id != 0,
                    (
                        Request::List { after_id },
                        Response::Page {
                            entries,
                            next_after_id,
                        },
                    ) => {
                        entries.len() <= SHARED_PAGE_SIZE
                            && entries.iter().all(|e| {
                                e.id > *after_id
                                    && e.name.len() <= MAX_SHARED_NAME_BYTES
                                    && !e.name.chars().any(char::is_control)
                            })
                            && entries.windows(2).all(|e| e[0].id < e[1].id)
                            && next_after_id
                                .is_none_or(|next| entries.last().is_some_and(|e| e.id == next))
                    }
                    _ => false,
                };
                if !valid {
                    return Ok(true);
                };
                if let Some(old) = self.received.get(&request_id)
                    && !matches!(
                        (old, &response),
                        (Response::Queued { .. }, Response::Rejected { .. })
                    )
                {
                    return Ok(true);
                }
                self.pending.remove(&request_id);
                self.received.insert(request_id, response.clone());
                self.emit(request_id, response);
            }
            FileTransferControl::SharedRequest {
                request_id,
                request,
            } => {
                let response = if let Some((previous, response)) = self.replies.get(&request_id) {
                    if previous == &request {
                        response.clone()
                    } else {
                        rejected("Request ID reused for a different operation")
                    }
                } else if request_id == 0 || self.replies.len() >= 256 {
                    rejected("Shared-file request limit reached; reconnect")
                } else {
                    let response = match &request {
                        Request::List { after_id } => {
                            let (entries, next_after_id) = self
                                .config
                                .share_catalog
                                .page(self.config.share_scope, *after_id);
                            Response::Page {
                                entries,
                                next_after_id,
                            }
                        }
                        Request::Fetch { file_id } => self.fetch(*file_id, request_id, tasks),
                    };
                    self.replies.insert(request_id, (request, response.clone()));
                    response
                };
                self.send(FileTransferControl::SharedResponse {
                    request_id,
                    response,
                })
                .await?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    fn fetch(
        &self,
        file_id: u64,
        request_id: u64,
        tasks: &mut tokio::task::JoinSet<()>,
    ) -> Response {
        if tasks.len() >= 4 {
            return rejected("Four file transfers are already active");
        }
        let Some(entry) = self
            .config
            .share_catalog
            .get(self.config.share_scope, file_id)
        else {
            return rejected("File is not shared or sharing was revoked");
        };
        // Cloned handles may share an OS offset. Serialize this file's readers.
        let Ok(guard) = entry.file.clone().try_lock_owned() else {
            return rejected("This shared file is already being read; retry after it finishes");
        };
        let spec = next_file_transfer_spec(&self.config, &self.allocator);
        let transfer_id = spec.transfer_id;
        let sender = self.sender.clone();
        let allocator = self.allocator.clone();
        let events = self.events.clone();
        let policy = self.config.send_policy;
        let stream_id = self.config.stream_id;
        let cancel_state = self.cancel_state.clone();
        let mut cancel = self.cancel.resubscribe();
        tasks.spawn(async move {
            let work = async {
                let file = guard.try_clone().await.map_err(FileTransferError::from)?;
                if file
                    .metadata()
                    .await
                    .map_err(FileTransferError::from)?
                    .len()
                    != entry.info.size_bytes
                {
                    return Err(FileTransferRuntimeError::Delivery(
                        "Shared file changed; publish it again".into(),
                    ));
                }
                let mut reader =
                    FileTransferReader::from_open_file(file, entry.info.name.clone(), spec, policy)
                        .await?;
                send_reader(
                    &mut reader,
                    &sender,
                    &events,
                    &mut cancel,
                    &allocator,
                    &cancel_state,
                    stream_id,
                )
                .await?;
                drop(guard);
                Ok::<(), FileTransferRuntimeError>(())
            };
            if let Err(error) = work.await {
                emit_event(
                    &events,
                    FileTransferEvent::Error {
                        transfer_id: Some(transfer_id),
                        message: error.to_string(),
                    },
                );
                let _ = send_file_control(
                    FileTransferControl::SharedResponse {
                        request_id,
                        response: rejected(
                            "Shared file could not be delivered; check the sending device",
                        ),
                    },
                    stream_id,
                    &sender,
                    &allocator,
                )
                .await;
            }
        });
        Response::Queued { transfer_id }
    }
}
fn rejected(message: &str) -> Response {
    Response::Rejected {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{MultiplexedPacket, UdpMultiplexer};
    struct Harness {
        commands: mpsc::Sender<FileTransferCommand>,
        events: mpsc::UnboundedReceiver<FileTransferEvent>,
        server_events: mpsc::UnboundedReceiver<FileTransferEvent>,
        scheduled: ScheduledDataSender,
        catalog: Arc<crate::shared_files::SharedFileCatalog>,
        directory: PathBuf,
        cancel: broadcast::Sender<()>,
        tasks: Vec<tokio::task::AbortHandle>,
    }
    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = self.cancel.send(());
            for task in &self.tasks {
                task.abort();
            }
            self.catalog.clear();
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
    impl Harness {
        async fn new(drop_first_reply: bool) -> Self {
            let directory = std::env::temp_dir().join(format!("rp-pull-{}", rand::random::<u64>()));
            std::fs::create_dir_all(directory.join("downloads")).unwrap();
            let left = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
            let right = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
            let left_addr = left.local_addr().unwrap();
            let right_addr = right.local_addr().unwrap();
            let (ls, lr) = left.split();
            let (rs, rr) = right.split();
            let (lout, lworker) = ScheduledDataSender::spawn(ls, right_addr, Default::default());
            let (rout, rworker) = ScheduledDataSender::spawn(rs, left_addr, Default::default());
            let (commands, lcommands) = mpsc::channel(16);
            let (keep_commands, rcommands) = mpsc::channel(16);
            let (ltx, lin) = mpsc::channel(128);
            let (rtx, rin) = mpsc::channel(128);
            let (events_tx, events) = mpsc::unbounded_channel();
            let (server_tx, server_events) = mpsc::unbounded_channel();
            let (cancel, _) = broadcast::channel(4);
            let catalog = Arc::new(crate::shared_files::SharedFileCatalog::default());
            let a = tokio::spawn(run_file_transfer_runtime(
                lout.clone(),
                lcommands,
                lin,
                events_tx,
                cancel.subscribe(),
                FileTransferRuntimeConfig {
                    receive_dir: directory.join("downloads"),
                    share_catalog: Arc::new(Default::default()),
                    share_scope: Some([4; 32]),
                    ..Default::default()
                },
            ));
            let b = tokio::spawn(run_file_transfer_runtime(
                rout,
                rcommands,
                rin,
                server_tx,
                cancel.subscribe(),
                FileTransferRuntimeConfig {
                    receive_dir: directory.join("received-by-server"),
                    share_catalog: catalog.clone(),
                    share_scope: Some([4; 32]),
                    ..Default::default()
                },
            ));
            let lf = tokio::spawn(async move {
                let mut drop_reply = drop_first_reply;
                while let Ok(packet) = lr.recv().await {
                    if let MultiplexedPacket::Data(e, addr)
                    | MultiplexedPacket::DataWithTiming(e, _, addr) = packet
                    {
                        if addr != right_addr {
                            continue;
                        }
                        if drop_reply
                            && matches!(
                                control_from_envelope(&e),
                                Ok(FileTransferControl::SharedResponse { .. })
                            )
                        {
                            drop_reply = false;
                            continue;
                        }
                        if ltx.send(e).await.is_err() {
                            break;
                        }
                    }
                }
            });
            let rf = tokio::spawn(async move {
                let _keep_commands = keep_commands;
                while let Ok(packet) = rr.recv().await {
                    if let MultiplexedPacket::Data(e, addr)
                    | MultiplexedPacket::DataWithTiming(e, _, addr) = packet
                    {
                        if addr == left_addr && rtx.send(e).await.is_err() {
                            break;
                        }
                    }
                }
            });
            Self {
                commands,
                events,
                server_events,
                scheduled: lout,
                catalog,
                directory,
                cancel,
                tasks: vec![
                    a.abort_handle(),
                    b.abort_handle(),
                    lf.abort_handle(),
                    rf.abort_handle(),
                    lworker.abort_handle(),
                    rworker.abort_handle(),
                ],
            }
        }
        async fn request(&self, id: u64, request: Request) {
            self.commands
                .send(FileTransferCommand::SharedRequest {
                    request_id: id,
                    request,
                })
                .await
                .unwrap();
        }
        async fn response(&mut self, id: u64) -> Response {
            tokio::time::timeout(Duration::from_secs(12), async {
                loop {
                    match self.events.recv().await.unwrap() {
                        FileTransferEvent::SharedResponse {
                            request_id,
                            response,
                        } if request_id == id => break response,
                        FileTransferEvent::Error { message, .. } => panic!("{message}"),
                        _ => {}
                    }
                }
            })
            .await
            .expect("shared response deadline")
        }
    }
    #[tokio::test]
    async fn pull_retries_a_lost_list_reply_and_duplicate_fetch_sends_once() {
        let mut h = Harness::new(true).await;
        let content: Vec<u8> = (0..262144).map(|n| (n % 251) as u8).collect();
        let source = h.directory.join("explicitly-shared.bin");
        std::fs::write(&source, &content).unwrap();
        let info = h.catalog.publish(&source, Some([4; 32])).await.unwrap();
        h.request(1, Request::List { after_id: 0 }).await;
        assert!(
            matches!(h.response(1).await,Response::Page {entries,..} if entries==vec![info.clone()])
        );
        h.request(2, Request::Fetch { file_id: info.id }).await;
        let duplicate = control_to_envelope(
            &FileTransferControl::SharedRequest {
                request_id: 2,
                request: Request::Fetch { file_id: info.id },
            },
            901,
            2,
            9001,
            now_ms(),
        )
        .unwrap();
        h.scheduled.send(duplicate).await.unwrap();
        let path = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match h.events.recv().await.unwrap() {
                    FileTransferEvent::IncomingCompleted { path, .. } => break path,
                    FileTransferEvent::Error { message, .. } => panic!("{message}"),
                    _ => {}
                }
            }
        })
        .await
        .expect("verified download deadline");
        assert_eq!(std::fs::read(path).unwrap(), content);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut started = 0;
        while let Ok(event) = h.server_events.try_recv() {
            if matches!(event, FileTransferEvent::OutgoingStarted { .. }) {
                started += 1;
            }
        }
        assert_eq!(
            started, 1,
            "a repeated fetch must not start another transfer"
        );
        h.catalog.clear();
        h.request(3, Request::Fetch { file_id: info.id }).await;
        assert!(matches!(h.response(3).await, Response::Rejected { .. }));
    }
    #[tokio::test]
    async fn unknown_ids_and_other_network_files_are_not_exposed() {
        let mut h = Harness::new(false).await;
        let source = h.directory.join("other-network.txt");
        std::fs::write(&source, b"private").unwrap();
        let info = h.catalog.publish(&source, Some([5; 32])).await.unwrap();
        h.request(1, Request::List { after_id: 0 }).await;
        assert!(matches!(h.response(1).await,Response::Page {entries,..} if entries.is_empty()));
        h.request(2, Request::Fetch { file_id: info.id }).await;
        assert!(matches!(h.response(2).await, Response::Rejected { .. }));
        assert!(
            std::fs::read_dir(h.directory.join("downloads"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}
