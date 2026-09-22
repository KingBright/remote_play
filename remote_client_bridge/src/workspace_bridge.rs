//! Native mobile adapter for the same independent-subscription protocol as desktop.
use crate::{EncodedAudioPacket, EncodedVideoNalu, hevc_keyframe};
use protocol::{
    ControlMessage,
    session::{SessionCommand, SubscriptionRequest},
};
use remote_core::{
    SharedHostStats, Statistics,
    client_session::AudioIngressEvent,
    file_transfer_runtime::{FileTransferCommand, FileTransferEvent},
    workspace_session::WorkspaceConnection,
};
use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

#[derive(Default)]
struct Queues {
    events: VecDeque<serde_json::Value>,
    video: HashMap<u32, VecDeque<EncodedVideoNalu>>,
    audio: HashMap<u32, VecDeque<EncodedAudioPacket>>,
}

#[derive(Clone)]
struct MobileClipboard {
    outgoing: Arc<Mutex<Option<protocol::ClipboardBundle>>>,
    queues: Arc<Mutex<Queues>>,
    directory: PathBuf,
}

#[async_trait::async_trait]
impl remote_core::ClipboardProvider for MobileClipboard {
    fn capabilities(&self) -> remote_core::ClipboardBackendCapabilities {
        remote_core::ClipboardBackendCapabilities {
            text: true,
            image: true,
            file_references: false,
            file_bytes: false,
        }
    }
    async fn read_clipboard(
        &mut self,
        _: remote_core::clipboard_plane::ClipboardSyncPolicy,
    ) -> Result<Option<protocol::ClipboardBundle>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.outgoing.lock().unwrap().clone())
    }
    async fn write_clipboard(
        &mut self,
        bundle: &protocol::ClipboardBundle,
        policy: remote_core::clipboard_plane::ClipboardSyncPolicy,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        policy.validate_bundle(bundle)?;
        let item = if let Some(image) = bundle.items.iter().find_map(|i| {
            if let protocol::ClipboardItem::Image(image) = i {
                Some(image)
            } else {
                None
            }
        }) {
            let extension = match image.mime_type.as_str() {
                "image/png" => "png",
                "image/tiff" => "tiff",
                _ => return Err("unsupported image format".into()),
            };
            tokio::fs::create_dir_all(&self.directory).await?;
            let path = self
                .directory
                .join(format!("clipboard-{}.{}", bundle.bundle_id, extension));
            tokio::fs::write(&path, &image.bytes).await?;
            serde_json::json!({"image":path,"mime":image.mime_type})
        } else if let Some(text) = bundle.items.iter().find_map(|i| {
            if let protocol::ClipboardItem::Text(text) = i {
                Some(&text.text)
            } else {
                None
            }
        }) {
            serde_json::json!({"text":text})
        } else {
            return Err("unsupported clipboard item".into());
        };
        let mut queues = self.queues.lock().unwrap();
        if queues.events.len() == 128 {
            queues.events.pop_front();
        }
        queues
            .events
            .push_back(serde_json::json!({"clipboard":item}));
        Ok(())
    }
}

struct ClipboardIngress(mpsc::Sender<protocol::DataEnvelope>);
impl remote_core::client_session::EnvelopeIngress for ClipboardIngress {
    fn route_inbound(&self, envelope: protocol::DataEnvelope) {
        let _ = self.0.try_send(envelope);
    }
}

pub struct MobileWorkspace {
    connection: Arc<WorkspaceConnection>,
    queues: Arc<Mutex<Queues>>,
    pumps: Mutex<HashMap<u32, tokio::task::AbortHandle>>,
    events: tokio::task::JoinHandle<()>,
    clipboard: MobileClipboard,
    clipboard_cancel: tokio::sync::broadcast::Sender<()>,
    clipboard_worker: tokio::task::AbortHandle,
}

impl Drop for MobileWorkspace {
    fn drop(&mut self) {
        self.events.abort();
        let _ = self.clipboard_cancel.send(());
        self.clipboard_worker.abort();
        for task in self.pumps.lock().unwrap().values() {
            task.abort();
        }
    }
}

