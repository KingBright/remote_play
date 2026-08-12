use crate::data_plane::{
    ObjectProgress, ObjectTransferError, ReliableObjectAssembler, checksum_crc32,
};
use protocol::{
    ChunkInfo, ContentKind, DataEnvelope, DataLane, DataPriority, FileTransferControl,
    FileTransferGroup, FileTransferManifest,
};
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

pub const DEFAULT_FILE_CHUNK_PAYLOAD_LEN: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileTransferPolicy {
    pub max_file_bytes: u64,
}

impl Default for FileTransferPolicy {
    fn default() -> Self {
        Self {
            max_file_bytes: 16 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransferSpec {
    pub transfer_id: u64,
    pub manifest_object_id: u64,
    pub file_object_id: u64,
    pub stream_id: u32,
    pub first_sequence_number: u64,
    pub timestamp_ms: u64,
    pub chunk_payload_len: usize,
    pub group: Option<FileTransferGroup>,
}

impl FileTransferSpec {
    pub fn new(
        transfer_id: u64,
        manifest_object_id: u64,
        file_object_id: u64,
        stream_id: u32,
        first_sequence_number: u64,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            transfer_id,
            manifest_object_id,
            file_object_id,
            stream_id,
            first_sequence_number,
            timestamp_ms,
            chunk_payload_len: DEFAULT_FILE_CHUNK_PAYLOAD_LEN,
            group: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileReceivePolicy {
    pub max_file_bytes: u64,
    pub allow_overwrite: bool,
}

impl Default for FileReceivePolicy {
    fn default() -> Self {
        Self {
            max_file_bytes: FileTransferPolicy::default().max_file_bytes,
            allow_overwrite: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransferProgress {
    pub transfer_id: u64,
    pub file_object_id: u64,
    pub received_chunks: u32,
    pub total_chunks: u32,
    pub received_bytes: u64,
    pub total_size: u64,
    pub is_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedFile {
    pub manifest: FileTransferManifest,
    pub path: PathBuf,
}

#[derive(Debug)]
pub enum FileTransferError {
    InvalidFileName,
    InvalidChunkPayloadLen,
    NotAFile(PathBuf),
    FileTooLarge {
        actual_bytes: u64,
        limit_bytes: u64,
    },
    TooManyChunks {
        chunk_count: u64,
    },
    UnexpectedContentKind(ContentKind),
    MissingChunkMetadata,
    InvalidChunk {
        reason: &'static str,
    },
    IncompleteTransfer {
        received_chunks: u32,
        total_chunks: u32,
    },
    ChecksumMismatch {
        expected: u32,
        actual: u32,
    },
    Encode(bincode::Error),
    Decode(bincode::Error),
    Io(io::Error),
    ObjectTransfer(ObjectTransferError),
}

impl fmt::Display for FileTransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileTransferError::InvalidFileName => {
                write!(f, "file transfer name is empty or contains path components")
            }
            FileTransferError::InvalidChunkPayloadLen => {
                write!(
                    f,
                    "file transfer chunk payload length must be greater than zero"
                )
            }
            FileTransferError::NotAFile(path) => {
                write!(f, "path is not a file: {}", path.display())
            }
            FileTransferError::FileTooLarge {
                actual_bytes,
                limit_bytes,
            } => write!(
                f,
                "file transfer is too large: {actual_bytes} > {limit_bytes} bytes"
            ),
            FileTransferError::TooManyChunks { chunk_count } => {
                write!(f, "file transfer requires too many chunks: {chunk_count}")
            }
            FileTransferError::UnexpectedContentKind(kind) => {
                write!(f, "unexpected file transfer content kind: {kind:?}")
            }
            FileTransferError::MissingChunkMetadata => write!(f, "missing file chunk metadata"),
            FileTransferError::InvalidChunk { reason } => {
                write!(f, "invalid file transfer chunk: {reason}")
            }
            FileTransferError::IncompleteTransfer {
                received_chunks,
                total_chunks,
            } => write!(
                f,
                "file transfer is incomplete: {received_chunks}/{total_chunks} chunks"
            ),
            FileTransferError::ChecksumMismatch { expected, actual } => write!(
                f,
                "file transfer checksum mismatch: expected {expected:#x}, got {actual:#x}"
            ),
            FileTransferError::Encode(err) => write!(f, "failed to encode file manifest: {err}"),
            FileTransferError::Decode(err) => write!(f, "failed to decode file manifest: {err}"),
            FileTransferError::Io(err) => write!(f, "file transfer IO failed: {err}"),
            FileTransferError::ObjectTransfer(err) => {
                write!(f, "file transfer object assembly failed: {err}")
            }
        }
    }
}

impl Error for FileTransferError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            FileTransferError::Encode(err) | FileTransferError::Decode(err) => Some(err),
            FileTransferError::Io(err) => Some(err),
            FileTransferError::ObjectTransfer(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for FileTransferError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<ObjectTransferError> for FileTransferError {
    fn from(value: ObjectTransferError) -> Self {
        Self::ObjectTransfer(value)
    }
}

#[derive(Debug)]
pub struct FileTransferReader {
    file: File,
    spec: FileTransferSpec,
    manifest: FileTransferManifest,
    manifest_sent: bool,
    next_chunk_index: u32,
    offset: u64,
    total_chunks: u32,
}

impl FileTransferReader {
    pub async fn from_path(
        path: impl AsRef<Path>,
        spec: FileTransferSpec,
        mime_type: Option<String>,
        policy: FileTransferPolicy,
    ) -> Result<Self, FileTransferError> {
        validate_chunk_payload_len(spec.chunk_payload_len)?;
        let path = path.as_ref();
        let metadata = tokio::fs::metadata(path).await?;
        if !metadata.is_file() {
            return Err(FileTransferError::NotAFile(path.to_path_buf()));
        }

        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| is_safe_file_name(name))
            .ok_or(FileTransferError::InvalidFileName)?
            .to_string();

        let size_bytes = metadata.len();
        if size_bytes > policy.max_file_bytes {
            return Err(FileTransferError::FileTooLarge {
                actual_bytes: size_bytes,
                limit_bytes: policy.max_file_bytes,
            });
        }

        let chunk_count = chunk_count(size_bytes, spec.chunk_payload_len)?;
        let checksum = checksum_file_crc32(path, spec.chunk_payload_len).await?;
        let manifest = FileTransferManifest {
            transfer_id: spec.transfer_id,
            file_object_id: spec.file_object_id,
            name,
            group: spec.group.clone(),
            mime_type,
            size_bytes,
            chunk_payload_len: spec.chunk_payload_len as u32,
            checksum_crc32: checksum,
        };

        let file = File::open(path).await?;
        Ok(Self {
            file,
            spec,
            manifest,
            manifest_sent: false,
            next_chunk_index: 0,
            offset: 0,
            total_chunks: chunk_count,
        })
    }

    pub fn manifest(&self) -> &FileTransferManifest {
        &self.manifest
    }

    pub fn manifest_mut(&mut self) -> &mut FileTransferManifest {
        &mut self.manifest
    }

    pub fn set_group(&mut self, group: FileTransferGroup) {
        self.manifest.group = Some(group);
    }

    pub fn total_chunks(&self) -> u32 {
        self.total_chunks
    }

    pub async fn next_envelope(&mut self) -> Result<Option<DataEnvelope>, FileTransferError> {
        if !self.manifest_sent {
            self.manifest_sent = true;
            return Ok(Some(manifest_to_envelope(
                &self.manifest,
                self.spec.manifest_object_id,
                self.spec.stream_id,
                self.spec.first_sequence_number,
                self.spec.timestamp_ms,
            )?));
        }

        if self.offset >= self.manifest.size_bytes {
            return Ok(None);
        }

        let remaining = self.manifest.size_bytes - self.offset;
        let read_len = remaining.min(self.spec.chunk_payload_len as u64) as usize;
        let mut payload = vec![0_u8; read_len];
        self.file.read_exact(&mut payload).await?;

        let chunk_index = self.next_chunk_index;
        self.next_chunk_index = self.next_chunk_index.saturating_add(1);
        let offset = self.offset;
        self.offset += payload.len() as u64;

        Ok(Some(DataEnvelope::reliable_object_chunk(
            ContentKind::FileChunk,
            self.spec.stream_id,
            self.spec.first_sequence_number + 1 + u64::from(chunk_index),
            self.spec.timestamp_ms,
            ChunkInfo {
                object_id: self.manifest.file_object_id,
                chunk_index,
                total_chunks: self.total_chunks,
                offset,
                total_size: self.manifest.size_bytes,
            },
            Some(self.manifest.checksum_crc32),
            payload,
        )))
    }
}

pub fn manifest_to_envelope(
    manifest: &FileTransferManifest,
    manifest_object_id: u64,
    stream_id: u32,
    sequence_number: u64,
    timestamp_ms: u64,
) -> Result<DataEnvelope, FileTransferError> {
    validate_manifest(manifest, FileReceivePolicy::default())?;
    let payload = manifest.encode().map_err(FileTransferError::Encode)?;
    let checksum = checksum_crc32(&payload);

    Ok(DataEnvelope::reliable_object_chunk(
        ContentKind::FileManifest,
        stream_id,
        sequence_number,
        timestamp_ms,
        ChunkInfo {
            object_id: manifest_object_id,
            chunk_index: 0,
            total_chunks: 1,
            offset: 0,
            total_size: payload.len() as u64,
        },
        Some(checksum),
        payload,
    ))
}

pub fn control_to_envelope(
    control: &FileTransferControl,
    object_id: u64,
    stream_id: u32,
    sequence_number: u64,
    timestamp_ms: u64,
) -> Result<DataEnvelope, FileTransferError> {
    let payload = control.encode().map_err(FileTransferError::Encode)?;
    let checksum = checksum_crc32(&payload);
    let mut envelope = DataEnvelope::reliable_object_chunk(
        ContentKind::FileControl,
        stream_id,
        sequence_number,
        timestamp_ms,
        ChunkInfo {
            object_id,
            chunk_index: 0,
            total_chunks: 1,
            offset: 0,
            total_size: payload.len() as u64,
        },
        Some(checksum),
        payload,
    );
    envelope.header.lane = DataLane::InteractiveControl;
    envelope.header.priority = DataPriority::Interactive;
    Ok(envelope)
}

pub fn control_from_envelope(
    envelope: &DataEnvelope,
) -> Result<FileTransferControl, FileTransferError> {
    ensure_content_kind(envelope, ContentKind::FileControl)?;
    let chunk = envelope
        .header
        .chunk
        .as_ref()
        .ok_or(FileTransferError::MissingChunkMetadata)?;
    if chunk.chunk_index != 0
        || chunk.total_chunks != 1
        || chunk.offset != 0
        || chunk.total_size != envelope.payload.len() as u64
    {
        return Err(FileTransferError::InvalidChunk {
            reason: "file control must be a single complete object chunk",
        });
    }
    if let Some(expected) = envelope
        .header
        .reliability_info
        .as_ref()
        .and_then(|info| info.checksum_crc32)
    {
        let actual = checksum_crc32(&envelope.payload);
        if actual != expected {
            return Err(FileTransferError::ChecksumMismatch { expected, actual });
        }
    }

    FileTransferControl::decode(&envelope.payload).map_err(FileTransferError::Decode)
}

pub struct FileManifestAssembler {
    inner: ReliableObjectAssembler,
}

impl FileManifestAssembler {
    pub fn from_first_chunk(envelope: &DataEnvelope) -> Result<Self, FileTransferError> {
        ensure_content_kind(envelope, ContentKind::FileManifest)?;
        let chunk = envelope
            .header
            .chunk
            .as_ref()
            .ok_or(FileTransferError::MissingChunkMetadata)?;
        let expected_checksum = envelope
            .header
            .reliability_info
            .as_ref()
            .and_then(|info| info.checksum_crc32);

        Ok(Self {
            inner: ReliableObjectAssembler::new(
                chunk.object_id,
                chunk.total_chunks,
                chunk.total_size,
                expected_checksum,
            ),
        })
    }

    pub fn push_chunk(
        &mut self,
        envelope: DataEnvelope,
    ) -> Result<ObjectProgress, FileTransferError> {
        ensure_content_kind(&envelope, ContentKind::FileManifest)?;
        self.inner.push_chunk(envelope).map_err(Into::into)
    }

    pub fn finish(
        self,
        policy: FileReceivePolicy,
    ) -> Result<FileTransferManifest, FileTransferError> {
        let completed = self.inner.finish()?;
        let manifest =
            FileTransferManifest::decode(&completed.bytes).map_err(FileTransferError::Decode)?;
        validate_manifest(&manifest, policy)?;
        Ok(manifest)
    }
}

#[derive(Debug)]
pub struct IncomingFileTransfer {
    manifest: FileTransferManifest,
    path: PathBuf,
    file: File,
    received_indices: HashSet<u32>,
    received_bytes: u64,
    total_chunks: u32,
}

impl IncomingFileTransfer {
    pub async fn start(
        manifest: FileTransferManifest,
        target_dir: impl AsRef<Path>,
        policy: FileReceivePolicy,
    ) -> Result<Self, FileTransferError> {
        validate_manifest(&manifest, policy)?;
        tokio::fs::create_dir_all(target_dir.as_ref()).await?;
        let relative_path = manifest
            .group
            .as_ref()
            .map(|group| group.relative_path.as_str())
            .unwrap_or(&manifest.name);
        let path = target_dir.as_ref().join(relative_path);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut options = OpenOptions::new();
        options.write(true).create(true);
        if policy.allow_overwrite {
            options.truncate(true);
        } else {
            options.create_new(true);
        }

        let file = options.open(&path).await?;
        file.set_len(manifest.size_bytes).await?;
        let total_chunks = chunk_count(manifest.size_bytes, manifest.chunk_payload_len as usize)?;

        Ok(Self {
            manifest,
            path,
            file,
            received_indices: HashSet::new(),
            received_bytes: 0,
            total_chunks,
        })
    }

    pub fn manifest(&self) -> &FileTransferManifest {
        &self.manifest
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn push_chunk(
        &mut self,
        envelope: DataEnvelope,
    ) -> Result<FileTransferProgress, FileTransferError> {
        ensure_content_kind(&envelope, ContentKind::FileChunk)?;
        let chunk = envelope
            .header
            .chunk
            .as_ref()
            .ok_or(FileTransferError::MissingChunkMetadata)?;
        self.validate_chunk(chunk, envelope.payload.len())?;

        if self.received_indices.insert(chunk.chunk_index) {
            self.file.seek(SeekFrom::Start(chunk.offset)).await?;
            self.file.write_all(&envelope.payload).await?;
            self.received_bytes += envelope.payload.len() as u64;
        }

        Ok(self.progress())
    }

    pub fn progress(&self) -> FileTransferProgress {
        FileTransferProgress {
            transfer_id: self.manifest.transfer_id,
            file_object_id: self.manifest.file_object_id,
            received_chunks: self.received_indices.len() as u32,
            total_chunks: self.total_chunks,
            received_bytes: self.received_bytes,
            total_size: self.manifest.size_bytes,
            is_complete: self.received_indices.len() as u32 == self.total_chunks,
        }
    }

    pub async fn finish(mut self) -> Result<ReceivedFile, FileTransferError> {
        if self.received_indices.len() as u32 != self.total_chunks {
            return Err(FileTransferError::IncompleteTransfer {
                received_chunks: self.received_indices.len() as u32,
                total_chunks: self.total_chunks,
            });
        }

        self.file.flush().await?;
        drop(self.file);

        let actual =
            checksum_file_crc32(&self.path, self.manifest.chunk_payload_len as usize).await?;
        if actual != self.manifest.checksum_crc32 {
            return Err(FileTransferError::ChecksumMismatch {
                expected: self.manifest.checksum_crc32,
                actual,
            });
        }

        Ok(ReceivedFile {
            manifest: self.manifest,
            path: self.path,
        })
    }

    pub async fn cancel(self) -> Result<PathBuf, FileTransferError> {
        let path = self.path.clone();
        drop(self.file);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(FileTransferError::Io(err)),
        }
        Ok(path)
    }

    fn validate_chunk(
        &self,
        chunk: &ChunkInfo,
        payload_len: usize,
    ) -> Result<(), FileTransferError> {
        if chunk.object_id != self.manifest.file_object_id {
            return Err(FileTransferError::InvalidChunk {
                reason: "object id does not match manifest file object id",
            });
        }
        if chunk.total_chunks != self.total_chunks || chunk.chunk_index >= self.total_chunks {
            return Err(FileTransferError::InvalidChunk {
                reason: "chunk index or total chunk count is invalid",
            });
        }
        if chunk.total_size != self.manifest.size_bytes {
            return Err(FileTransferError::InvalidChunk {
                reason: "chunk total size does not match manifest",
            });
        }
        if payload_len > self.manifest.chunk_payload_len as usize {
            return Err(FileTransferError::InvalidChunk {
                reason: "chunk payload exceeds negotiated payload length",
            });
        }

        let expected_offset =
            u64::from(chunk.chunk_index) * u64::from(self.manifest.chunk_payload_len);
        if chunk.offset != expected_offset {
            return Err(FileTransferError::InvalidChunk {
                reason: "chunk offset does not match chunk index",
            });
        }
        if chunk.offset + payload_len as u64 > self.manifest.size_bytes {
            return Err(FileTransferError::InvalidChunk {
                reason: "chunk extends past declared file size",
            });
        }

        let is_last_chunk = chunk.chunk_index + 1 == self.total_chunks;
        if !is_last_chunk && payload_len != self.manifest.chunk_payload_len as usize {
            return Err(FileTransferError::InvalidChunk {
                reason: "non-final chunks must fill the negotiated payload length",
            });
        }

        Ok(())
    }
}

fn ensure_content_kind(
    envelope: &DataEnvelope,
    expected: ContentKind,
) -> Result<(), FileTransferError> {
    if envelope.header.kind != expected {
        return Err(FileTransferError::UnexpectedContentKind(
            envelope.header.kind,
        ));
    }
    Ok(())
}

fn validate_manifest(
    manifest: &FileTransferManifest,
    policy: FileReceivePolicy,
) -> Result<(), FileTransferError> {
    if !is_safe_file_name(&manifest.name) {
        return Err(FileTransferError::InvalidFileName);
    }
    if let Some(group) = &manifest.group {
        validate_group_metadata(group, manifest)?;
    }
    validate_chunk_payload_len(manifest.chunk_payload_len as usize)?;
    if manifest.size_bytes > policy.max_file_bytes {
        return Err(FileTransferError::FileTooLarge {
            actual_bytes: manifest.size_bytes,
            limit_bytes: policy.max_file_bytes,
        });
    }
    let _ = chunk_count(manifest.size_bytes, manifest.chunk_payload_len as usize)?;
    Ok(())
}

fn validate_chunk_payload_len(chunk_payload_len: usize) -> Result<(), FileTransferError> {
    if chunk_payload_len == 0 || chunk_payload_len > u32::MAX as usize {
        return Err(FileTransferError::InvalidChunkPayloadLen);
    }
    Ok(())
}

fn validate_group_metadata(
    group: &FileTransferGroup,
    manifest: &FileTransferManifest,
) -> Result<(), FileTransferError> {
    if group.file_count == 0 || group.file_index >= group.file_count {
        return Err(FileTransferError::InvalidChunk {
            reason: "file group index or count is invalid",
        });
    }
    if group.group_total_size_bytes < manifest.size_bytes {
        return Err(FileTransferError::InvalidChunk {
            reason: "file group total size is smaller than file size",
        });
    }
    if !is_safe_relative_path(&group.relative_path) {
        return Err(FileTransferError::InvalidFileName);
    }
    Ok(())
}

fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path).components().all(|component| {
            matches!(component, Component::Normal(_))
                && !component.as_os_str().to_string_lossy().is_empty()
        })
}

fn chunk_count(size_bytes: u64, chunk_payload_len: usize) -> Result<u32, FileTransferError> {
    validate_chunk_payload_len(chunk_payload_len)?;
    if size_bytes == 0 {
        return Ok(0);
    }

    let count = size_bytes.div_ceil(chunk_payload_len as u64);
    u32::try_from(count).map_err(|_| FileTransferError::TooManyChunks { chunk_count: count })
}

fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && Path::new(name)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

async fn checksum_file_crc32(path: &Path, buffer_len: usize) -> Result<u32, FileTransferError> {
    validate_chunk_payload_len(buffer_len)?;
    let mut file = File::open(path).await?;
    let mut buffer = vec![0_u8; buffer_len];
    let mut crc = StreamingCrc32::new();

    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        crc.update(&buffer[..read]);
    }

    Ok(crc.finalize())
}

struct StreamingCrc32 {
    state: u32,
}

impl StreamingCrc32 {
    fn new() -> Self {
        Self { state: 0xffff_ffff }
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.state ^= *byte as u32;
            for _ in 0..8 {
                let mask = (self.state & 1).wrapping_neg();
                self.state = (self.state >> 1) ^ (0xedb8_8320 & mask);
            }
        }
    }

    fn finalize(self) -> u32 {
        !self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{MultiplexedPacket, UdpMultiplexer};
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::time::{Duration, timeout};

    static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn transfer_spec(chunk_payload_len: usize) -> FileTransferSpec {
        FileTransferSpec {
            chunk_payload_len,
            ..FileTransferSpec::new(7, 11, 12, 1, 100, 1_000)
        }
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let counter = TEMP_DIR_COUNTER.fetch_add(1, Relaxed);
        std::env::temp_dir().join(format!("{prefix}-{nanos}-{counter}"))
    }

    #[tokio::test]
    async fn reader_streams_manifest_then_raw_file_chunks_without_loading_whole_file() {
        let base = unique_temp_dir("remote-play-file-transfer");
        tokio::fs::create_dir_all(&base)
            .await
            .expect("temp dir should be created");
        let source = base.join("clip.bin");
        let bytes: Vec<u8> = (0..70_000).map(|index| (index % 251) as u8).collect();
        tokio::fs::write(&source, &bytes)
            .await
            .expect("source file should be written");

        let mut reader = FileTransferReader::from_path(
            &source,
            transfer_spec(16 * 1024),
            Some("application/octet-stream".to_string()),
            FileTransferPolicy::default(),
        )
        .await
        .expect("reader should be created");

        let manifest_envelope = reader
            .next_envelope()
            .await
            .expect("manifest read should succeed")
            .expect("manifest should be first");
        assert_eq!(manifest_envelope.header.kind, ContentKind::FileManifest);

        let mut manifest_assembler = FileManifestAssembler::from_first_chunk(&manifest_envelope)
            .expect("manifest assembler should start");
        let progress = manifest_assembler
            .push_chunk(manifest_envelope)
            .expect("manifest should push");
        assert!(progress.is_complete);
        let manifest = manifest_assembler
            .finish(FileReceivePolicy::default())
            .expect("manifest should decode");
        assert_eq!(manifest.name, "clip.bin");
        assert_eq!(manifest.size_bytes, bytes.len() as u64);
        assert_eq!(manifest.checksum_crc32, checksum_crc32(&bytes));

        let mut chunks = Vec::new();
        while let Some(envelope) = reader
            .next_envelope()
            .await
            .expect("file chunk read should succeed")
        {
            assert_eq!(envelope.header.kind, ContentKind::FileChunk);
            assert!(envelope.payload.len() <= 16 * 1024);
            chunks.push(envelope);
        }

        assert_eq!(chunks.len(), bytes.len().div_ceil(16 * 1024));
        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn incoming_file_transfer_writes_chunks_out_of_order_and_verifies_checksum() {
        let base = unique_temp_dir("remote-play-file-transfer");
        let source_dir = base.join("source");
        let target_dir = base.join("target");
        tokio::fs::create_dir_all(&source_dir)
            .await
            .expect("source dir should be created");
        let source = source_dir.join("paste.dat");
        let bytes: Vec<u8> = (0..50_000).map(|index| (index % 193) as u8).collect();
        tokio::fs::write(&source, &bytes)
            .await
            .expect("source file should be written");

        let mut reader = FileTransferReader::from_path(
            &source,
            transfer_spec(8192),
            None,
            FileTransferPolicy::default(),
        )
        .await
        .expect("reader should be created");
        let manifest_envelope = reader
            .next_envelope()
            .await
            .expect("manifest read should succeed")
            .expect("manifest should exist");
        let mut manifest_assembler = FileManifestAssembler::from_first_chunk(&manifest_envelope)
            .expect("manifest assembler should start");
        manifest_assembler
            .push_chunk(manifest_envelope)
            .expect("manifest should push");
        let manifest = manifest_assembler
            .finish(FileReceivePolicy::default())
            .expect("manifest should decode");

        let mut receiver = IncomingFileTransfer::start(
            manifest,
            &target_dir,
            FileReceivePolicy {
                allow_overwrite: true,
                ..FileReceivePolicy::default()
            },
        )
        .await
        .expect("receiver should start");

        let mut chunks = Vec::new();
        while let Some(envelope) = reader
            .next_envelope()
            .await
            .expect("file chunk read should succeed")
        {
            chunks.push(envelope);
        }
        chunks.reverse();

        let mut last_progress = None;
        for envelope in chunks {
            last_progress = Some(
                receiver
                    .push_chunk(envelope)
                    .await
                    .expect("chunk should write"),
            );
        }
        assert!(last_progress.expect("progress should exist").is_complete);

        let received = receiver.finish().await.expect("file should finish");
        assert_eq!(received.manifest.name, "paste.dat");
        let received_bytes = tokio::fs::read(&received.path)
            .await
            .expect("received file should read");
        assert_eq!(received_bytes, bytes);

        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn file_transfer_roundtrips_over_udp_data_path() {
        let base = unique_temp_dir("remote-play-file-transfer");
        let source_dir = base.join("source");
        let target_dir = base.join("target");
        tokio::fs::create_dir_all(&source_dir)
            .await
            .expect("source dir should be created");
        let source = source_dir.join("udp-paste.bin");
        let bytes: Vec<u8> = (0..24_000).map(|index| (index % 211) as u8).collect();
        tokio::fs::write(&source, &bytes)
            .await
            .expect("source file should be written");

        let sender_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("sender bind should succeed");
        let sender = sender_mux.split().0;
        let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("receiver bind should succeed");
        let receiver_addr = receiver_mux
            .local_addr()
            .expect("receiver local addr should exist");
        let receiver = receiver_mux.split().1;

        let mut reader = FileTransferReader::from_path(
            &source,
            transfer_spec(4096),
            Some("application/octet-stream".to_string()),
            FileTransferPolicy::default(),
        )
        .await
        .expect("reader should be created");

        while let Some(envelope) = reader
            .next_envelope()
            .await
            .expect("envelope should be produced")
        {
            sender
                .send_data(&envelope, receiver_addr)
                .await
                .expect("envelope should send");
        }

        let first = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("manifest should arrive")
            .expect("manifest should decode");
        let MultiplexedPacket::Data(manifest_envelope, _) = first else {
            panic!("expected manifest data packet");
        };
        assert_eq!(manifest_envelope.header.kind, ContentKind::FileManifest);

        let mut manifest_assembler = FileManifestAssembler::from_first_chunk(&manifest_envelope)
            .expect("manifest assembler should start");
        manifest_assembler
            .push_chunk(manifest_envelope)
            .expect("manifest should push");
        let manifest = manifest_assembler
            .finish(FileReceivePolicy::default())
            .expect("manifest should decode");
        let expected_chunks = chunk_count(manifest.size_bytes, manifest.chunk_payload_len as usize)
            .expect("chunk count should compute");

        let mut incoming = IncomingFileTransfer::start(
            manifest,
            &target_dir,
            FileReceivePolicy {
                allow_overwrite: true,
                ..FileReceivePolicy::default()
            },
        )
        .await
        .expect("incoming transfer should start");

        for _ in 0..expected_chunks {
            let packet = timeout(Duration::from_secs(1), receiver.recv())
                .await
                .expect("file chunk should arrive")
                .expect("file chunk should decode");
            let MultiplexedPacket::Data(envelope, _) = packet else {
                panic!("expected file chunk data packet");
            };
            incoming
                .push_chunk(envelope)
                .await
                .expect("file chunk should write");
        }

        let received = incoming.finish().await.expect("file should finish");
        let received_bytes = tokio::fs::read(&received.path)
            .await
            .expect("received file should read");
        assert_eq!(received_bytes, bytes);

        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn file_control_roundtrips_over_udp_data_path() {
        let sender_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("sender bind should succeed");
        let sender = sender_mux.split().0;
        let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("receiver bind should succeed");
        let receiver_addr = receiver_mux
            .local_addr()
            .expect("receiver local addr should exist");
        let receiver = receiver_mux.split().1;

        let control = FileTransferControl::CancelTransfer {
            transfer_id: 7,
            file_object_id: 12,
        };
        let envelope = control_to_envelope(&control, 90, 2, 100, 1_000)
            .expect("control envelope should encode");
        assert_eq!(envelope.header.kind, ContentKind::FileControl);
        assert_eq!(envelope.header.lane, DataLane::InteractiveControl);

        sender
            .send_data(&envelope, receiver_addr)
            .await
            .expect("control envelope should send");

        let packet = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("control should arrive")
            .expect("control should decode");
        let MultiplexedPacket::Data(envelope, _) = packet else {
            panic!("expected file control data packet");
        };
        assert_eq!(
            control_from_envelope(&envelope).expect("control should decode"),
            control
        );
    }

    #[tokio::test]
    async fn reader_rejects_unsafe_file_names_and_size_limit() {
        let base = unique_temp_dir("remote-play-file-transfer");
        tokio::fs::create_dir_all(&base)
            .await
            .expect("temp dir should be created");
        let source = base.join("too-big.txt");
        tokio::fs::write(&source, b"hello")
            .await
            .expect("source file should be written");

        let err = FileTransferReader::from_path(
            &source,
            transfer_spec(1024),
            None,
            FileTransferPolicy { max_file_bytes: 4 },
        )
        .await
        .expect_err("size limit should reject file");
        assert!(matches!(err, FileTransferError::FileTooLarge { .. }));

        let unsafe_manifest = FileTransferManifest {
            transfer_id: 1,
            file_object_id: 2,
            name: "../escape.txt".to_string(),
            group: None,
            mime_type: None,
            size_bytes: 1,
            chunk_payload_len: 1024,
            checksum_crc32: 0,
        };
        let err = IncomingFileTransfer::start(
            unsafe_manifest,
            base.join("target"),
            FileReceivePolicy::default(),
        )
        .await
        .expect_err("unsafe file name should be rejected");
        assert!(matches!(err, FileTransferError::InvalidFileName));

        let _ = tokio::fs::remove_dir_all(&base).await;
    }
}
