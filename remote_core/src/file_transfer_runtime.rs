use crate::file_transfer::{
    FileManifestAssembler, FileReceivePolicy, FileTransferError, FileTransferPolicy,
    FileTransferReader, FileTransferSpec, IncomingFileTransfer, control_from_envelope,
    control_to_envelope,
};
use crate::scheduled_sender::{ScheduledDataSendError, ScheduledDataSender};
use protocol::{ContentKind, DataEnvelope, FileTransferControl, FileTransferGroup};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc};

#[derive(Debug, Clone)]
pub struct FileTransferRuntimeConfig {
    pub stream_id: u32,
    pub next_transfer_id: u64,
    pub next_object_id: u64,
    pub next_sequence_number: u64,
    pub chunk_payload_len: usize,
    pub send_policy: FileTransferPolicy,
    pub receive_policy: FileReceivePolicy,
    pub receive_dir: PathBuf,
}

impl Default for FileTransferRuntimeConfig {
    fn default() -> Self {
        Self {
            stream_id: 2,
            next_transfer_id: 1,
            next_object_id: 10_000,
            next_sequence_number: 1,
            chunk_payload_len: crate::file_transfer::DEFAULT_FILE_CHUNK_PAYLOAD_LEN,
            send_policy: FileTransferPolicy::default(),
            receive_policy: FileReceivePolicy::default(),
            receive_dir: std::env::temp_dir().join("remote-play-received-files"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTransferCommand {
    SendFile {
        path: PathBuf,
        mime_type: Option<String>,
    },
    SendFileGroup {
        files: Vec<FileTransferGroupFile>,
    },
    CancelTransfer {
        transfer_id: u64,
    },
    CancelGroup {
        group_id: u64,
    },
    SetReceiveConfig {
        receive_dir: PathBuf,
        receive_policy: FileReceivePolicy,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransferGroupFile {
    pub path: PathBuf,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTransferEvent {
    OutgoingGroupStarted {
        group_id: u64,
        file_count: u32,
        total_size_bytes: u64,
    },
    OutgoingStarted {
        transfer_id: u64,
        file_object_id: u64,
        group: Option<FileTransferGroup>,
        name: String,
        size_bytes: u64,
        total_chunks: u32,
    },
    OutgoingProgress {
        transfer_id: u64,
        file_object_id: u64,
        sent_chunks: u32,
        total_chunks: u32,
        sent_bytes: u64,
        total_size: u64,
    },
    OutgoingCompleted {
        transfer_id: u64,
        file_object_id: u64,
        group: Option<FileTransferGroup>,
    },
    OutgoingCancelled {
        transfer_id: u64,
        file_object_id: u64,
        group: Option<FileTransferGroup>,
    },
    OutgoingGroupCompleted {
        group_id: u64,
        file_count: u32,
        total_size_bytes: u64,
    },
    OutgoingGroupCancelled {
        group_id: u64,
    },
    IncomingStarted {
        transfer_id: u64,
        file_object_id: u64,
        group: Option<FileTransferGroup>,
        name: String,
        size_bytes: u64,
        path: PathBuf,
    },
    IncomingProgress {
        transfer_id: u64,
        file_object_id: u64,
        received_chunks: u32,
        total_chunks: u32,
        received_bytes: u64,
        total_size: u64,
    },
    IncomingCompleted {
        transfer_id: u64,
        file_object_id: u64,
        group: Option<FileTransferGroup>,
        path: PathBuf,
        size_bytes: u64,
    },
    IncomingCancelled {
        transfer_id: u64,
        file_object_id: u64,
        group: Option<FileTransferGroup>,
        path: PathBuf,
    },
    IncomingGroupCompleted {
        group_id: u64,
        paths: Vec<PathBuf>,
        total_size_bytes: u64,
    },
    IncomingGroupCancelled {
        group_id: u64,
    },
    Error {
        transfer_id: Option<u64>,
        message: String,
    },
}

#[derive(Debug)]
pub enum FileTransferRuntimeError {
    Send(ScheduledDataSendError),
    Transfer(FileTransferError),
}

impl fmt::Display for FileTransferRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileTransferRuntimeError::Send(err) => {
                write!(f, "file transfer sender failed: {err}")
            }
            FileTransferRuntimeError::Transfer(err) => {
                write!(f, "file transfer failed: {err}")
            }
        }
    }
}

impl Error for FileTransferRuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            FileTransferRuntimeError::Send(err) => Some(err),
            FileTransferRuntimeError::Transfer(err) => Some(err),
        }
    }
}

impl From<ScheduledDataSendError> for FileTransferRuntimeError {
    fn from(value: ScheduledDataSendError) -> Self {
        Self::Send(value)
    }
}

impl From<FileTransferError> for FileTransferRuntimeError {
    fn from(value: FileTransferError) -> Self {
        Self::Transfer(value)
    }
}

#[derive(Clone)]
struct FileTransferIdAllocator {
    next_transfer_id: Arc<AtomicU64>,
    next_object_id: Arc<AtomicU64>,
    next_sequence_number: Arc<AtomicU64>,
}

impl FileTransferIdAllocator {
    fn new(config: &FileTransferRuntimeConfig) -> Self {
        Self {
            next_transfer_id: Arc::new(AtomicU64::new(config.next_transfer_id)),
            next_object_id: Arc::new(AtomicU64::new(config.next_object_id)),
            next_sequence_number: Arc::new(AtomicU64::new(config.next_sequence_number)),
        }
    }

    fn next_transfer_id(&self) -> u64 {
        self.next_transfer_id.fetch_add(1, Relaxed)
    }

    fn next_group_id(&self) -> u64 {
        self.next_transfer_id.fetch_add(1, Relaxed)
    }

    fn next_object_id_pair(&self) -> (u64, u64) {
        let manifest_object_id = self.next_object_id.fetch_add(2, Relaxed);
        (manifest_object_id, manifest_object_id + 1)
    }

    fn next_sequence_number(&self) -> u64 {
        self.next_sequence_number.fetch_add(1, Relaxed)
    }
}

#[derive(Clone, Default)]
struct FileTransferCancelState {
    inner: Arc<Mutex<FileTransferCancelStateInner>>,
}

#[derive(Default)]
struct FileTransferCancelStateInner {
    transfer_ids: HashSet<u64>,
    group_ids: HashSet<u64>,
}

impl FileTransferCancelState {
    fn cancel_transfer(&self, transfer_id: u64) {
        self.inner
            .lock()
            .expect("file transfer cancel state lock should not be poisoned")
            .transfer_ids
            .insert(transfer_id);
    }

    fn cancel_group(&self, group_id: u64) {
        self.inner
            .lock()
            .expect("file transfer cancel state lock should not be poisoned")
            .group_ids
            .insert(group_id);
    }

    fn is_transfer_cancelled(&self, transfer_id: u64) -> bool {
        self.inner
            .lock()
            .expect("file transfer cancel state lock should not be poisoned")
            .transfer_ids
            .contains(&transfer_id)
    }