impl MobileWorkspace {
    pub async fn connect(endpoint: SocketAddr, receive_dir: PathBuf) -> Result<Self, String> {
        let clipboard_directory = receive_dir
            .parent()
            .unwrap_or(&receive_dir)
            .join("clipboard");
        let (connection, mut events) = WorkspaceConnection::connect(endpoint, receive_dir).await?;
        let connection = Arc::new(connection);
        let queues = Arc::new(Mutex::new(Queues::default()));
        let clipboard = MobileClipboard {
            outgoing: Arc::new(Mutex::new(None)),
            queues: queues.clone(),
            directory: clipboard_directory,
        };
        let (clipboard_tx, clipboard_rx) = mpsc::channel(128);
        connection
            .clipboard(Some(Arc::new(ClipboardIngress(clipboard_tx))))
            .await?;
        let (clipboard_sender, clipboard_worker) =
            remote_core::scheduled_sender::ScheduledDataSender::spawn(
                connection.sender.clone(),
                endpoint,
                Default::default(),
            );
        let (clipboard_cancel, clipboard_cancel_rx) = tokio::sync::broadcast::channel(1);
        tokio::spawn(remote_core::clipboard_runtime::run_clipboard_sync(
            clipboard.clone(),
            clipboard_sender,
            clipboard_rx,
            clipboard_cancel_rx,
            Default::default(),
        ));
        let state = queues.clone();
        let task = tokio::spawn(async move {
            let mut configs = HashMap::new();
            let mut owners = HashMap::new();
            loop {
                tokio::select! {
                    Some(event) = events.control.recv() => {
                        let mut state = state.lock().unwrap();
                        if let SessionCommand::Subscribed { id, audio_owner:Some(owner), .. } = &event { owners.insert(*id, *owner); }
                        if let SessionCommand::Unsubscribed { id } = &event {
                            state.video.remove(id);
                            if let Some(owner) = owners.remove(id) && !owners.values().any(|other| *other == owner) {
                                let stream_id = protocol::remote_system_audio_stream_id(owner);
                                state.audio.remove(&stream_id); configs.remove(&stream_id);
                            }
                        }
                        if state.events.len() == 128 { state.events.pop_front(); }
                        state.events.push_back(serde_json::json!({"control":event}));
                    }
                    Some(event) = events.files.recv() => {
                        let detail = match &event {
                            FileTransferEvent::IncomingClipboardReady { paths, .. } => serde_json::json!({"clipboard_files":paths}),
                            FileTransferEvent::IncomingCompleted { path, .. } => serde_json::json!({"path": path}),
                            FileTransferEvent::Error { message, .. } => serde_json::json!({"error":message}),
                            _ => serde_json::Value::Null,
                        };
                        let mut state = state.lock().unwrap();
                        if state.events.len() == 128 { state.events.pop_front(); }
                        state.events.push_back(serde_json::json!({"file":format!("{event:?}"),"detail":detail}));
                    }
                    Some(event) = events.audio.recv() => match event {
                        AudioIngressEvent::StreamConfig(config) => {
                            if configs.len() < 16 || configs.contains_key(&config.stream_id) { configs.insert(config.stream_id, config); }
                        }
                        AudioIngressEvent::Packet(packet) => {
                            let Some(config) = configs.get(&packet.header.ssrc) else { continue; };
                            let mut state = state.lock().unwrap();
                            let queue = state.audio.entry(packet.header.ssrc).or_default();
                            if queue.len() >= 16 { queue.pop_front(); }
                            queue.push_back(EncodedAudioPacket { data: packet.payload, sample_rate_hz:config.sample_rate_hz, channels:config.channels, pts_us: i64::from(packet.header.timestamp) * 1000 });
                        }
                    },
                    else => break,
                }
            }
        });
        connection
            .control(SessionCommand::ListSources { request_id: 1 })
            .await?;
        Ok(Self {
            connection,
            queues,
            pumps: Mutex::new(HashMap::new()),
            events: task,
            clipboard,
            clipboard_cancel,
            clipboard_worker: clipboard_worker.abort_handle(),
        })
    }

    pub async fn command(&self, json: &str) -> Result<(), String> {
        let command: SessionCommand = serde_json::from_str(json).map_err(|e| e.to_string())?;
        match command {
            SessionCommand::Subscribe(request) => self.subscribe(request).await,
            SessionCommand::Unsubscribe { id } => {
                if let Some(task) = self.pumps.lock().unwrap().remove(&id) {
                    task.abort();
                }
                self.queues.lock().unwrap().video.remove(&id);
                self.connection
                    .control(SessionCommand::Unsubscribe { id })
                    .await
            }
            other => self.connection.control(other).await,
        }
    }

