use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadType {
    VideoH265 = 96,
    AudioOpus = 97,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataLane {
    RealtimeVideo,
    RealtimeAudio,
    InteractiveControl,
    ReliableObject,
    Background,
}

impl DataLane {
    pub fn is_realtime(self) -> bool {
        matches!(
            self,
            DataLane::RealtimeVideo | DataLane::RealtimeAudio | DataLane::InteractiveControl
        )
    }

    pub fn allows_stale_drop(self) -> bool {
        matches!(self, DataLane::RealtimeVideo | DataLane::RealtimeAudio)
    }

    pub fn requires_reliable_delivery(self) -> bool {
        matches!(self, DataLane::ReliableObject | DataLane::Background)
    }

    fn wire_id(self) -> u8 {
        match self {
            DataLane::RealtimeVideo => 1,
            DataLane::RealtimeAudio => 2,
            DataLane::InteractiveControl => 3,
            DataLane::ReliableObject => 4,
            DataLane::Background => 5,
        }
    }

    fn from_wire_id(id: u8) -> Result<Self, CompactRealtimeError> {
        match id {
            1 => Ok(DataLane::RealtimeVideo),
            2 => Ok(DataLane::RealtimeAudio),
            3 => Ok(DataLane::InteractiveControl),
            4 => Ok(DataLane::ReliableObject),
            5 => Ok(DataLane::Background),
            _ => Err(CompactRealtimeError::InvalidLane(id)),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    VideoH265,
    AudioOpus,
    Control,
    ClipboardText,
    ClipboardBinary,
    FileChunk,
    FileManifest,
    Arbitrary,
    ClipboardBundle,
    FileControl,
    AudioStreamConfig,
}

impl ContentKind {
    pub fn default_lane(self) -> DataLane {
        match self {
            ContentKind::VideoH265 => DataLane::RealtimeVideo,
            ContentKind::AudioOpus => DataLane::RealtimeAudio,
            ContentKind::AudioStreamConfig => DataLane::InteractiveControl,
            ContentKind::Control => DataLane::InteractiveControl,
            ContentKind::ClipboardText
            | ContentKind::ClipboardBinary
            | ContentKind::FileManifest
            | ContentKind::FileChunk
            | ContentKind::Arbitrary
            | ContentKind::ClipboardBundle => DataLane::ReliableObject,
            ContentKind::FileControl => DataLane::InteractiveControl,
        }
    }

    fn wire_id(self) -> u8 {
        match self {
            ContentKind::VideoH265 => 1,
            ContentKind::AudioOpus => 2,
            ContentKind::Control => 3,
            ContentKind::ClipboardText => 4,
            ContentKind::ClipboardBinary => 5,
            ContentKind::FileManifest => 6,
            ContentKind::FileChunk => 7,
            ContentKind::Arbitrary => 8,
            ContentKind::ClipboardBundle => 9,
            ContentKind::FileControl => 10,
            ContentKind::AudioStreamConfig => 11,
        }
    }

    fn from_wire_id(id: u8) -> Result<Self, CompactRealtimeError> {
        match id {
            1 => Ok(ContentKind::VideoH265),
            2 => Ok(ContentKind::AudioOpus),
            3 => Ok(ContentKind::Control),
            4 => Ok(ContentKind::ClipboardText),
            5 => Ok(ContentKind::ClipboardBinary),
            6 => Ok(ContentKind::FileManifest),
            7 => Ok(ContentKind::FileChunk),
            8 => Ok(ContentKind::Arbitrary),
            9 => Ok(ContentKind::ClipboardBundle),
            10 => Ok(ContentKind::FileControl),
            11 => Ok(ContentKind::AudioStreamConfig),
            _ => Err(CompactRealtimeError::InvalidContentKind(id)),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    RemoteSystem,
    RemoteMicrophone,
    RemoteMixed,
    ViewerMicrophoneTalkback,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDirection {
    HostToClient,
    ClientToHost,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    Opus,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioControlTarget {
    ViewerTalkbackPlayback,
}

pub const REMOTE_MICROPHONE_AUDIO_STREAM_OFFSET: u32 = 1;
pub const REMOTE_SYSTEM_AUDIO_STREAM_OFFSET: u32 = 2;
pub const VIEWER_TALKBACK_AUDIO_STREAM_OFFSET: u32 = 100;

pub fn remote_microphone_audio_stream_id(session_id: u32) -> u32 {
    session_id.wrapping_add(REMOTE_MICROPHONE_AUDIO_STREAM_OFFSET)
}

pub fn remote_system_audio_stream_id(session_id: u32) -> u32 {
    session_id.wrapping_add(REMOTE_SYSTEM_AUDIO_STREAM_OFFSET)
}

pub fn viewer_talkback_audio_stream_id(session_id: u32) -> u32 {
    session_id.wrapping_add(VIEWER_TALKBACK_AUDIO_STREAM_OFFSET)
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AudioStreamConfig {
    pub stream_id: u32,
    pub source: AudioSource,
    pub direction: AudioDirection,
    pub codec: AudioCodec,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub frame_duration_ms: u16,
}

impl AudioStreamConfig {
    pub fn new(
        stream_id: u32,
        source: AudioSource,
        direction: AudioDirection,
        sample_rate_hz: u32,
        channels: u16,
        frame_duration_ms: u16,
    ) -> Self {
        Self {
            stream_id,
            source,
            direction,
            codec: AudioCodec::Opus,
            sample_rate_hz,
            channels,
            frame_duration_ms,
        }
    }

    pub fn remote_system(
        stream_id: u32,
        sample_rate_hz: u32,
        channels: u16,
        frame_duration_ms: u16,
    ) -> Self {
        Self::new(
            stream_id,
            AudioSource::RemoteSystem,
            AudioDirection::HostToClient,
            sample_rate_hz,
            channels,
            frame_duration_ms,
        )
    }

    pub fn remote_microphone(
        stream_id: u32,
        sample_rate_hz: u32,
        channels: u16,
        frame_duration_ms: u16,
    ) -> Self {
        Self::new(
            stream_id,
            AudioSource::RemoteMicrophone,
            AudioDirection::HostToClient,
            sample_rate_hz,
            channels,
            frame_duration_ms,
        )
    }

    pub fn remote_mixed(
        stream_id: u32,
        sample_rate_hz: u32,
        channels: u16,
        frame_duration_ms: u16,
    ) -> Self {
        Self::new(
            stream_id,
            AudioSource::RemoteMixed,
            AudioDirection::HostToClient,
            sample_rate_hz,
            channels,
            frame_duration_ms,
        )
    }

    pub fn viewer_microphone_talkback(
        stream_id: u32,
        sample_rate_hz: u32,
        channels: u16,
        frame_duration_ms: u16,
    ) -> Self {
        Self::new(
            stream_id,
            AudioSource::ViewerMicrophoneTalkback,
            AudioDirection::ClientToHost,
            sample_rate_hz,
            channels,
            frame_duration_ms,
        )
    }

    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    pub fn decode(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FileTransferGroup {
    pub group_id: u64,
    pub file_index: u32,
    pub file_count: u32,
    pub relative_path: String,
    pub group_total_size_bytes: u64,
    pub group_checksum_crc32: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FileTransferManifest {
    pub transfer_id: u64,
    pub file_object_id: u64,
    pub name: String,
    pub group: Option<FileTransferGroup>,
    pub mime_type: Option<String>,
    pub size_bytes: u64,
    pub chunk_payload_len: u32,
    pub checksum_crc32: u32,
}

impl FileTransferManifest {
    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    pub fn decode(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum FileTransferControl {
    CancelTransfer {
        transfer_id: u64,
        file_object_id: u64,
    },
    CancelGroup {
        group_id: u64,
    },
}

impl FileTransferControl {
    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    pub fn decode(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ClipboardBundle {
    pub bundle_id: u64,
    pub items: Vec<ClipboardItem>,
}

impl ClipboardBundle {
    pub fn new(bundle_id: u64, items: Vec<ClipboardItem>) -> Self {
        Self { bundle_id, items }
    }

    pub fn text(bundle_id: u64, text: impl Into<String>) -> Self {
        Self::new(
            bundle_id,
            vec![ClipboardItem::Text(ClipboardText { text: text.into() })],
        )
    }

    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    pub fn decode(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ClipboardItem {
    Text(ClipboardText),
    Image(ClipboardImage),
    File(ClipboardFile),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ClipboardText {
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bytes: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ClipboardFile {
    pub name: String,
    pub mime_type: Option<String>,
    pub bytes: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DataPriority {
    Background = 0,
    Normal = 1,
    Interactive = 2,
    Realtime = 3,
}

impl DataPriority {
    pub fn for_lane(lane: DataLane) -> Self {
        match lane {
            DataLane::RealtimeVideo | DataLane::RealtimeAudio => DataPriority::Realtime,
            DataLane::InteractiveControl => DataPriority::Interactive,
            DataLane::ReliableObject => DataPriority::Normal,
            DataLane::Background => DataPriority::Background,
        }
    }

    fn wire_id(self) -> u8 {
        self as u8
    }

    fn from_wire_id(id: u8) -> Result<Self, CompactRealtimeError> {
        match id {
            0 => Ok(DataPriority::Background),
            1 => Ok(DataPriority::Normal),
            2 => Ok(DataPriority::Interactive),
            3 => Ok(DataPriority::Realtime),
            _ => Err(CompactRealtimeError::InvalidPriority(id)),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReliabilityMode {
    BestEffort,
    Reliable,
}

impl ReliabilityMode {
    pub fn for_lane(lane: DataLane) -> Self {
        if lane.requires_reliable_delivery() {
            ReliabilityMode::Reliable
        } else {
            ReliabilityMode::BestEffort
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ChunkInfo {
    pub object_id: u64,
    pub chunk_index: u32,
    pub total_chunks: u32,
    pub offset: u64,
    pub total_size: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReliabilityInfo {
    pub ack_id: u64,
    pub retry_count: u8,
    pub checksum_crc32: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DataHeader {
    pub version: u8,
    pub lane: DataLane,
    pub kind: ContentKind,
    pub priority: DataPriority,
    pub reliability: ReliabilityMode,
    pub stream_id: u32,
    pub sequence_number: u64,
    /// Sender-side timestamp in milliseconds. Realtime receivers can use this for freshness.
    pub timestamp_ms: u64,
    /// Optional absolute deadline in sender timebase milliseconds.
    pub deadline_ms: Option<u64>,
    pub chunk: Option<ChunkInfo>,
    pub reliability_info: Option<ReliabilityInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DataEnvelope {
    pub header: DataHeader,
    pub payload: Vec<u8>,
}

impl DataEnvelope {
    pub fn new(
        lane: DataLane,
        kind: ContentKind,
        stream_id: u32,
        sequence_number: u64,
        timestamp_ms: u64,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            header: DataHeader {
                version: 1,
                lane,
                kind,
                priority: DataPriority::for_lane(lane),
                reliability: ReliabilityMode::for_lane(lane),
                stream_id,
                sequence_number,
                timestamp_ms,
                deadline_ms: None,
                chunk: None,
                reliability_info: None,
            },
            payload,
        }
    }

    pub fn realtime_video(
        stream_id: u32,
        sequence_number: u64,
        timestamp_ms: u64,
        deadline_ms: u64,
        payload: Vec<u8>,
    ) -> Self {
        let mut envelope = Self::new(
            DataLane::RealtimeVideo,
            ContentKind::VideoH265,
            stream_id,
            sequence_number,
            timestamp_ms,
            payload,
        );
        envelope.header.deadline_ms = Some(deadline_ms);
        envelope
    }

    pub fn realtime_audio(
        stream_id: u32,
        sequence_number: u64,
        timestamp_ms: u64,
        payload: Vec<u8>,
    ) -> Self {
        Self::new(
            DataLane::RealtimeAudio,
            ContentKind::AudioOpus,
            stream_id,
            sequence_number,
            timestamp_ms,
            payload,
        )
    }

    pub fn audio_stream_config(
        config: &AudioStreamConfig,
        sequence_number: u64,
        timestamp_ms: u64,
    ) -> Result<Self, bincode::Error> {
        Ok(Self::new(
            DataLane::InteractiveControl,
            ContentKind::AudioStreamConfig,
            config.stream_id,
            sequence_number,
            timestamp_ms,
            config.encode()?,
        ))
    }

    pub fn reliable_object_chunk(
        kind: ContentKind,
        stream_id: u32,
        sequence_number: u64,
        timestamp_ms: u64,
        chunk: ChunkInfo,
        checksum_crc32: Option<u32>,
        payload: Vec<u8>,
    ) -> Self {
        let mut envelope = Self::new(
            DataLane::ReliableObject,
            kind,
            stream_id,
            sequence_number,
            timestamp_ms,
            payload,
        );
        envelope.header.chunk = Some(chunk);
        envelope.header.reliability_info = Some(ReliabilityInfo {
            ack_id: sequence_number,
            retry_count: 0,
            checksum_crc32,
        });
        envelope
    }

    pub fn is_realtime(&self) -> bool {
        self.header.lane.is_realtime()
    }

    pub fn allows_stale_drop(&self) -> bool {
        self.header.lane.allows_stale_drop()
    }

    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    pub fn decode(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }

    pub fn encode_compact_realtime(&self) -> Result<Vec<u8>, CompactRealtimeError> {
        let header = CompactRealtimeHeader::from_data_header(&self.header)?;
        header.encode_with_payload(&self.payload)
    }

    pub fn decode_compact_realtime(data: &[u8]) -> Result<Self, CompactRealtimeError> {
        CompactRealtimeHeader::decode_envelope(data)
    }
}

pub const COMPACT_REALTIME_WIRE_VERSION: u8 = 1;
pub const COMPACT_REALTIME_HEADER_LEN: usize = 26;

const COMPACT_REALTIME_DEADLINE_FLAG: u8 = 0x01;
const COMPACT_REALTIME_KNOWN_FLAGS: u8 = COMPACT_REALTIME_DEADLINE_FLAG;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactRealtimeHeader {
    pub version: u8,
    pub lane: DataLane,
    pub kind: ContentKind,
    pub priority: DataPriority,
    pub stream_id: u32,
    pub sequence_number: u64,
    pub timestamp_ms: u64,
    pub deadline_delta_ms: Option<u16>,
}

impl CompactRealtimeHeader {
    pub fn from_data_header(header: &DataHeader) -> Result<Self, CompactRealtimeError> {
        if header.version > 0x0f {
            return Err(CompactRealtimeError::UnsupportedVersion(header.version));
        }

        if !header.lane.is_realtime() {
            return Err(CompactRealtimeError::NonRealtimeLane(header.lane));
        }

        if header.chunk.is_some()
            || header.reliability_info.is_some()
            || header.reliability != ReliabilityMode::BestEffort
        {
            return Err(CompactRealtimeError::UnsupportedRealtimeMetadata);
        }

        let deadline_delta_ms = match header.deadline_ms {
            Some(deadline_ms) => {
                let delta = deadline_ms.checked_sub(header.timestamp_ms).ok_or(
                    CompactRealtimeError::DeadlineBeforeTimestamp {
                        timestamp_ms: header.timestamp_ms,
                        deadline_ms,
                    },
                )?;
                Some(
                    u16::try_from(delta)
                        .map_err(|_| CompactRealtimeError::DeadlineDeltaTooLarge(delta))?,
                )
            }
            None => None,
        };

        Ok(Self {
            version: header.version,
            lane: header.lane,
            kind: header.kind,
            priority: header.priority,
            stream_id: header.stream_id,
            sequence_number: header.sequence_number,
            timestamp_ms: header.timestamp_ms,
            deadline_delta_ms,
        })
    }

    pub fn to_data_header(self) -> Result<DataHeader, CompactRealtimeError> {
        if !self.lane.is_realtime() {
            return Err(CompactRealtimeError::NonRealtimeLane(self.lane));
        }

        let deadline_ms = match self.deadline_delta_ms {
            Some(delta) => Some(self.timestamp_ms.checked_add(delta as u64).ok_or(
                CompactRealtimeError::DeadlineOverflow {
                    timestamp_ms: self.timestamp_ms,
                    deadline_delta_ms: delta,
                },
            )?),
            None => None,
        };

        Ok(DataHeader {
            version: self.version,
            lane: self.lane,
            kind: self.kind,
            priority: self.priority,
            reliability: ReliabilityMode::BestEffort,
            stream_id: self.stream_id,
            sequence_number: self.sequence_number,
            timestamp_ms: self.timestamp_ms,
            deadline_ms,
            chunk: None,
            reliability_info: None,
        })
    }

    pub fn encode_with_payload(self, payload: &[u8]) -> Result<Vec<u8>, CompactRealtimeError> {
        if self.version > 0x0f {
            return Err(CompactRealtimeError::UnsupportedVersion(self.version));
        }

        if !self.lane.is_realtime() {
            return Err(CompactRealtimeError::NonRealtimeLane(self.lane));
        }

        let mut flags = 0;
        let deadline_delta_ms = if let Some(delta) = self.deadline_delta_ms {
            flags |= COMPACT_REALTIME_DEADLINE_FLAG;
            delta
        } else {
            0
        };

        let mut out = Vec::with_capacity(COMPACT_REALTIME_HEADER_LEN + payload.len());
        out.push((self.version << 4) | flags);
        out.push(self.lane.wire_id());
        out.push(self.kind.wire_id());
        out.push(self.priority.wire_id());
        out.extend_from_slice(&self.stream_id.to_be_bytes());
        out.extend_from_slice(&self.sequence_number.to_be_bytes());
        out.extend_from_slice(&self.timestamp_ms.to_be_bytes());
        out.extend_from_slice(&deadline_delta_ms.to_be_bytes());
        out.extend_from_slice(payload);
        Ok(out)
    }

    pub fn decode(data: &[u8]) -> Result<(Self, &[u8]), CompactRealtimeError> {
        if data.len() < COMPACT_REALTIME_HEADER_LEN {
            return Err(CompactRealtimeError::InvalidLength {
                expected_at_least: COMPACT_REALTIME_HEADER_LEN,
                actual: data.len(),
            });
        }

        let version = data[0] >> 4;
        let flags = data[0] & 0x0f;
        if version != COMPACT_REALTIME_WIRE_VERSION {
            return Err(CompactRealtimeError::UnsupportedVersion(version));
        }
        if flags & !COMPACT_REALTIME_KNOWN_FLAGS != 0 {
            return Err(CompactRealtimeError::UnknownFlags(flags));
        }

        let lane = DataLane::from_wire_id(data[1])?;
        if !lane.is_realtime() {
            return Err(CompactRealtimeError::NonRealtimeLane(lane));
        }

        let kind = ContentKind::from_wire_id(data[2])?;
        let priority = DataPriority::from_wire_id(data[3])?;
        let stream_id = u32::from_be_bytes(data[4..8].try_into().unwrap());
        let sequence_number = u64::from_be_bytes(data[8..16].try_into().unwrap());
        let timestamp_ms = u64::from_be_bytes(data[16..24].try_into().unwrap());
        let raw_deadline_delta = u16::from_be_bytes(data[24..26].try_into().unwrap());
        let deadline_delta_ms = if flags & COMPACT_REALTIME_DEADLINE_FLAG != 0 {
            Some(raw_deadline_delta)
        } else {
            if raw_deadline_delta != 0 {
                return Err(CompactRealtimeError::DeadlineDeltaWithoutFlag(
                    raw_deadline_delta,
                ));
            }
            None
        };

        Ok((
            Self {
                version,
                lane,
                kind,
                priority,
                stream_id,
                sequence_number,
                timestamp_ms,
                deadline_delta_ms,
            },
            &data[COMPACT_REALTIME_HEADER_LEN..],
        ))
    }

    pub fn decode_envelope(data: &[u8]) -> Result<DataEnvelope, CompactRealtimeError> {
        let (header, payload) = Self::decode(data)?;
        Ok(DataEnvelope {
            header: header.to_data_header()?,
            payload: payload.to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactRealtimeError {
    InvalidLength {
        expected_at_least: usize,
        actual: usize,
    },
    UnsupportedVersion(u8),
    UnknownFlags(u8),
    InvalidLane(u8),
    InvalidContentKind(u8),
    InvalidPriority(u8),
    NonRealtimeLane(DataLane),
    UnsupportedRealtimeMetadata,
    DeadlineBeforeTimestamp {
        timestamp_ms: u64,
        deadline_ms: u64,
    },
    DeadlineDeltaTooLarge(u64),
    DeadlineDeltaWithoutFlag(u16),
    DeadlineOverflow {
        timestamp_ms: u64,
        deadline_delta_ms: u16,
    },
}

impl fmt::Display for CompactRealtimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompactRealtimeError::InvalidLength {
                expected_at_least,
                actual,
            } => write!(
                f,
                "compact realtime packet too short: expected at least {expected_at_least} bytes, got {actual}"
            ),
            CompactRealtimeError::UnsupportedVersion(version) => {
                write!(f, "unsupported compact realtime version {version}")
            }
            CompactRealtimeError::UnknownFlags(flags) => {
                write!(f, "unknown compact realtime flags {flags:#x}")
            }
            CompactRealtimeError::InvalidLane(lane) => {
                write!(f, "invalid compact realtime lane id {lane}")
            }
            CompactRealtimeError::InvalidContentKind(kind) => {
                write!(f, "invalid compact realtime content kind id {kind}")
            }
            CompactRealtimeError::InvalidPriority(priority) => {
                write!(f, "invalid compact realtime priority id {priority}")
            }
            CompactRealtimeError::NonRealtimeLane(lane) => {
                write!(f, "compact realtime packet cannot carry lane {lane:?}")
            }
            CompactRealtimeError::UnsupportedRealtimeMetadata => write!(
                f,
                "compact realtime packet cannot carry chunk or reliability metadata"
            ),
            CompactRealtimeError::DeadlineBeforeTimestamp {
                timestamp_ms,
                deadline_ms,
            } => write!(
                f,
                "compact realtime deadline {deadline_ms} is before timestamp {timestamp_ms}"
            ),
            CompactRealtimeError::DeadlineDeltaTooLarge(delta) => write!(
                f,
                "compact realtime deadline delta {delta}ms exceeds u16 range"
            ),
            CompactRealtimeError::DeadlineDeltaWithoutFlag(delta) => write!(
                f,
                "compact realtime deadline delta {delta} present without deadline flag"
            ),
            CompactRealtimeError::DeadlineOverflow {
                timestamp_ms,
                deadline_delta_ms,
            } => write!(
                f,
                "compact realtime deadline overflows timestamp {timestamp_ms} plus delta {deadline_delta_ms}"
            ),
        }
    }
}

impl Error for CompactRealtimeError {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RtpHeader {
    pub version: u8,
    pub payload_type: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket {
    pub header: RtpHeader,
    pub payload: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ControlMessage {
    HandshakeReq,
    HandshakeAck,
    /// Sent at 1000Hz from client to host
    Input(InputEvent),
    /// Sent from host to client for Force Feedback (Future)
    ForceFeedback,
    /// Sent at 1Hz from host to client with host-side telemetry
    HostTelemetry {
        fps: f32,
        encode_latency_ms: f32,
        jitter_ms: f32,
        bitrate_kbps: u32,
    },
    /// Start capturing and streaming
    StartStream {
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        session_id: u32,
    },
    /// Stop streaming and return to standby
    StopStream,
    /// Adjust active-session audio behavior without touching realtime media packets.
    AudioControl {
        session_id: u32,
        target: AudioControlTarget,
        muted: bool,
        volume_percent: u8,
    },
    /// Keep-alive ping from client to host
    Heartbeat,
    /// Update streaming parameters on the fly
    UpdateStreamSettings {
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        session_id: u32,
    },
    /// Precise timestamp ping for RTT & Clock offset estimation
    Ping {
        client_send_ts: u64,
    },
    /// Precise timestamp pong response
    Pong {
        client_send_ts: u64,
        host_recv_ts: u64,
        host_send_ts: u64,
    },
    /// Request an immediate IDR keyframe from the host for fast auto-recovery (PLI/FIR)
    RequestKeyframe {
        session_id: u32,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchAction {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum InputEvent {
    KeyDown(u32), // Scancode
    KeyUp(u32),
    MouseMove {
        dx: i32,
        dy: i32,
    },
    MouseMoveAbsolute {
        x: u16,
        y: u16,
    },
    Key {
        key_code: u16,
        pressed: bool,
        modifiers: u8,
    },
    ModifiersChanged(u8),
    MouseDown(u8), // Button ID
    MouseUp(u8),
    MouseScroll {
        delta_x: i32,
        delta_y: i32,
    },
    Touch {
        action: TouchAction,
        pointer_id: u32,
        normalized_x: f32,
        normalized_y: f32,
        pressure: f32,
    },
}

pub mod input_modifiers {
    pub const SHIFT: u8 = 1 << 0;
    pub const CONTROL: u8 = 1 << 1;
    pub const ALT: u8 = 1 << 2;
    pub const META: u8 = 1 << 3;
    pub const FUNCTION: u8 = 1 << 4;
    pub const CAPS_LOCK: u8 = 1 << 5;
}

impl RtpPacket {
    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    pub fn decode(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}

impl ControlMessage {
    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    pub fn decode(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_control(message: ControlMessage) -> ControlMessage {
        let encoded = message.encode().expect("control message should encode");
        ControlMessage::decode(&encoded).expect("control message should decode")
    }

    fn roundtrip_envelope(envelope: DataEnvelope) -> DataEnvelope {
        let encoded = envelope.encode().expect("data envelope should encode");
        DataEnvelope::decode(&encoded).expect("data envelope should decode")
    }

    #[test]
    fn data_lane_classification_matches_latency_requirements() {
        assert!(DataLane::RealtimeVideo.is_realtime());
        assert!(DataLane::RealtimeAudio.is_realtime());
        assert!(DataLane::InteractiveControl.is_realtime());
        assert!(!DataLane::ReliableObject.is_realtime());
        assert!(!DataLane::Background.is_realtime());

        assert!(DataLane::RealtimeVideo.allows_stale_drop());
        assert!(DataLane::RealtimeAudio.allows_stale_drop());
        assert!(!DataLane::InteractiveControl.allows_stale_drop());

        assert_eq!(
            ReliabilityMode::for_lane(DataLane::ReliableObject),
            ReliabilityMode::Reliable
        );
        assert_eq!(
            ReliabilityMode::for_lane(DataLane::RealtimeVideo),
            ReliabilityMode::BestEffort
        );
    }

    #[test]
    fn content_kind_defaults_to_expected_lane() {
        assert_eq!(
            ContentKind::VideoH265.default_lane(),
            DataLane::RealtimeVideo
        );
        assert_eq!(
            ContentKind::AudioOpus.default_lane(),
            DataLane::RealtimeAudio
        );
        assert_eq!(
            ContentKind::AudioStreamConfig.default_lane(),
            DataLane::InteractiveControl
        );
        assert_eq!(
            ContentKind::Control.default_lane(),
            DataLane::InteractiveControl
        );
        assert_eq!(
            ContentKind::ClipboardText.default_lane(),
            DataLane::ReliableObject
        );
        assert_eq!(
            ContentKind::ClipboardBundle.default_lane(),
            DataLane::ReliableObject
        );
        assert_eq!(
            ContentKind::FileManifest.default_lane(),
            DataLane::ReliableObject
        );
        assert_eq!(
            ContentKind::FileChunk.default_lane(),
            DataLane::ReliableObject
        );
        assert_eq!(
            ContentKind::FileControl.default_lane(),
            DataLane::InteractiveControl
        );
    }

    #[test]
    fn audio_stream_config_roundtrips() {
        let configs = [
            AudioStreamConfig::remote_system(10, 48_000, 2, 20),
            AudioStreamConfig::remote_microphone(11, 48_000, 1, 20),
            AudioStreamConfig::remote_mixed(12, 48_000, 2, 20),
            AudioStreamConfig::viewer_microphone_talkback(13, 48_000, 1, 20),
        ];

        for config in configs {
            let encoded = config.encode().expect("audio config should encode");
            let decoded = AudioStreamConfig::decode(&encoded).expect("audio config should decode");
            assert_eq!(decoded, config);
        }
    }

    #[test]
    fn derived_audio_stream_ids_use_documented_offsets() {
        let session_id = u32::MAX - 50;

        assert_eq!(
            remote_microphone_audio_stream_id(session_id),
            session_id.wrapping_add(REMOTE_MICROPHONE_AUDIO_STREAM_OFFSET)
        );
        assert_eq!(
            remote_system_audio_stream_id(session_id),
            session_id.wrapping_add(REMOTE_SYSTEM_AUDIO_STREAM_OFFSET)
        );
        assert_eq!(
            viewer_talkback_audio_stream_id(session_id),
            session_id.wrapping_add(VIEWER_TALKBACK_AUDIO_STREAM_OFFSET)
        );
    }

    #[test]
    fn audio_stream_config_envelope_uses_interactive_control_lane() {
        let config = AudioStreamConfig::remote_microphone(22, 48_000, 1, 20);
        let envelope = DataEnvelope::audio_stream_config(&config, 7, 1_234)
            .expect("audio stream config envelope should encode");

        assert_eq!(envelope.header.lane, DataLane::InteractiveControl);
        assert_eq!(envelope.header.kind, ContentKind::AudioStreamConfig);
        assert_eq!(envelope.header.stream_id, config.stream_id);
        assert_eq!(envelope.header.sequence_number, 7);
        assert_eq!(envelope.header.timestamp_ms, 1_234);
        assert_eq!(envelope.header.priority, DataPriority::Interactive);
        assert_eq!(envelope.header.reliability, ReliabilityMode::BestEffort);
        assert_eq!(
            AudioStreamConfig::decode(&envelope.payload).expect("payload should decode"),
            config
        );
    }

    #[test]
    fn file_transfer_manifest_roundtrips() {
        let manifest = FileTransferManifest {
            transfer_id: 11,
            file_object_id: 12,
            name: "clip.mov".to_string(),
            group: Some(FileTransferGroup {
                group_id: 77,
                file_index: 0,
                file_count: 1,
                relative_path: "clip.mov".to_string(),
                group_total_size_bytes: 1_048_576,
                group_checksum_crc32: 0xabcd_1234,
            }),
            mime_type: Some("video/quicktime".to_string()),
            size_bytes: 1_048_576,
            chunk_payload_len: 16 * 1024,
            checksum_crc32: 0x1234_abcd,
        };

        let encoded = manifest.encode().expect("manifest should encode");
        let decoded = FileTransferManifest::decode(&encoded).expect("manifest should decode");

        assert_eq!(decoded, manifest);
    }

    #[test]
    fn file_transfer_control_roundtrips() {
        let transfer = FileTransferControl::CancelTransfer {
            transfer_id: 11,
            file_object_id: 12,
        };
        let encoded = transfer.encode().expect("control should encode");
        assert_eq!(
            FileTransferControl::decode(&encoded).expect("control should decode"),
            transfer
        );

        let group = FileTransferControl::CancelGroup { group_id: 77 };
        let encoded = group.encode().expect("group control should encode");
        assert_eq!(
            FileTransferControl::decode(&encoded).expect("group control should decode"),
            group
        );
    }

    #[test]
    fn realtime_video_envelope_roundtrips() {
        let envelope =
            DataEnvelope::realtime_video(10, 44, 1_000, 1_016, vec![0, 0, 0, 1, 0x26, 0x01]);

        let decoded = roundtrip_envelope(envelope.clone());

        assert_eq!(decoded, envelope);
        assert!(decoded.is_realtime());
        assert!(decoded.allows_stale_drop());
        assert_eq!(decoded.header.priority, DataPriority::Realtime);
        assert_eq!(decoded.header.reliability, ReliabilityMode::BestEffort);
        assert_eq!(decoded.header.deadline_ms, Some(1_016));
        assert!(decoded.header.chunk.is_none());
    }

    #[test]
    fn realtime_envelope_size_overhead_stays_small() {
        let payload = vec![0x55; 1_400];
        let rtp = RtpPacket {
            header: RtpHeader {
                version: 2,
                payload_type: PayloadType::VideoH265 as u8,
                sequence_number: 44,
                timestamp: 1_000,
                ssrc: 10,
            },
            payload: payload.clone(),
        };
        let envelope = DataEnvelope::realtime_video(10, 44, 1_000, 1_016, payload);

        let rtp_len = rtp.encode().expect("rtp should encode").len();
        let envelope_len = envelope
            .encode()
            .expect("data envelope should encode")
            .len();

        assert!(
            envelope_len <= rtp_len + 40,
            "realtime envelope overhead grew too large: rtp={rtp_len}, envelope={envelope_len}"
        );
    }

    #[test]
    fn compact_realtime_envelope_roundtrips() {
        let envelope =
            DataEnvelope::realtime_video(10, 44, 1_000, 1_016, vec![0, 0, 0, 1, 0x26, 0x01]);

        let encoded = envelope
            .encode_compact_realtime()
            .expect("compact realtime envelope should encode");
        let decoded = DataEnvelope::decode_compact_realtime(&encoded)
            .expect("compact realtime envelope should decode");

        assert_eq!(
            encoded.len(),
            COMPACT_REALTIME_HEADER_LEN + envelope.payload.len()
        );
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn compact_audio_stream_config_envelope_roundtrips() {
        let config = AudioStreamConfig::viewer_microphone_talkback(31, 48_000, 1, 20);
        let envelope = DataEnvelope::audio_stream_config(&config, 2, 9_000)
            .expect("audio config envelope should encode");

        let encoded = envelope
            .encode_compact_realtime()
            .expect("audio config should stay on compact realtime path");
        let decoded = DataEnvelope::decode_compact_realtime(&encoded)
            .expect("audio config should decode from compact realtime path");

        assert_eq!(decoded, envelope);
        assert_eq!(
            AudioStreamConfig::decode(&decoded.payload).expect("payload should decode"),
            config
        );
    }

    #[test]
    fn compact_realtime_size_overhead_is_close_to_current_rtp_path() {
        let payload = vec![0x55; 1_400];
        let rtp = RtpPacket {
            header: RtpHeader {
                version: 2,
                payload_type: PayloadType::VideoH265 as u8,
                sequence_number: 44,
                timestamp: 1_000,
                ssrc: 10,
            },
            payload: payload.clone(),
        };
        let envelope = DataEnvelope::realtime_video(10, 44, 1_000, 1_016, payload);

        let rtp_len = rtp.encode().expect("rtp should encode").len();
        let compact_len = envelope
            .encode_compact_realtime()
            .expect("compact realtime envelope should encode")
            .len();

        assert!(
            compact_len <= rtp_len + 8,
            "compact realtime overhead grew too large: rtp={rtp_len}, compact={compact_len}"
        );
    }

    #[test]
    fn compact_realtime_rejects_reliable_object_metadata() {
        let envelope = DataEnvelope::reliable_object_chunk(
            ContentKind::FileChunk,
            77,
            3,
            2_000,
            ChunkInfo {
                object_id: 99,
                chunk_index: 2,
                total_chunks: 8,
                offset: 32_768,
                total_size: 131_072,
            },
            Some(0x1234_abcd),
            vec![1, 2, 3, 4],
        );

        assert!(matches!(
            envelope.encode_compact_realtime(),
            Err(CompactRealtimeError::NonRealtimeLane(
                DataLane::ReliableObject
            ))
        ));
    }

    #[test]
    fn compact_realtime_rejects_deadlines_before_timestamps() {
        let mut envelope = DataEnvelope::realtime_video(10, 44, 1_000, 1_016, vec![1]);
        envelope.header.deadline_ms = Some(999);

        assert!(matches!(
            envelope.encode_compact_realtime(),
            Err(CompactRealtimeError::DeadlineBeforeTimestamp { .. })
        ));
    }

    #[test]
    fn reliable_file_chunk_envelope_roundtrips() {
        let chunk = ChunkInfo {
            object_id: 99,
            chunk_index: 2,
            total_chunks: 8,
            offset: 32_768,
            total_size: 131_072,
        };
        let envelope = DataEnvelope::reliable_object_chunk(
            ContentKind::FileChunk,
            77,
            3,
            2_000,
            chunk.clone(),
            Some(0x1234_abcd),
            vec![1, 2, 3, 4],
        );

        let decoded = roundtrip_envelope(envelope.clone());

        assert_eq!(decoded, envelope);
        assert!(!decoded.is_realtime());
        assert!(!decoded.allows_stale_drop());
        assert_eq!(decoded.header.priority, DataPriority::Normal);
        assert_eq!(decoded.header.reliability, ReliabilityMode::Reliable);
        assert_eq!(decoded.header.chunk, Some(chunk));
        assert_eq!(
            decoded.header.reliability_info,
            Some(ReliabilityInfo {
                ack_id: 3,
                retry_count: 0,
                checksum_crc32: Some(0x1234_abcd),
            })
        );
    }

    #[test]
    fn clipboard_bundle_roundtrips_text_image_and_file_items() {
        let bundle = ClipboardBundle::new(
            42,
            vec![
                ClipboardItem::Text(ClipboardText {
                    text: "hello clipboard".to_string(),
                }),
                ClipboardItem::Image(ClipboardImage {
                    mime_type: "image/png".to_string(),
                    width: Some(2),
                    height: Some(1),
                    bytes: vec![0x89, b'P', b'N', b'G'],
                }),
                ClipboardItem::File(ClipboardFile {
                    name: "note.txt".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    bytes: b"file bytes".to_vec(),
                }),
            ],
        );

        let encoded = bundle.encode().expect("clipboard bundle should encode");
        let decoded = ClipboardBundle::decode(&encoded).expect("clipboard bundle should decode");

        assert_eq!(decoded, bundle);
    }

    #[test]
    fn rtp_packet_roundtrips() {
        let packet = RtpPacket {
            header: RtpHeader {
                version: 2,
                payload_type: PayloadType::VideoH265 as u8,
                sequence_number: u16::MAX,
                timestamp: 123_456,
                ssrc: 0xfeed_beef,
            },
            payload: vec![0, 0, 0, 1, 42, 43, 44],
        };

        let encoded = packet.encode().expect("rtp packet should encode");
        let decoded = RtpPacket::decode(&encoded).expect("rtp packet should decode");

        assert_eq!(decoded, packet);
    }

    #[test]
    fn start_stream_control_message_roundtrips() {
        let decoded = roundtrip_control(ControlMessage::StartStream {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 8_000,
            session_id: 99,
        });

        match decoded {
            ControlMessage::StartStream {
                width,
                height,
                fps,
                bitrate_kbps,
                session_id,
            } => {
                assert_eq!(width, 1920);
                assert_eq!(height, 1080);
                assert_eq!(fps, 60);
                assert_eq!(bitrate_kbps, 8_000);
                assert_eq!(session_id, 99);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn host_telemetry_control_message_roundtrips() {
        let decoded = roundtrip_control(ControlMessage::HostTelemetry {
            fps: 59.94,
            encode_latency_ms: 4.5,
            jitter_ms: 1.25,
            bitrate_kbps: 12_000,
        });

        match decoded {
            ControlMessage::HostTelemetry {
                fps,
                encode_latency_ms,
                jitter_ms,
                bitrate_kbps,
            } => {
                assert_eq!(fps, 59.94);
                assert_eq!(encode_latency_ms, 4.5);
                assert_eq!(jitter_ms, 1.25);
                assert_eq!(bitrate_kbps, 12_000);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn request_keyframe_control_message_roundtrips() {
        let decoded = roundtrip_control(ControlMessage::RequestKeyframe { session_id: 12345 });
        match decoded {
            ControlMessage::RequestKeyframe { session_id } => {
                assert_eq!(session_id, 12345);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn audio_control_message_roundtrips() {
        let decoded = roundtrip_control(ControlMessage::AudioControl {
            session_id: 77,
            target: AudioControlTarget::ViewerTalkbackPlayback,
            muted: true,
            volume_percent: 65,
        });

        match decoded {
            ControlMessage::AudioControl {
                session_id,
                target,
                muted,
                volume_percent,
            } => {
                assert_eq!(session_id, 77);
                assert_eq!(target, AudioControlTarget::ViewerTalkbackPlayback);
                assert!(muted);
                assert_eq!(volume_percent, 65);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn input_control_messages_roundtrip() {
        let cases = [
            ControlMessage::Input(InputEvent::KeyDown(12)),
            ControlMessage::Input(InputEvent::KeyUp(12)),
            ControlMessage::Input(InputEvent::MouseMove { dx: -7, dy: 9 }),
            ControlMessage::Input(InputEvent::MouseMoveAbsolute {
                x: 12_345,
                y: 54_321,
            }),
            ControlMessage::Input(InputEvent::Key {
                key_code: 0x7b,
                pressed: true,
                modifiers: input_modifiers::SHIFT | input_modifiers::META,
            }),
            ControlMessage::Input(InputEvent::ModifiersChanged(input_modifiers::ALT)),
            ControlMessage::Input(InputEvent::MouseDown(1)),
            ControlMessage::Input(InputEvent::MouseUp(1)),
            ControlMessage::Input(InputEvent::MouseScroll {
                delta_x: -11,
                delta_y: 23,
            }),
        ];

        for case in cases {
            let decoded = roundtrip_control(case.clone());
            match (case, decoded) {
                (
                    ControlMessage::Input(InputEvent::KeyDown(expected)),
                    ControlMessage::Input(InputEvent::KeyDown(actual)),
                ) => {
                    assert_eq!(actual, expected);
                }
                (
                    ControlMessage::Input(InputEvent::KeyUp(expected)),
                    ControlMessage::Input(InputEvent::KeyUp(actual)),
                ) => {
                    assert_eq!(actual, expected);
                }
                (
                    ControlMessage::Input(InputEvent::MouseMove {
                        dx: expected_dx,
                        dy: expected_dy,
                    }),
                    ControlMessage::Input(InputEvent::MouseMove { dx, dy }),
                ) => {
                    assert_eq!(dx, expected_dx);
                    assert_eq!(dy, expected_dy);
                }
                (
                    ControlMessage::Input(InputEvent::MouseMoveAbsolute {
                        x: expected_x,
                        y: expected_y,
                    }),
                    ControlMessage::Input(InputEvent::MouseMoveAbsolute { x, y }),
                ) => assert_eq!((x, y), (expected_x, expected_y)),
                (
                    ControlMessage::Input(InputEvent::Key {
                        key_code: expected_code,
                        pressed: expected_pressed,
                        modifiers: expected_modifiers,
                    }),
                    ControlMessage::Input(InputEvent::Key {
                        key_code,
                        pressed,
                        modifiers,
                    }),
                ) => assert_eq!(
                    (key_code, pressed, modifiers),
                    (expected_code, expected_pressed, expected_modifiers),
                ),
                (
                    ControlMessage::Input(InputEvent::ModifiersChanged(expected)),
                    ControlMessage::Input(InputEvent::ModifiersChanged(actual)),
                ) => assert_eq!(actual, expected),
                (
                    ControlMessage::Input(InputEvent::MouseDown(expected)),
                    ControlMessage::Input(InputEvent::MouseDown(actual)),
                ) => {
                    assert_eq!(actual, expected);
                }
                (
                    ControlMessage::Input(InputEvent::MouseUp(expected)),
                    ControlMessage::Input(InputEvent::MouseUp(actual)),
                ) => {
                    assert_eq!(actual, expected);
                }
                (
                    ControlMessage::Input(InputEvent::MouseScroll {
                        delta_x: expected_x,
                        delta_y: expected_y,
                    }),
                    ControlMessage::Input(InputEvent::MouseScroll { delta_x, delta_y }),
                ) => {
                    assert_eq!(delta_x, expected_x);
                    assert_eq!(delta_y, expected_y);
                }
                (expected, actual) => {
                    panic!("roundtrip mismatch: expected {expected:?}, got {actual:?}")
                }
            }
        }
    }

    #[test]
    fn simple_control_messages_roundtrip_to_expected_variants() {
        assert!(matches!(
            roundtrip_control(ControlMessage::HandshakeReq),
            ControlMessage::HandshakeReq
        ));
        assert!(matches!(
            roundtrip_control(ControlMessage::HandshakeAck),
            ControlMessage::HandshakeAck
        ));
        assert!(matches!(
            roundtrip_control(ControlMessage::StopStream),
            ControlMessage::StopStream
        ));
        assert!(matches!(
            roundtrip_control(ControlMessage::Heartbeat),
            ControlMessage::Heartbeat
        ));
        assert!(matches!(
            roundtrip_control(ControlMessage::UpdateStreamSettings {
                width: 3840,
                height: 2160,
                fps: 120,
                bitrate_kbps: 60000,
                session_id: 123
            }),
            ControlMessage::UpdateStreamSettings { width: 3840, height: 2160, fps: 120, bitrate_kbps: 60000, session_id: 123 }
        ));
        assert!(matches!(
            roundtrip_control(ControlMessage::Ping { client_send_ts: 12345 }),
            ControlMessage::Ping { client_send_ts: 12345 }
        ));
        assert!(matches!(
            roundtrip_control(ControlMessage::Pong { client_send_ts: 12345, host_recv_ts: 12348, host_send_ts: 12349 }),
            ControlMessage::Pong { client_send_ts: 12345, host_recv_ts: 12348, host_send_ts: 12349 }
        ));
    }
}