    fn is_group_cancelled(&self, group_id: u64) -> bool {
        self.inner
            .lock()
            .expect("file transfer cancel state lock should not be poisoned")
            .group_ids
            .contains(&group_id)
    }
}

pub async fn run_file_transfer_runtime(
    data_sender: ScheduledDataSender,
    mut command_rx: mpsc::Receiver<FileTransferCommand>,
    mut inbound_rx: mpsc::Receiver<DataEnvelope>,
    event_tx: mpsc::UnboundedSender<FileTransferEvent>,
    mut cancel_rx: broadcast::Receiver<()>,
    config: FileTransferRuntimeConfig,
) -> Result<(), FileTransferRuntimeError> {
    let allocator = FileTransferIdAllocator::new(&config);
    let cancel_state = FileTransferCancelState::default();
    let mut inbound = InboundFileTransfers::new(config.receive_dir.clone(), config.receive_policy);

    loop {
        tokio::select! {
            _ = cancel_rx.recv() => {
                return Ok(());
            }
            maybe_command = command_rx.recv() => {
                let Some(command) = maybe_command else {
                    return Ok(());
                };
                match command {
                    FileTransferCommand::SendFile { path, mime_type } => {
                        let spec = next_file_transfer_spec(&config, &allocator);
                        SendFileTask {
                            path,
                            mime_type,
                            spec,
                            data_sender: data_sender.clone(),
                            event_tx: event_tx.clone(),
                            cancel_rx: cancel_rx.resubscribe(),
                            send_policy: config.send_policy,
                            allocator: allocator.clone(),
                            cancel_state: cancel_state.clone(),
                            stream_id: config.stream_id,
                        }
                        .spawn();
                    }
                    FileTransferCommand::SendFileGroup { files } => {
                        let group_id = allocator.next_group_id();
                        SendFileGroupTask {
                            group_id,
                            files,
                            config: config.clone(),
                            data_sender: data_sender.clone(),
                            event_tx: event_tx.clone(),
                            cancel_rx: cancel_rx.resubscribe(),
                            allocator: allocator.clone(),
                            cancel_state: cancel_state.clone(),
                        }
                        .spawn();
                    }
                    FileTransferCommand::CancelTransfer { transfer_id } => {
                        cancel_state.cancel_transfer(transfer_id);
                    }
                    FileTransferCommand::CancelGroup { group_id } => {
                        cancel_state.cancel_group(group_id);
                    }
                    FileTransferCommand::SetReceiveConfig {
                        receive_dir,
                        receive_policy,
                    } => {
                        inbound.set_receive_config(receive_dir, receive_policy);
                    }
                }
            }
            maybe_envelope = inbound_rx.recv() => {
                let Some(envelope) = maybe_envelope else {
                    return Ok(());
                };
                inbound.handle_envelope(envelope, &event_tx).await;
            }
        }
    }
}

fn next_file_transfer_spec(
    config: &FileTransferRuntimeConfig,
    allocator: &FileTransferIdAllocator,
) -> FileTransferSpec {
    let transfer_id = allocator.next_transfer_id();
    let (manifest_object_id, file_object_id) = allocator.next_object_id_pair();
    FileTransferSpec {
        transfer_id,
        manifest_object_id,
        file_object_id,
        stream_id: config.stream_id,
        first_sequence_number: 0,
        timestamp_ms: now_ms(),
        chunk_payload_len: config.chunk_payload_len,
        group: None,
    }
}

struct SendFileTask {
    path: PathBuf,
    mime_type: Option<String>,
    spec: FileTransferSpec,
    data_sender: ScheduledDataSender,
    event_tx: mpsc::UnboundedSender<FileTransferEvent>,
    cancel_rx: broadcast::Receiver<()>,
    send_policy: FileTransferPolicy,
    allocator: FileTransferIdAllocator,
    cancel_state: FileTransferCancelState,
    stream_id: u32,
}

struct SendFileGroupTask {
    group_id: u64,
    files: Vec<FileTransferGroupFile>,
    config: FileTransferRuntimeConfig,
    data_sender: ScheduledDataSender,
    event_tx: mpsc::UnboundedSender<FileTransferEvent>,
    cancel_rx: broadcast::Receiver<()>,
    allocator: FileTransferIdAllocator,
    cancel_state: FileTransferCancelState,
}

struct PreparedGroupFile {
    path: PathBuf,
    mime_type: Option<String>,
    relative_path: String,
}

impl SendFileGroupTask {
    fn spawn(self) {
        tokio::spawn(async move {
            let group_id = self.group_id;
            let event_tx = self.event_tx.clone();
            if let Err(err) = self.run().await {
                emit_event(
                    &event_tx,
                    FileTransferEvent::Error {
                        transfer_id: Some(group_id),
                        message: err.to_string(),
                    },
                );
            }
        });
    }

    async fn run(mut self) -> Result<(), FileTransferRuntimeError> {
        if self.files.is_empty() {
            return Ok(());
        }

        let files = expand_group_files(&self.files).await?;
        if files.is_empty() {
            return Ok(());
        }

        let mut readers = Vec::with_capacity(files.len());
        for file in &files {
            let spec = next_file_transfer_spec(&self.config, &self.allocator);
            let reader = FileTransferReader::from_path(
                &file.path,
                spec,
                file.mime_type.clone(),
                self.config.send_policy,
            )
            .await?;
            readers.push(PreparedGroupReader {
                reader,
                relative_path: file.relative_path.clone(),
            });
        }

        let group_total_size_bytes = readers
            .iter()
            .map(|reader| reader.reader.manifest().size_bytes)
            .sum::<u64>();
        let group_checksum_crc32 = file_group_checksum_crc32(&readers);
        let file_count = u32::try_from(readers.len()).unwrap_or(u32::MAX);

        emit_event(
            &self.event_tx,
            FileTransferEvent::OutgoingGroupStarted {
                group_id: self.group_id,
                file_count,
                total_size_bytes: group_total_size_bytes,
            },
        );

        for (index, reader) in readers.into_iter().enumerate() {
            if self.cancel_state.is_group_cancelled(self.group_id) {
                send_file_control(
                    FileTransferControl::CancelGroup {
                        group_id: self.group_id,
                    },
                    self.config.stream_id,
                    &self.data_sender,
                    &self.allocator,
                )
                .await?;
                emit_event(
                    &self.event_tx,
                    FileTransferEvent::OutgoingGroupCancelled {
                        group_id: self.group_id,
                    },
                );
                return Ok(());
            }

            let mut file_reader = reader.reader;
            let file_index = u32::try_from(index).unwrap_or(u32::MAX);
            let relative_path = reader.relative_path;
            file_reader.set_group(FileTransferGroup {
                group_id: self.group_id,
                file_index,
                file_count,
                relative_path,
                group_total_size_bytes,
                group_checksum_crc32,
            });
            match send_reader(
                &mut file_reader,
                &self.data_sender,
                &self.event_tx,
                &mut self.cancel_rx,
                &self.allocator,
                &self.cancel_state,
                self.config.stream_id,
            )
            .await?
            {
                SendReaderOutcome::Completed => {}
                SendReaderOutcome::TransferCancelled => return Ok(()),
                SendReaderOutcome::GroupCancelled { group_id } => {
                    self.cancel_state.cancel_group(group_id);
                    emit_event(
                        &self.event_tx,
                        FileTransferEvent::OutgoingGroupCancelled { group_id },
                    );
                    return Ok(());
                }
            }
        }

        emit_event(
            &self.event_tx,
            FileTransferEvent::OutgoingGroupCompleted {
                group_id: self.group_id,
                file_count,
                total_size_bytes: group_total_size_bytes,
            },
        );
        Ok(())
    }
}

enum SendReaderOutcome {
    Completed,
    TransferCancelled,
    GroupCancelled { group_id: u64 },
}

struct PreparedGroupReader {
    reader: FileTransferReader,
    relative_path: String,
}

async fn expand_group_files(
    files: &[FileTransferGroupFile],
) -> Result<Vec<PreparedGroupFile>, FileTransferError> {
    let mut expanded = Vec::new();
    for file in files {
        let metadata = tokio::fs::symlink_metadata(&file.path).await?;
        if metadata.is_file() {
            expanded.push(PreparedGroupFile {
                path: file.path.clone(),
                mime_type: file.mime_type.clone(),
                relative_path: top_level_relative_path(&file.path)?,
            });
        } else if metadata.is_dir() {
            expand_group_directory(&file.path, &mut expanded).await?;
        } else {
            return Err(FileTransferError::NotAFile(file.path.clone()));
        }
    }

    expanded.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(expanded)
}

async fn expand_group_directory(
    root: &Path,
    expanded: &mut Vec<PreparedGroupFile>,
) -> Result<(), FileTransferError> {
    let root_name = top_level_relative_path(root)?;
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            entries.push(entry.path());
        }
        entries.sort();

        for path in entries.into_iter().rev() {
            let metadata = tokio::fs::symlink_metadata(&path).await?;
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                let child = path
                    .strip_prefix(root)
                    .map_err(|_| FileTransferError::InvalidFileName)?;
                expanded.push(PreparedGroupFile {
                    relative_path: nested_relative_path(&root_name, child)?,
                    path,
                    mime_type: None,
                });
            } else {
                return Err(FileTransferError::NotAFile(path));
            }
        }
    }

    Ok(())
}

fn top_level_relative_path(path: &Path) -> Result<String, FileTransferError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or(FileTransferError::InvalidFileName)?;
    Ok(name.to_string())
}

fn nested_relative_path(root_name: &str, child: &Path) -> Result<String, FileTransferError> {
    let mut parts = vec![root_name.to_string()];
    for component in child.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_str().ok_or(FileTransferError::InvalidFileName)?;
                if value.is_empty() {
                    return Err(FileTransferError::InvalidFileName);
                }
                parts.push(value.to_string());
            }
            _ => return Err(FileTransferError::InvalidFileName),
        }
    }
    Ok(parts.join("/"))
}

impl SendFileTask {
    fn spawn(self) {
        tokio::spawn(async move {
            let transfer_id = self.spec.transfer_id;
            let event_tx = self.event_tx.clone();
            if let Err(err) = self.run().await {
                emit_event(
                    &event_tx,
                    FileTransferEvent::Error {
                        transfer_id: Some(transfer_id),
                        message: err.to_string(),
                    },
                );
            }
        });
    }

    async fn run(mut self) -> Result<(), FileTransferRuntimeError> {
        let mut reader =
            FileTransferReader::from_path(&self.path, self.spec, self.mime_type, self.send_policy)
                .await?;
        send_reader(
            &mut reader,
            &self.data_sender,
            &self.event_tx,
            &mut self.cancel_rx,
            &self.allocator,
            &self.cancel_state,
            self.stream_id,
        )
        .await?;
        Ok(())
    }
}