    async fn subscribe(&self, request: SubscriptionRequest) -> Result<(), String> {
        let id = request.id;
        if self.pumps.lock().unwrap().len() >= protocol::session::MAX_SUBSCRIPTIONS {
            return Err("too many subscriptions".into());
        }
        let (tx, mut rx) = mpsc::channel(4);
        self.connection
            .subscribe(
                request,
                tx,
                Statistics::new(),
                Arc::new(SharedHostStats::default()),
            )
            .await?;
        let state = self.queues.clone();
        let connection = Arc::downgrade(&self.connection);
        let task = tokio::spawn(async move {
            let mut waiting = true;
            while let Some((packet, timing)) = rx.recv().await {
                let keyframe = hevc_keyframe(&packet.payload);
                let overflow = {
                    let mut state = state.lock().unwrap();
                    let queue = state.video.entry(id).or_default();
                    let overflow = queue.len() >= 4;
                    if overflow {
                        queue.clear();
                        waiting = true;
                    }
                    if !waiting || keyframe {
                        waiting = false;
                        queue.push_back(EncodedVideoNalu {
                            data: packet.payload,
                            keyframe,
                            pts_us: timing.capture_ts_us as i64,
                        });
                    }
                    overflow
                };
                if overflow && let Some(connection) = connection.upgrade() {
                    let _ = connection
                        .sender
                        .send_control(
                            &ControlMessage::RequestKeyframe { session_id: id },
                            connection.target,
                        )
                        .await;
                }
            }
        });
        if let Some(old) = self.pumps.lock().unwrap().insert(id, task.abort_handle()) {
            old.abort();
        }
        Ok(())
    }

    pub async fn send_file(&self, path: PathBuf) -> Result<(), String> {
        if !self.connection.files_available {
            return Err("peer does not support file transfer".into());
        }
        self.connection
            .file_commands
            .send(FileTransferCommand::SendFile {
                path,
                mime_type: None,
            })
            .await
            .map_err(|e| e.to_string())
    }
    pub async fn keyframe(&self, id: u32) -> Result<(), String> {
        self.connection
            .sender
            .send_control(
                &ControlMessage::RequestKeyframe { session_id: id },
                self.connection.target,
            )
            .await
            .map_err(|e| e.to_string())
    }
    pub async fn send_clipboard(&self, json: &str) -> Result<(), String> {
        if !self.connection.clipboard_available {
            return Err("peer does not support clipboard sharing".into());
        }
        let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if let Some(files) = value["files"].as_array() {
            let files = files
                .iter()
                .map(|path| {
                    Ok(remote_core::file_transfer_runtime::FileTransferGroupFile {
                        path: path.as_str().ok_or("invalid file path")?.into(),
                        mime_type: None,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            return self
                .connection
                .file_commands
                .send(FileTransferCommand::SendClipboardFiles { files })
                .await
                .map_err(|e| e.to_string());
        }
        let item = if let Some(text) = value["text"].as_str() {
            protocol::ClipboardItem::Text(protocol::ClipboardText { text: text.into() })
        } else if let Some(path) = value["image"].as_str() {
            if tokio::fs::metadata(path)
                .await
                .map_err(|e| e.to_string())?
                .len()
                > 64 * 1024 * 1024
            {
                return Err("clipboard image too large".into());
            }
            protocol::ClipboardItem::Image(protocol::ClipboardImage {
                mime_type: value["mime"].as_str().unwrap_or("image/png").into(),
                width: None,
                height: None,
                bytes: tokio::fs::read(path).await.map_err(|e| e.to_string())?,
            })
        } else {
            return Err("empty clipboard".into());
        };
        let bundle = protocol::ClipboardBundle::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            vec![item],
        );
        remote_core::clipboard_plane::ClipboardSyncPolicy::default()
            .validate_bundle(&bundle)
            .map_err(|e| e.to_string())?;
        *self.clipboard.outgoing.lock().unwrap() = Some(bundle);
        Ok(())
    }
    pub async fn settings(&self, json: &str) -> Result<(), String> {
        let request = serde_json::from_str(json).map_err(|e| e.to_string())?;
        self.connection.update_settings(&request).await
    }
    pub fn events(&self) -> String {
        let mut events = self
            .queues
            .lock()
            .unwrap()
            .events
            .drain(..)
            .collect::<Vec<_>>();
        events.push(serde_json::json!({"transfers_active":self.connection.file_activity.load(std::sync::atomic::Ordering::Relaxed)}));
        serde_json::to_string(&events).unwrap_or_else(|_| "[]".into())
    }
    pub fn video(&self, id: u32) -> Option<Vec<u8>> {
        let packet = self
            .queues
            .lock()
            .unwrap()
            .video
            .get_mut(&id)?
            .pop_front()?;
        let mut data = Vec::with_capacity(packet.data.len() + 9);
        data.push(u8::from(packet.keyframe));
        data.extend_from_slice(&packet.pts_us.to_le_bytes());
        data.extend(packet.data);
        Some(data)
    }
    pub fn audio(&self, id: u32) -> Option<Vec<u8>> {
        let packet = self
            .queues
            .lock()
            .unwrap()
            .audio
            .get_mut(&id)?
            .pop_front()?;
        let mut data = Vec::with_capacity(packet.data.len() + 6);
        data.extend_from_slice(&packet.sample_rate_hz.to_le_bytes());
        data.extend_from_slice(&packet.channels.to_le_bytes());
        data.extend(packet.data);
        Some(data)
    }
}