async fn send_reader(
    reader: &mut FileTransferReader,
    data_sender: &ScheduledDataSender,
    event_tx: &mpsc::UnboundedSender<FileTransferEvent>,
    cancel_rx: &mut broadcast::Receiver<()>,
    allocator: &FileTransferIdAllocator,
    cancel_state: &FileTransferCancelState,
    stream_id: u32,
) -> Result<SendReaderOutcome, FileTransferRuntimeError> {
    let manifest = reader.manifest().clone();
    let total_chunks = reader.total_chunks();
    emit_event(
        event_tx,
        FileTransferEvent::OutgoingStarted {
            transfer_id: manifest.transfer_id,
            file_object_id: manifest.file_object_id,
            group: manifest.group.clone(),
            name: manifest.name.clone(),
            size_bytes: manifest.size_bytes,
            total_chunks,
        },
    );

    let mut sent_file_chunks = 0_u32;
    let mut sent_file_bytes = 0_u64;

    loop {
        if let Some(outcome) = handle_outgoing_cancel(
            &manifest,
            data_sender,
            event_tx,
            allocator,
            cancel_state,
            stream_id,
        )
        .await?
        {
            return Ok(outcome);
        }

        let maybe_envelope = tokio::select! {
            _ = cancel_rx.recv() => {
                return Ok(SendReaderOutcome::TransferCancelled);
            }
            result = reader.next_envelope() => {
                result?
            }
        };

        if let Some(outcome) = handle_outgoing_cancel(
            &manifest,
            data_sender,
            event_tx,
            allocator,
            cancel_state,
            stream_id,
        )
        .await?
        {
            return Ok(outcome);
        }

        let Some(mut envelope) = maybe_envelope else {
            emit_event(
                event_tx,
                FileTransferEvent::OutgoingCompleted {
                    transfer_id: manifest.transfer_id,
                    file_object_id: manifest.file_object_id,
                    group: manifest.group.clone(),
                },
            );
            return Ok(SendReaderOutcome::Completed);
        };

        assign_sequence_number(&mut envelope, allocator.next_sequence_number());
        if envelope.header.kind == ContentKind::FileChunk {
            sent_file_chunks = sent_file_chunks.saturating_add(1);
            sent_file_bytes = sent_file_bytes.saturating_add(envelope.payload.len() as u64);
        }

        data_sender.send(envelope).await?;

        if sent_file_chunks > 0 {
            emit_event(
                event_tx,
                FileTransferEvent::OutgoingProgress {
                    transfer_id: manifest.transfer_id,
                    file_object_id: manifest.file_object_id,
                    sent_chunks: sent_file_chunks,
                    total_chunks,
                    sent_bytes: sent_file_bytes,
                    total_size: manifest.size_bytes,
                },
            );
        }
    }
}

async fn handle_outgoing_cancel(
    manifest: &protocol::FileTransferManifest,
    data_sender: &ScheduledDataSender,
    event_tx: &mpsc::UnboundedSender<FileTransferEvent>,
    allocator: &FileTransferIdAllocator,
    cancel_state: &FileTransferCancelState,
    stream_id: u32,
) -> Result<Option<SendReaderOutcome>, FileTransferRuntimeError> {
    if let Some(group) = &manifest.group
        && (cancel_state.is_group_cancelled(group.group_id)
            || cancel_state.is_transfer_cancelled(manifest.transfer_id))
    {
        send_file_control(
            FileTransferControl::CancelGroup {
                group_id: group.group_id,
            },
            stream_id,
            data_sender,
            allocator,
        )
        .await?;
        emit_event(
            event_tx,
            FileTransferEvent::OutgoingCancelled {
                transfer_id: manifest.transfer_id,
                file_object_id: manifest.file_object_id,
                group: manifest.group.clone(),
            },
        );
        return Ok(Some(SendReaderOutcome::GroupCancelled {
            group_id: group.group_id,
        }));
    }

    if cancel_state.is_transfer_cancelled(manifest.transfer_id) {
        send_file_control(
            FileTransferControl::CancelTransfer {
                transfer_id: manifest.transfer_id,
                file_object_id: manifest.file_object_id,
            },
            stream_id,
            data_sender,
            allocator,
        )
        .await?;
        emit_event(
            event_tx,
            FileTransferEvent::OutgoingCancelled {
                transfer_id: manifest.transfer_id,
                file_object_id: manifest.file_object_id,
                group: manifest.group.clone(),
            },
        );
        return Ok(Some(SendReaderOutcome::TransferCancelled));
    }

    Ok(None)
}

async fn send_file_control(
    control: FileTransferControl,
    stream_id: u32,
    data_sender: &ScheduledDataSender,
    allocator: &FileTransferIdAllocator,
) -> Result<(), FileTransferRuntimeError> {
    let (object_id, _) = allocator.next_object_id_pair();
    let sequence_number = allocator.next_sequence_number();
    let envelope = control_to_envelope(&control, object_id, stream_id, sequence_number, now_ms())?;
    data_sender.send(envelope).await?;
    Ok(())
}

fn file_group_checksum_crc32(readers: &[PreparedGroupReader]) -> u32 {
    let mut bytes = Vec::new();
    for reader in readers {
        let manifest = reader.reader.manifest();
        bytes.extend_from_slice(reader.relative_path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&manifest.size_bytes.to_be_bytes());
        bytes.extend_from_slice(&manifest.checksum_crc32.to_be_bytes());
    }
    crate::data_plane::checksum_crc32(&bytes)
}

fn assign_sequence_number(envelope: &mut DataEnvelope, sequence_number: u64) {
    envelope.header.sequence_number = sequence_number;
    if let Some(reliability) = &mut envelope.header.reliability_info {
        reliability.ack_id = sequence_number;
    }
}

struct InboundFileTransfers {
    receive_dir: PathBuf,
    receive_policy: FileReceivePolicy,
    manifest_assemblers: HashMap<u64, FileManifestAssembler>,
    incoming_files: HashMap<u64, IncomingFileTransfer>,
    incoming_groups: HashMap<u64, IncomingFileGroup>,
    cancelled_file_objects: HashSet<u64>,
    cancelled_groups: HashSet<u64>,
    pending_receive_config: Option<(PathBuf, FileReceivePolicy)>,
}

impl InboundFileTransfers {
    fn new(receive_dir: PathBuf, receive_policy: FileReceivePolicy) -> Self {
        Self {
            receive_dir,
            receive_policy,
            manifest_assemblers: HashMap::new(),
            incoming_files: HashMap::new(),
            incoming_groups: HashMap::new(),
            cancelled_file_objects: HashSet::new(),
            cancelled_groups: HashSet::new(),
            pending_receive_config: None,
        }
    }

    fn set_receive_config(&mut self, receive_dir: PathBuf, receive_policy: FileReceivePolicy) {
        if self.has_active_incoming_work() {
            self.pending_receive_config = Some((receive_dir, receive_policy));
            return;
        }

        self.receive_dir = receive_dir;
        self.receive_policy = receive_policy;
    }

    fn has_active_incoming_work(&self) -> bool {
        !self.manifest_assemblers.is_empty()
            || !self.incoming_files.is_empty()
            || !self.incoming_groups.is_empty()
    }

    fn apply_pending_receive_config_if_idle(&mut self) {
        if self.has_active_incoming_work() {
            return;
        }

        if let Some((receive_dir, receive_policy)) = self.pending_receive_config.take() {
            self.receive_dir = receive_dir;
            self.receive_policy = receive_policy;
        }
    }

    async fn handle_envelope(
        &mut self,
        envelope: DataEnvelope,
        event_tx: &mpsc::UnboundedSender<FileTransferEvent>,
    ) {
        let result = match envelope.header.kind {
            ContentKind::FileManifest => self.handle_manifest(envelope, event_tx).await,
            ContentKind::FileChunk => self.handle_file_chunk(envelope, event_tx).await,
            ContentKind::FileControl => self.handle_control(envelope, event_tx).await,
            other => Err(FileTransferError::UnexpectedContentKind(other)),
        };

        if let Err(err) = result {
            emit_event(
                event_tx,
                FileTransferEvent::Error {
                    transfer_id: None,
                    message: err.to_string(),
                },
            );
        }

        self.apply_pending_receive_config_if_idle();
    }

    async fn handle_manifest(
        &mut self,
        envelope: DataEnvelope,
        event_tx: &mpsc::UnboundedSender<FileTransferEvent>,
    ) -> Result<(), FileTransferError> {
        let object_id = envelope
            .header
            .chunk
            .as_ref()
            .map(|chunk| chunk.object_id)
            .ok_or(FileTransferError::MissingChunkMetadata)?;

        let assembler = match self.manifest_assemblers.entry(object_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(FileManifestAssembler::from_first_chunk(&envelope)?)
            }
        };
        let progress = assembler.push_chunk(envelope)?;
        if !progress.is_complete {
            return Ok(());
        }

        let assembler = self
            .manifest_assemblers
            .remove(&object_id)
            .expect("complete manifest assembler should still be present");
        let manifest = assembler.finish(self.receive_policy)?;
        if self.is_manifest_cancelled(&manifest) {
            return Ok(());
        }
        let incoming =
            IncomingFileTransfer::start(manifest.clone(), &self.receive_dir, self.receive_policy)
                .await?;
        let path = incoming.path().to_path_buf();
        let file_object_id = manifest.file_object_id;

        emit_event(
            event_tx,
            FileTransferEvent::IncomingStarted {
                transfer_id: manifest.transfer_id,
                file_object_id,
                group: manifest.group.clone(),
                name: manifest.name.clone(),
                size_bytes: manifest.size_bytes,
                path,
            },
        );
        if incoming.progress().is_complete {
            let received = incoming.finish().await?;
            let group = received.manifest.group.clone();
            let path = received.path.clone();
            emit_event(
                event_tx,
                FileTransferEvent::IncomingCompleted {
                    transfer_id: received.manifest.transfer_id,
                    file_object_id: received.manifest.file_object_id,
                    group: group.clone(),
                    path: received.path,
                    size_bytes: received.manifest.size_bytes,
                },
            );
            self.record_group_completion(group, path, event_tx)?;
        } else {
            self.incoming_files.insert(file_object_id, incoming);
        }
        Ok(())
    }

    async fn handle_control(
        &mut self,
        envelope: DataEnvelope,
        event_tx: &mpsc::UnboundedSender<FileTransferEvent>,
    ) -> Result<(), FileTransferError> {
        match control_from_envelope(&envelope)? {
            FileTransferControl::CancelTransfer {
                transfer_id,
                file_object_id,
            } => {
                self.cancelled_file_objects.insert(file_object_id);
                self.cancel_incoming_file(transfer_id, file_object_id, event_tx)
                    .await?;
            }
            FileTransferControl::CancelGroup { group_id } => {
                self.cancelled_groups.insert(group_id);
                self.cancel_incoming_group(group_id, event_tx).await?;
            }
        }
        Ok(())
    }

    async fn handle_file_chunk(
        &mut self,
        envelope: DataEnvelope,
        event_tx: &mpsc::UnboundedSender<FileTransferEvent>,
    ) -> Result<(), FileTransferError> {
        let object_id = envelope
            .header
            .chunk
            .as_ref()
            .map(|chunk| chunk.object_id)
            .ok_or(FileTransferError::MissingChunkMetadata)?;
        let incoming =
            self.incoming_files
                .get_mut(&object_id)
                .ok_or(FileTransferError::InvalidChunk {
                    reason: "file chunk arrived before its manifest",
                })?;

        let progress = incoming.push_chunk(envelope).await?;
        emit_event(
            event_tx,
            FileTransferEvent::IncomingProgress {
                transfer_id: progress.transfer_id,
                file_object_id: progress.file_object_id,
                received_chunks: progress.received_chunks,
                total_chunks: progress.total_chunks,
                received_bytes: progress.received_bytes,
                total_size: progress.total_size,
            },
        );

        if progress.is_complete {
            let incoming = self
                .incoming_files
                .remove(&object_id)
                .expect("complete incoming transfer should still be present");
            let received = incoming.finish().await?;
            let group = received.manifest.group.clone();
            let path = received.path.clone();
            emit_event(
                event_tx,
                FileTransferEvent::IncomingCompleted {
                    transfer_id: received.manifest.transfer_id,
                    file_object_id: received.manifest.file_object_id,
                    group: group.clone(),
                    path: received.path,
                    size_bytes: received.manifest.size_bytes,
                },
            );
            self.record_group_completion(group, path, event_tx)?;
        }

        Ok(())
    }

    fn record_group_completion(
        &mut self,
        group: Option<FileTransferGroup>,
        path: PathBuf,
        event_tx: &mpsc::UnboundedSender<FileTransferEvent>,
    ) -> Result<(), FileTransferError> {
        let Some(group) = group else {
            return Ok(());
        };

        let publish_path = group_publish_path(&self.receive_dir, &group.relative_path)?;
        let entry = self
            .incoming_groups
            .entry(group.group_id)
            .or_insert_with(|| IncomingFileGroup::new(&group));
        entry.record(&group, path, publish_path)?;

        if entry.is_complete() {
            let completed = self
                .incoming_groups
                .remove(&group.group_id)
                .expect("complete incoming group should still be present");
            emit_event(
                event_tx,
                FileTransferEvent::IncomingGroupCompleted {
                    group_id: group.group_id,
                    paths: completed.paths(),
                    total_size_bytes: group.group_total_size_bytes,
                },
            );
        }

        Ok(())
    }

    fn is_manifest_cancelled(&self, manifest: &protocol::FileTransferManifest) -> bool {
        self.cancelled_file_objects
            .contains(&manifest.file_object_id)
            || manifest
                .group
                .as_ref()
                .is_some_and(|group| self.cancelled_groups.contains(&group.group_id))
    }

    async fn cancel_incoming_file(
        &mut self,
        transfer_id: u64,
        file_object_id: u64,
        event_tx: &mpsc::UnboundedSender<FileTransferEvent>,
    ) -> Result<(), FileTransferError> {
        let Some(incoming) = self.incoming_files.remove(&file_object_id) else {
            return Ok(());
        };
        let group = incoming.manifest().group.clone();
        let path = incoming.cancel().await?;
        cleanup_empty_parent_dirs(&path, &self.receive_dir).await?;
        emit_event(
            event_tx,
            FileTransferEvent::IncomingCancelled {
                transfer_id,
                file_object_id,
                group,
                path,
            },
        );
        Ok(())
    }

    async fn cancel_incoming_group(
        &mut self,
        group_id: u64,
        event_tx: &mpsc::UnboundedSender<FileTransferEvent>,
    ) -> Result<(), FileTransferError> {
        let mut file_object_ids = self
            .incoming_files
            .iter()
            .filter_map(|(file_object_id, incoming)| {
                incoming
                    .manifest()
                    .group
                    .as_ref()
                    .filter(|group| group.group_id == group_id)
                    .map(|_| *file_object_id)
            })
            .collect::<Vec<_>>();
        file_object_ids.sort_unstable();

        for file_object_id in file_object_ids {
            if let Some(incoming) = self.incoming_files.remove(&file_object_id) {
                let transfer_id = incoming.manifest().transfer_id;
                let group = incoming.manifest().group.clone();
                let path = incoming.cancel().await?;
                cleanup_empty_parent_dirs(&path, &self.receive_dir).await?;
                emit_event(
                    event_tx,
                    FileTransferEvent::IncomingCancelled {
                        transfer_id,
                        file_object_id,
                        group,
                        path,
                    },
                );
            }
        }

        if let Some(group) = self.incoming_groups.remove(&group_id) {
            for path in group.completed_paths() {
                remove_materialized_file(&path, &self.receive_dir).await?;
            }
        }

        emit_event(
            event_tx,
            FileTransferEvent::IncomingGroupCancelled { group_id },
        );
        Ok(())
    }
}

struct IncomingFileGroup {
    file_count: u32,
    completed_files: Vec<Option<PathBuf>>,
    publish_paths: Vec<PathBuf>,
    completed_count: u32,
}

impl IncomingFileGroup {
    fn new(group: &FileTransferGroup) -> Self {
        Self {
            file_count: group.file_count,
            completed_files: vec![None; group.file_count as usize],
            publish_paths: Vec::new(),
            completed_count: 0,
        }
    }

    fn record(
        &mut self,
        group: &FileTransferGroup,
        path: PathBuf,
        publish_path: PathBuf,
    ) -> Result<(), FileTransferError> {
        if group.file_count != self.file_count || group.file_index >= self.file_count {
            return Err(FileTransferError::InvalidChunk {
                reason: "file group completion metadata is inconsistent",
            });
        }
        let slot = &mut self.completed_files[group.file_index as usize];
        if slot.is_none() {
            self.completed_count = self.completed_count.saturating_add(1);
            if !self.publish_paths.contains(&publish_path) {
                self.publish_paths.push(publish_path);
            }
        }
        *slot = Some(path);
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.completed_count == self.file_count
    }

    fn paths(self) -> Vec<PathBuf> {
        self.publish_paths
    }

    fn completed_paths(self) -> Vec<PathBuf> {
        self.completed_files.into_iter().flatten().collect()
    }
}

async fn remove_materialized_file(
    path: &Path,
    receive_dir: &Path,
) -> Result<(), FileTransferError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(FileTransferError::Io(err)),
    }
    cleanup_empty_parent_dirs(path, receive_dir).await
}

async fn cleanup_empty_parent_dirs(
    path: &Path,
    receive_dir: &Path,
) -> Result<(), FileTransferError> {
    let mut parent = path.parent();
    while let Some(dir) = parent {
        if dir == receive_dir {
            break;
        }
        match tokio::fs::remove_dir(dir).await {
            Ok(()) => parent = dir.parent(),
            Err(err)
                if err.kind() == std::io::ErrorKind::NotFound
                    || err.kind() == std::io::ErrorKind::DirectoryNotEmpty =>
            {
                break;
            }
            Err(err) => return Err(FileTransferError::Io(err)),
        }
    }
    Ok(())
}

fn group_publish_path(
    receive_dir: &Path,
    relative_path: &str,
) -> Result<PathBuf, FileTransferError> {
    let mut components = Path::new(relative_path).components();
    let Some(Component::Normal(first)) = components.next() else {
        return Err(FileTransferError::InvalidFileName);
    };
    if components.any(|component| !matches!(component, Component::Normal(_))) {
        return Err(FileTransferError::InvalidFileName);
    }
    Ok(receive_dir.join(first))
}

fn emit_event(event_tx: &mpsc::UnboundedSender<FileTransferEvent>, event: FileTransferEvent) {
    let _ = event_tx.send(event);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_plane::LaneSchedulerConfig;
    use crate::media_plane::rtp_to_realtime_data;
    use crate::net::{MultiplexedPacket, UdpMultiplexer};
    use crate::scheduled_sender::{ScheduledDataSender, ScheduledDataSenderConfig};
    use protocol::{PayloadType, RtpHeader, RtpPacket};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::time::timeout;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ObservedDataPacket {
        Media(ContentKind),
        File(ContentKind),
        Other(ContentKind),
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{nanos}"))
    }

    fn media_packet(payload_type: PayloadType, sequence_number: u16, ssrc: u32) -> RtpPacket {
        let timestamp = if payload_type == PayloadType::VideoH265 {
            now_ms().saturating_add(1_000) as u32
        } else {
            sequence_number as u32 * 960
        };

        RtpPacket {
            header: RtpHeader {
                version: 2,
                payload_type: payload_type as u8,
                sequence_number,
                timestamp,
                ssrc,
            },
            payload: vec![sequence_number as u8; 64],
        }
    }

    async fn wait_until_file_sender_has_queued_reliable(sender: &ScheduledDataSender) {
        timeout(Duration::from_secs(2), async {
            loop {
                let stats = sender.stats();
                if stats.entrance_enqueued > 0
                    && stats.sent_realtime == 0
                    && stats.sent_reliable == 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("file transfer should queue reliable data before the first sender tick");
    }

    #[test]
    fn inbound_receive_config_applies_immediately_when_idle() {
        let mut inbound =
            InboundFileTransfers::new(PathBuf::from("/tmp/old"), FileReceivePolicy::default());
        let next_policy = FileReceivePolicy {
            allow_overwrite: true,
            ..FileReceivePolicy::default()
        };

        inbound.set_receive_config(PathBuf::from("/tmp/new"), next_policy);

        assert_eq!(inbound.receive_dir, PathBuf::from("/tmp/new"));
        assert_eq!(inbound.receive_policy, next_policy);
        assert_eq!(inbound.pending_receive_config, None);
    }

    #[test]
    fn inbound_receive_config_waits_for_active_group_boundary() {
        let mut inbound =
            InboundFileTransfers::new(PathBuf::from("/tmp/old"), FileReceivePolicy::default());
        let group = FileTransferGroup {
            group_id: 9,
            file_index: 0,
            file_count: 2,
            relative_path: "Folder/a.txt".to_string(),
            group_total_size_bytes: 10,
            group_checksum_crc32: 0,
        };
        inbound
            .incoming_groups
            .insert(group.group_id, IncomingFileGroup::new(&group));
        let next_policy = FileReceivePolicy {
            allow_overwrite: true,
            ..FileReceivePolicy::default()
        };

        inbound.set_receive_config(PathBuf::from("/tmp/new"), next_policy);

        assert_eq!(inbound.receive_dir, PathBuf::from("/tmp/old"));
        assert_eq!(inbound.receive_policy, FileReceivePolicy::default());
        assert_eq!(
            inbound.pending_receive_config,
            Some((PathBuf::from("/tmp/new"), next_policy))
        );

        inbound.incoming_groups.clear();
        inbound.apply_pending_receive_config_if_idle();

        assert_eq!(inbound.receive_dir, PathBuf::from("/tmp/new"));
        assert_eq!(inbound.receive_policy, next_policy);
        assert_eq!(inbound.pending_receive_config, None);
    }

    #[tokio::test]
    async fn runtime_sends_file_over_scheduled_udp_path_and_receiver_materializes_it() {
        let base = unique_temp_dir("remote-play-file-runtime");
        let source_dir = base.join("source");
        let receive_dir = base.join("received");
        tokio::fs::create_dir_all(&source_dir)
            .await
            .expect("source dir should be created");
        let source = source_dir.join("runtime.bin");
        let bytes: Vec<u8> = (0..33_000).map(|index| (index % 229) as u8).collect();
        tokio::fs::write(&source, &bytes)
            .await
            .expect("source file should be written");

        let source_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("source bind should succeed");
        let source_sender = source_mux.split().0;
        let target_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("target bind should succeed");
        let target_addr = target_mux.local_addr().expect("target addr should exist");
        let (target_udp_sender, target_udp_receiver) = target_mux.split();

        let (source_scheduled_sender, _source_worker) = ScheduledDataSender::spawn(
            source_sender,
            target_addr,
            ScheduledDataSenderConfig {
                tick_interval: Duration::from_millis(1),
                send_budget_per_tick: 8,
                ..ScheduledDataSenderConfig::default()
            },
        );
        let (target_scheduled_sender, _target_worker) = ScheduledDataSender::spawn(
            target_udp_sender,
            source_mux.local_addr().expect("source addr should exist"),
            ScheduledDataSenderConfig::default(),
        );

        let (source_command_tx, source_command_rx) = mpsc::channel(8);
        let (_source_inbound_tx, source_inbound_rx) = mpsc::channel(8);
        let (source_event_tx, mut source_event_rx) = mpsc::unbounded_channel();
        let (_target_command_tx, target_command_rx) = mpsc::channel(8);
        let (target_inbound_tx, target_inbound_rx) = mpsc::channel(128);
        let (target_event_tx, mut target_event_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _) = broadcast::channel(4);

        let source_runtime = tokio::spawn(run_file_transfer_runtime(
            source_scheduled_sender,
            source_command_rx,
            source_inbound_rx,
            source_event_tx,
            cancel_tx.subscribe(),
            FileTransferRuntimeConfig {
                chunk_payload_len: 4096,
                ..FileTransferRuntimeConfig::default()
            },
        ));
        let target_runtime = tokio::spawn(run_file_transfer_runtime(
            target_scheduled_sender,
            target_command_rx,
            target_inbound_rx,
            target_event_tx,
            cancel_tx.subscribe(),
            FileTransferRuntimeConfig {
                chunk_payload_len: 4096,
                receive_dir: receive_dir.clone(),
                receive_policy: FileReceivePolicy {
                    allow_overwrite: true,
                    ..FileReceivePolicy::default()
                },
                ..FileTransferRuntimeConfig::default()
            },
        ));

        let udp_router = tokio::spawn(async move {
            loop {
                let packet = match target_udp_receiver.recv().await {
                    Ok(packet) => packet,
                    Err(_) => break,
                };
                if let MultiplexedPacket::Data(envelope, _) = packet
                    && target_inbound_tx.send(envelope).await.is_err()
                {
                    break;
                }
            }
        });

        source_command_tx
            .send(FileTransferCommand::SendFile {
                path: source.clone(),
                mime_type: Some("application/octet-stream".to_string()),
            })
            .await
            .expect("send command should queue");

        let mut outgoing_completed = false;
        while !outgoing_completed {
            let event = timeout(Duration::from_secs(2), source_event_rx.recv())
                .await
                .expect("source event should arrive")
                .expect("source event channel should remain open");
            if matches!(event, FileTransferEvent::OutgoingCompleted { .. }) {
                outgoing_completed = true;
            }
        }

        let received_path = loop {
            let event = timeout(Duration::from_secs(2), target_event_rx.recv())
                .await
                .expect("target event should arrive")
                .expect("target event channel should remain open");
            if let FileTransferEvent::IncomingCompleted { path, .. } = event {
                break path;
            }
        };

        let received_bytes = tokio::fs::read(&received_path)
            .await
            .expect("received file should read");
        assert_eq!(received_bytes, bytes);

        let _ = cancel_tx.send(());
        udp_router.abort();
        source_runtime
            .await
            .expect("source runtime should join")
            .expect("source runtime should stop cleanly");
        target_runtime
            .await
            .expect("target runtime should join")
            .expect("target runtime should stop cleanly");
        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn shared_sender_prioritizes_media_while_file_transfer_completes() {
        let base = unique_temp_dir("remote-play-file-runtime-media-mix");
        let source_dir = base.join("source");
        let receive_dir = base.join("received");
        tokio::fs::create_dir_all(&source_dir)
            .await
            .expect("source dir should be created");
        let source = source_dir.join("mixed.bin");
        let bytes: Vec<u8> = (0..4096).map(|index| (index % 251) as u8).collect();
        tokio::fs::write(&source, &bytes)
            .await
            .expect("source file should be written");

        let source_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("source bind should succeed");
        let source_sender = source_mux.split().0;
        let target_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("target bind should succeed");
        let target_addr = target_mux.local_addr().expect("target addr should exist");
        let (target_udp_sender, target_udp_receiver) = target_mux.split();

        let (source_scheduled_sender, _source_worker) = ScheduledDataSender::spawn(
            source_sender,
            target_addr,
            ScheduledDataSenderConfig {
                scheduler: LaneSchedulerConfig {
                    max_realtime_queued: 8,
                    max_reliable_queued: 64,
                },
                queue_capacity: 128,
                send_budget_per_tick: 1,
                tick_interval: Duration::from_millis(250),
            },
        );
        let (target_scheduled_sender, _target_worker) = ScheduledDataSender::spawn(
            target_udp_sender,
            source_mux.local_addr().expect("source addr should exist"),
            ScheduledDataSenderConfig::default(),
        );

        let (source_command_tx, source_command_rx) = mpsc::channel(8);
        let (_source_inbound_tx, source_inbound_rx) = mpsc::channel(8);
        let (source_event_tx, mut source_event_rx) = mpsc::unbounded_channel();
        let (_target_command_tx, target_command_rx) = mpsc::channel(8);
        let (target_inbound_tx, target_inbound_rx) = mpsc::channel(128);
        let (target_event_tx, mut target_event_rx) = mpsc::unbounded_channel();
        let (observed_tx, mut observed_rx) = mpsc::unbounded_channel::<ObservedDataPacket>();
        let (cancel_tx, _) = broadcast::channel(4);

        let source_runtime = tokio::spawn(run_file_transfer_runtime(
            source_scheduled_sender.clone(),
            source_command_rx,
            source_inbound_rx,
            source_event_tx,
            cancel_tx.subscribe(),
            FileTransferRuntimeConfig {
                chunk_payload_len: 1024,
                ..FileTransferRuntimeConfig::default()
            },
        ));
        let target_runtime = tokio::spawn(run_file_transfer_runtime(
            target_scheduled_sender,
            target_command_rx,
            target_inbound_rx,
            target_event_tx,
            cancel_tx.subscribe(),
            FileTransferRuntimeConfig {
                chunk_payload_len: 1024,
                receive_dir: receive_dir.clone(),
                receive_policy: FileReceivePolicy {
                    allow_overwrite: true,
                    ..FileReceivePolicy::default()
                },
                ..FileTransferRuntimeConfig::default()
            },
        ));

        let udp_router = tokio::spawn(async move {
            loop {
                let packet = match target_udp_receiver.recv().await {
                    Ok(packet) => packet,
                    Err(_) => break,
                };
                let MultiplexedPacket::Data(envelope, _) = packet else {
                    continue;
                };

                let kind = envelope.header.kind;
                let observed = if matches!(
                    envelope.header.lane,
                    protocol::DataLane::RealtimeVideo | protocol::DataLane::RealtimeAudio
                ) {
                    ObservedDataPacket::Media(kind)
                } else if matches!(
                    kind,
                    ContentKind::FileManifest | ContentKind::FileChunk | ContentKind::FileControl
                ) {
                    ObservedDataPacket::File(kind)
                } else {
                    ObservedDataPacket::Other(kind)
                };
                let _ = observed_tx.send(observed);

                if matches!(
                    kind,
                    ContentKind::FileManifest | ContentKind::FileChunk | ContentKind::FileControl
                ) && target_inbound_tx.send(envelope).await.is_err()
                {
                    break;
                }
            }
        });

        source_command_tx
            .send(FileTransferCommand::SendFile {
                path: source.clone(),
                mime_type: Some("application/octet-stream".to_string()),
            })
            .await
            .expect("send command should queue");
        wait_until_file_sender_has_queued_reliable(&source_scheduled_sender).await;

        let video = media_packet(PayloadType::VideoH265, 77, 900);
        let audio = media_packet(PayloadType::AudioOpus, 78, 901);
        source_scheduled_sender
            .try_send(rtp_to_realtime_data(&audio).expect("audio should adapt"))
            .expect("audio should queue in the shared sender");
        source_scheduled_sender
            .try_send(rtp_to_realtime_data(&video).expect("video should adapt"))
            .expect("video should queue in the shared sender");

        let first = timeout(Duration::from_secs(2), observed_rx.recv())
            .await
            .expect("first packet should arrive")
            .expect("observed channel should remain open");
        let second = timeout(Duration::from_secs(2), observed_rx.recv())
            .await
            .expect("second packet should arrive")
            .expect("observed channel should remain open");

        assert_eq!(first, ObservedDataPacket::Media(ContentKind::VideoH265));
        assert_eq!(second, ObservedDataPacket::Media(ContentKind::AudioOpus));

        let mut outgoing_completed = false;
        while !outgoing_completed {
            let event = timeout(Duration::from_secs(5), source_event_rx.recv())
                .await
                .expect("source event should arrive")
                .expect("source event channel should remain open");
            if matches!(event, FileTransferEvent::OutgoingCompleted { .. }) {
                outgoing_completed = true;
            }
        }

        let received_path = loop {
            let event = timeout(Duration::from_secs(5), target_event_rx.recv())
                .await
                .expect("target event should arrive")
                .expect("target event channel should remain open");
            if let FileTransferEvent::IncomingCompleted { path, .. } = event {
                break path;
            }
        };
        let received_bytes = tokio::fs::read(&received_path)
            .await
            .expect("received file should read");
        assert_eq!(received_bytes, bytes);

        let stats = source_scheduled_sender.stats();
        assert_eq!(stats.sent_realtime, 2);
        assert!(stats.sent_reliable >= 2);
        assert_eq!(stats.scheduler_dropped_stale_realtime, 0);
        assert_eq!(stats.scheduler_dropped_realtime_capacity, 0);
        assert_eq!(stats.send_errors, 0);

        let _ = cancel_tx.send(());
        udp_router.abort();
        source_runtime
            .await
            .expect("source runtime should join")
            .expect("source runtime should stop cleanly");
        target_runtime
            .await
            .expect("target runtime should join")
            .expect("target runtime should stop cleanly");
        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn runtime_publishes_group_completion_after_all_files_materialize() {
        let base = unique_temp_dir("remote-play-file-runtime-group");
        let source_dir = base.join("source");
        let receive_dir = base.join("received");
        tokio::fs::create_dir_all(&source_dir)
            .await
            .expect("source dir should be created");
        let first = source_dir.join("first.txt");
        let second = source_dir.join("second.txt");
        tokio::fs::write(&first, b"first")
            .await
            .expect("first file should write");
        tokio::fs::write(&second, b"second file")
            .await
            .expect("second file should write");

        let source_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("source bind should succeed");
        let source_sender = source_mux.split().0;
        let target_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("target bind should succeed");
        let target_addr = target_mux.local_addr().expect("target addr should exist");
        let (target_udp_sender, target_udp_receiver) = target_mux.split();

        let (source_scheduled_sender, _source_worker) = ScheduledDataSender::spawn(
            source_sender,
            target_addr,
            ScheduledDataSenderConfig {
                tick_interval: Duration::from_millis(1),
                send_budget_per_tick: 8,
                ..ScheduledDataSenderConfig::default()
            },
        );
        let (target_scheduled_sender, _target_worker) = ScheduledDataSender::spawn(
            target_udp_sender,
            source_mux.local_addr().expect("source addr should exist"),
            ScheduledDataSenderConfig::default(),
        );

        let (source_command_tx, source_command_rx) = mpsc::channel(8);
        let (_source_inbound_tx, source_inbound_rx) = mpsc::channel(8);
        let (source_event_tx, mut source_event_rx) = mpsc::unbounded_channel();
        let (_target_command_tx, target_command_rx) = mpsc::channel(8);
        let (target_inbound_tx, target_inbound_rx) = mpsc::channel(128);
        let (target_event_tx, mut target_event_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _) = broadcast::channel(4);

        let source_runtime = tokio::spawn(run_file_transfer_runtime(
            source_scheduled_sender,
            source_command_rx,
            source_inbound_rx,
            source_event_tx,
            cancel_tx.subscribe(),
            FileTransferRuntimeConfig {
                chunk_payload_len: 4,
                ..FileTransferRuntimeConfig::default()
            },
        ));
        let target_runtime = tokio::spawn(run_file_transfer_runtime(
            target_scheduled_sender,
            target_command_rx,
            target_inbound_rx,
            target_event_tx,
            cancel_tx.subscribe(),
            FileTransferRuntimeConfig {
                chunk_payload_len: 4,
                receive_dir: receive_dir.clone(),
                receive_policy: FileReceivePolicy {
                    allow_overwrite: true,
                    ..FileReceivePolicy::default()
                },
                ..FileTransferRuntimeConfig::default()
            },
        ));

        let udp_router = tokio::spawn(async move {
            loop {
                let packet = match target_udp_receiver.recv().await {
                    Ok(packet) => packet,
                    Err(_) => break,
                };
                if let MultiplexedPacket::Data(envelope, _) = packet
                    && target_inbound_tx.send(envelope).await.is_err()
                {
                    break;
                }
            }
        });

        source_command_tx
            .send(FileTransferCommand::SendFileGroup {
                files: vec![
                    FileTransferGroupFile {
                        path: first.clone(),
                        mime_type: Some("text/plain".to_string()),
                    },
                    FileTransferGroupFile {
                        path: second.clone(),
                        mime_type: Some("text/plain".to_string()),
                    },
                ],
            })
            .await
            .expect("send group command should queue");

        let mut outgoing_group_completed = false;
        while !outgoing_group_completed {
            let event = timeout(Duration::from_secs(2), source_event_rx.recv())
                .await
                .expect("source event should arrive")
                .expect("source event channel should remain open");
            if matches!(event, FileTransferEvent::OutgoingGroupCompleted { .. }) {
                outgoing_group_completed = true;
            }
        }

        let group_paths = loop {
            let event = timeout(Duration::from_secs(2), target_event_rx.recv())
                .await
                .expect("target event should arrive")
                .expect("target event channel should remain open");
            if let FileTransferEvent::IncomingGroupCompleted { paths, .. } = event {
                break paths;
            }
        };

        assert_eq!(group_paths.len(), 2);
        assert_eq!(
            tokio::fs::read(&group_paths[0])
                .await
                .expect("first received file should read"),
            b"first"
        );
        assert_eq!(
            tokio::fs::read(&group_paths[1])
                .await
                .expect("second received file should read"),
            b"second file"
        );

        let _ = cancel_tx.send(());
        udp_router.abort();
        source_runtime
            .await
            .expect("source runtime should join")
            .expect("source runtime should stop cleanly");
        target_runtime
            .await
            .expect("target runtime should join")
            .expect("target runtime should stop cleanly");
        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn runtime_expands_directory_groups_and_publishes_top_level_path() {
        let base = unique_temp_dir("remote-play-file-runtime-directory-group");
        let source_dir = base.join("source");
        let receive_dir = base.join("received");
        let project_dir = source_dir.join("Project");
        let nested_dir = project_dir.join("nested");
        tokio::fs::create_dir_all(&nested_dir)
            .await
            .expect("nested source dir should be created");
        tokio::fs::write(project_dir.join("root.txt"), b"root file")
            .await
            .expect("root file should write");
        tokio::fs::write(nested_dir.join("child.txt"), b"child file")
            .await
            .expect("child file should write");

        let source_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("source bind should succeed");
        let source_sender = source_mux.split().0;
        let target_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("target bind should succeed");
        let target_addr = target_mux.local_addr().expect("target addr should exist");
        let (target_udp_sender, target_udp_receiver) = target_mux.split();

        let (source_scheduled_sender, _source_worker) = ScheduledDataSender::spawn(
            source_sender,
            target_addr,
            ScheduledDataSenderConfig {
                tick_interval: Duration::from_millis(1),
                send_budget_per_tick: 8,
                ..ScheduledDataSenderConfig::default()
            },
        );
        let (target_scheduled_sender, _target_worker) = ScheduledDataSender::spawn(
            target_udp_sender,
            source_mux.local_addr().expect("source addr should exist"),
            ScheduledDataSenderConfig::default(),
        );

        let (source_command_tx, source_command_rx) = mpsc::channel(8);
        let (_source_inbound_tx, source_inbound_rx) = mpsc::channel(8);
        let (source_event_tx, mut source_event_rx) = mpsc::unbounded_channel();
        let (_target_command_tx, target_command_rx) = mpsc::channel(8);
        let (target_inbound_tx, target_inbound_rx) = mpsc::channel(128);
        let (target_event_tx, mut target_event_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _) = broadcast::channel(4);

        let source_runtime = tokio::spawn(run_file_transfer_runtime(
            source_scheduled_sender,
            source_command_rx,
            source_inbound_rx,
            source_event_tx,
            cancel_tx.subscribe(),
            FileTransferRuntimeConfig {
                chunk_payload_len: 4,
                ..FileTransferRuntimeConfig::default()
            },
        ));
        let target_runtime = tokio::spawn(run_file_transfer_runtime(
            target_scheduled_sender,
            target_command_rx,
            target_inbound_rx,
            target_event_tx,
            cancel_tx.subscribe(),
            FileTransferRuntimeConfig {
                chunk_payload_len: 4,
                receive_dir: receive_dir.clone(),
                receive_policy: FileReceivePolicy {
                    allow_overwrite: true,
                    ..FileReceivePolicy::default()
                },
                ..FileTransferRuntimeConfig::default()
            },
        ));

        let udp_router = tokio::spawn(async move {
            loop {
                let packet = match target_udp_receiver.recv().await {
                    Ok(packet) => packet,
                    Err(_) => break,
                };
                if let MultiplexedPacket::Data(envelope, _) = packet
                    && target_inbound_tx.send(envelope).await.is_err()
                {
                    break;
                }
            }
        });

        source_command_tx
            .send(FileTransferCommand::SendFileGroup {
                files: vec![FileTransferGroupFile {
                    path: project_dir.clone(),
                    mime_type: None,
                }],
            })
            .await
            .expect("send directory group command should queue");

        let mut outgoing_file_count = None;
        while outgoing_file_count.is_none() {
            let event = timeout(Duration::from_secs(2), source_event_rx.recv())
                .await
                .expect("source event should arrive")
                .expect("source event channel should remain open");
            if let FileTransferEvent::OutgoingGroupStarted { file_count, .. } = event {
                outgoing_file_count = Some(file_count);
            }
        }
        assert_eq!(outgoing_file_count, Some(2));

        let group_paths = loop {
            let event = timeout(Duration::from_secs(2), target_event_rx.recv())
                .await
                .expect("target event should arrive")
                .expect("target event channel should remain open");
            if let FileTransferEvent::IncomingGroupCompleted { paths, .. } = event {
                break paths;
            }
        };

        assert_eq!(group_paths, vec![receive_dir.join("Project")]);
        assert_eq!(
            tokio::fs::read(receive_dir.join("Project/root.txt"))
                .await
                .expect("received root file should read"),
            b"root file"
        );
        assert_eq!(
            tokio::fs::read(receive_dir.join("Project/nested/child.txt"))
                .await
                .expect("received nested file should read"),
            b"child file"
        );

        let _ = cancel_tx.send(());
        udp_router.abort();
        source_runtime
            .await
            .expect("source runtime should join")
            .expect("source runtime should stop cleanly");
        target_runtime
            .await
            .expect("target runtime should join")
            .expect("target runtime should stop cleanly");
        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn runtime_cleans_partial_file_on_remote_cancel_transfer() {
        let base = unique_temp_dir("remote-play-file-runtime-cancel-file");
        let receive_dir = base.join("received");
        let mut inbound = InboundFileTransfers::new(
            receive_dir.clone(),
            FileReceivePolicy {
                allow_overwrite: true,
                ..FileReceivePolicy::default()
            },
        );
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let manifest = protocol::FileTransferManifest {
            transfer_id: 55,
            file_object_id: 56,
            name: "cancel.bin".to_string(),
            group: None,
            mime_type: None,
            size_bytes: 1024,
            chunk_payload_len: 512,
            checksum_crc32: 0,
        };
        let manifest_envelope =
            crate::file_transfer::manifest_to_envelope(&manifest, 54, 2, 1, 1_000)
                .expect("manifest should encode");
        inbound.handle_envelope(manifest_envelope, &event_tx).await;

        let partial_path = match event_rx.recv().await.expect("incoming started should emit") {
            FileTransferEvent::IncomingStarted { path, .. } => path,
            event => panic!("unexpected event: {event:?}"),
        };
        assert!(
            tokio::fs::metadata(&partial_path).await.is_ok(),
            "partial file should exist before cancellation"
        );

        let cancel = FileTransferControl::CancelTransfer {
            transfer_id: manifest.transfer_id,
            file_object_id: manifest.file_object_id,
        };
        let cancel_envelope =
            control_to_envelope(&cancel, 90, 2, 2, 1_001).expect("cancel control should encode");
        inbound.handle_envelope(cancel_envelope, &event_tx).await;

        match event_rx.recv().await.expect("incoming cancel should emit") {
            FileTransferEvent::IncomingCancelled {
                transfer_id,
                file_object_id,
                path,
                ..
            } => {
                assert_eq!(transfer_id, manifest.transfer_id);
                assert_eq!(file_object_id, manifest.file_object_id);
                assert_eq!(path, partial_path);
            }
            event => panic!("unexpected event: {event:?}"),
        }
        assert!(
            tokio::fs::metadata(&partial_path).await.is_err(),
            "partial file should be removed after cancellation"
        );

        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn outgoing_cancel_transfer_sends_remote_control_and_stops_reader() {
        let base = unique_temp_dir("remote-play-file-runtime-outgoing-cancel");
        tokio::fs::create_dir_all(&base)
            .await
            .expect("temp dir should be created");
        let source = base.join("cancel-source.bin");
        tokio::fs::write(&source, vec![7_u8; 8192])
            .await
            .expect("source file should write");

        let mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("sender bind should succeed");
        let sender = mux.split().0;
        let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("receiver bind should succeed");
        let receiver_addr = receiver_mux
            .local_addr()
            .expect("receiver addr should exist");
        let receiver = receiver_mux.split().1;
        let (scheduled_sender, _worker) = ScheduledDataSender::spawn(
            sender,
            receiver_addr,
            ScheduledDataSenderConfig {
                tick_interval: Duration::from_millis(1),
                send_budget_per_tick: 4,
                ..ScheduledDataSenderConfig::default()
            },
        );
        let config = FileTransferRuntimeConfig {
            chunk_payload_len: 1024,
            ..FileTransferRuntimeConfig::default()
        };
        let allocator = FileTransferIdAllocator::new(&config);
        let spec = FileTransferSpec {
            chunk_payload_len: 1024,
            ..FileTransferSpec::new(7, 11, 12, config.stream_id, 0, 1_000)
        };
        let mut reader = FileTransferReader::from_path(
            &source,
            spec,
            Some("application/octet-stream".to_string()),
            FileTransferPolicy::default(),
        )
        .await
        .expect("reader should create");
        let cancel_state = FileTransferCancelState::default();
        cancel_state.cancel_transfer(7);
        let (_shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let mut shutdown_rx = shutdown_rx;
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let outcome = send_reader(
            &mut reader,
            &scheduled_sender,
            &event_tx,
            &mut shutdown_rx,
            &allocator,
            &cancel_state,
            config.stream_id,
        )
        .await
        .expect("send reader should stop cleanly");
        assert!(matches!(outcome, SendReaderOutcome::TransferCancelled));

        assert!(matches!(
            event_rx.recv().await.expect("start event should emit"),
            FileTransferEvent::OutgoingStarted { transfer_id: 7, .. }
        ));
        assert!(matches!(
            event_rx.recv().await.expect("cancel event should emit"),
            FileTransferEvent::OutgoingCancelled {
                transfer_id: 7,
                file_object_id: 12,
                ..
            }
        ));

        let packet = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("cancel control should arrive")
            .expect("cancel control packet should decode");
        let MultiplexedPacket::Data(envelope, _) = packet else {
            panic!("expected data packet");
        };
        assert_eq!(
            control_from_envelope(&envelope).expect("cancel control should decode"),
            FileTransferControl::CancelTransfer {
                transfer_id: 7,
                file_object_id: 12
            }
        );

        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn runtime_cancel_group_removes_completed_and_partial_files_without_publish() {
        let base = unique_temp_dir("remote-play-file-runtime-cancel-group");
        let receive_dir = base.join("received");
        let mut inbound = InboundFileTransfers::new(
            receive_dir.clone(),
            FileReceivePolicy {
                allow_overwrite: true,
                ..FileReceivePolicy::default()
            },
        );
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let group_id = 77;

        let first_manifest = protocol::FileTransferManifest {
            transfer_id: 101,
            file_object_id: 102,
            name: "first.txt".to_string(),
            group: Some(FileTransferGroup {
                group_id,
                file_index: 0,
                file_count: 2,
                relative_path: "Folder/first.txt".to_string(),
                group_total_size_bytes: 4,
                group_checksum_crc32: 0,
            }),
            mime_type: Some("text/plain".to_string()),
            size_bytes: 0,
            chunk_payload_len: 512,
            checksum_crc32: crate::data_plane::checksum_crc32(&[]),
        };
        inbound
            .handle_envelope(
                crate::file_transfer::manifest_to_envelope(&first_manifest, 100, 2, 1, 1_000)
                    .expect("first manifest should encode"),
                &event_tx,
            )
            .await;
        let _ = event_rx.recv().await.expect("first start should emit");
        let first_path = match event_rx.recv().await.expect("first complete should emit") {
            FileTransferEvent::IncomingCompleted { path, .. } => path,
            event => panic!("unexpected event: {event:?}"),
        };
        assert!(
            tokio::fs::metadata(&first_path).await.is_ok(),
            "completed group file should exist before group cancellation"
        );

        let second_manifest = protocol::FileTransferManifest {
            transfer_id: 103,
            file_object_id: 104,
            name: "second.txt".to_string(),
            group: Some(FileTransferGroup {
                group_id,
                file_index: 1,
                file_count: 2,
                relative_path: "Folder/second.txt".to_string(),
                group_total_size_bytes: 4,
                group_checksum_crc32: 0,
            }),
            mime_type: Some("text/plain".to_string()),
            size_bytes: 4,
            chunk_payload_len: 512,
            checksum_crc32: crate::data_plane::checksum_crc32(b"data"),
        };
        inbound
            .handle_envelope(
                crate::file_transfer::manifest_to_envelope(&second_manifest, 101, 2, 2, 1_001)
                    .expect("second manifest should encode"),
                &event_tx,
            )
            .await;
        let second_path = match event_rx.recv().await.expect("second start should emit") {
            FileTransferEvent::IncomingStarted { path, .. } => path,
            event => panic!("unexpected event: {event:?}"),
        };
        assert!(
            tokio::fs::metadata(&second_path).await.is_ok(),
            "partial group file should exist before group cancellation"
        );

        inbound
            .handle_envelope(
                control_to_envelope(
                    &FileTransferControl::CancelGroup { group_id },
                    200,
                    2,
                    3,
                    1_002,
                )
                .expect("group cancel should encode"),
                &event_tx,
            )
            .await;

        assert!(matches!(
            event_rx
                .recv()
                .await
                .expect("file cancellation should emit"),
            FileTransferEvent::IncomingCancelled {
                file_object_id: 104,
                ..
            }
        ));
        assert!(matches!(
            event_rx
                .recv()
                .await
                .expect("group cancellation should emit"),
            FileTransferEvent::IncomingGroupCancelled { group_id: 77 }
        ));
        assert!(
            tokio::fs::metadata(&first_path).await.is_err(),
            "completed group file should be removed after group cancellation"
        );
        assert!(
            tokio::fs::metadata(&second_path).await.is_err(),
            "partial group file should be removed after group cancellation"
        );
        assert!(
            timeout(Duration::from_millis(20), event_rx.recv())
                .await
                .is_err(),
            "cancelled group must not emit IncomingGroupCompleted"
        );

        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn runtime_completes_zero_byte_file_after_manifest() {
        let base = unique_temp_dir("remote-play-file-runtime-empty");
        let receive_dir = base.join("received");
        tokio::fs::create_dir_all(&base)
            .await
            .expect("temp dir should be created");

        let (_command_tx, command_rx) = mpsc::channel(1);
        let (inbound_tx, inbound_rx) = mpsc::channel(8);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _) = broadcast::channel(1);

        let mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("bind should succeed");
        let (scheduled_sender, _worker) = ScheduledDataSender::spawn(
            mux.split().0,
            mux.local_addr().expect("local addr should exist"),
            ScheduledDataSenderConfig::default(),
        );
        let runtime = tokio::spawn(run_file_transfer_runtime(
            scheduled_sender,
            command_rx,
            inbound_rx,
            event_tx,
            cancel_tx.subscribe(),
            FileTransferRuntimeConfig {
                receive_dir: receive_dir.clone(),
                receive_policy: FileReceivePolicy {
                    allow_overwrite: true,
                    ..FileReceivePolicy::default()
                },
                ..FileTransferRuntimeConfig::default()
            },
        ));

        let manifest = protocol::FileTransferManifest {
            transfer_id: 99,
            file_object_id: 100,
            name: "empty.txt".to_string(),
            group: None,
            mime_type: Some("text/plain".to_string()),
            size_bytes: 0,
            chunk_payload_len: 4096,
            checksum_crc32: crate::data_plane::checksum_crc32(&[]),
        };
        let envelope = crate::file_transfer::manifest_to_envelope(&manifest, 98, 2, 1, 1_000)
            .expect("manifest should encode");
        inbound_tx
            .send(envelope)
            .await
            .expect("manifest should queue");

        let mut completed_path = None;
        for _ in 0..2 {
            let event = timeout(Duration::from_secs(1), event_rx.recv())
                .await
                .expect("event should arrive")
                .expect("event channel should remain open");
            if let FileTransferEvent::IncomingCompleted { path, .. } = event {
                completed_path = Some(path);
            }
        }

        let path = completed_path.expect("empty file should complete");
        assert_eq!(
            tokio::fs::read(&path)
                .await
                .expect("empty file should read"),
            Vec::<u8>::new()
        );

        let _ = cancel_tx.send(());
        runtime
            .await
            .expect("runtime should join")
            .expect("runtime should stop cleanly");
        let _ = tokio::fs::remove_dir_all(&base).await;
    }
}
