use protocol::{ChunkInfo, ContentKind, DataEnvelope, DataLane, DataPriority};
use std::cmp::Ordering;
use std::collections::{HashSet, VecDeque};
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimePacketSpec {
    pub lane: DataLane,
    pub stream_id: u32,
    pub sequence_number: u64,
    pub timestamp_ms: u64,
    pub deadline_ms: u64,
}

impl RealtimePacketSpec {
    pub fn video(
        stream_id: u32,
        sequence_number: u64,
        timestamp_ms: u64,
        deadline_ms: u64,
    ) -> Self {
        Self {
            lane: DataLane::RealtimeVideo,
            stream_id,
            sequence_number,
            timestamp_ms,
            deadline_ms,
        }
    }

    pub fn audio(
        stream_id: u32,
        sequence_number: u64,
        timestamp_ms: u64,
        deadline_ms: u64,
    ) -> Self {
        Self {
            lane: DataLane::RealtimeAudio,
            stream_id,
            sequence_number,
            timestamp_ms,
            deadline_ms,
        }
    }
}

pub fn make_realtime_envelope(spec: RealtimePacketSpec, payload: Vec<u8>) -> DataEnvelope {
    let kind = match spec.lane {
        DataLane::RealtimeVideo => ContentKind::VideoH265,
        DataLane::RealtimeAudio => ContentKind::AudioOpus,
        _ => panic!("realtime packet spec requires an audio or video lane"),
    };

    let mut envelope = DataEnvelope::new(
        spec.lane,
        kind,
        spec.stream_id,
        spec.sequence_number,
        spec.timestamp_ms,
        payload,
    );
    envelope.header.deadline_ms = Some(spec.deadline_ms);
    envelope
}

pub fn is_stale(envelope: &DataEnvelope, now_ms: u64) -> bool {
    envelope.allows_stale_drop()
        && envelope
            .header
            .deadline_ms
            .is_some_and(|deadline_ms| deadline_ms < now_ms)
}

pub fn realtime_send_order(left: &DataEnvelope, right: &DataEnvelope) -> Ordering {
    let priority_order = right.header.priority.cmp(&left.header.priority);
    if priority_order != Ordering::Equal {
        return priority_order;
    }

    let left_deadline = left.header.deadline_ms.unwrap_or(u64::MAX);
    let right_deadline = right.header.deadline_ms.unwrap_or(u64::MAX);
    let deadline_order = left_deadline.cmp(&right_deadline);
    if deadline_order != Ordering::Equal {
        return deadline_order;
    }

    left.header
        .sequence_number
        .cmp(&right.header.sequence_number)
}

pub fn should_preempt_bulk(realtime: &DataEnvelope, bulk: &DataEnvelope) -> bool {
    realtime.header.priority > bulk.header.priority
        && realtime.header.lane.is_realtime()
        && !bulk.header.lane.is_realtime()
}

pub fn default_realtime_priority(lane: DataLane) -> DataPriority {
    match lane {
        DataLane::RealtimeVideo | DataLane::RealtimeAudio => DataPriority::Realtime,
        DataLane::InteractiveControl => DataPriority::Interactive,
        DataLane::ReliableObject => DataPriority::Normal,
        DataLane::Background => DataPriority::Background,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SchedulerStats {
    pub queued_realtime: usize,
    pub queued_reliable: usize,
    pub dropped_stale_realtime: u64,
    pub dropped_realtime_capacity: u64,
    pub rejected_reliable_capacity: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneSchedulerConfig {
    pub max_realtime_queued: usize,
    pub max_reliable_queued: usize,
}

impl Default for LaneSchedulerConfig {
    fn default() -> Self {
        Self {
            max_realtime_queued: 256,
            max_reliable_queued: 64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerPushResult {
    Queued,
    DroppedStaleRealtime,
    DroppedRealtimeForCapacity,
    RejectedReliableForCapacity,
}

pub struct LaneScheduler {
    realtime: Vec<DataEnvelope>,
    reliable: VecDeque<DataEnvelope>,
    config: LaneSchedulerConfig,
    dropped_stale_realtime: u64,
    dropped_realtime_capacity: u64,
    rejected_reliable_capacity: u64,
}

impl LaneScheduler {
    pub fn new() -> Self {
        Self::with_config(LaneSchedulerConfig::default())
    }

    pub fn with_config(config: LaneSchedulerConfig) -> Self {
        Self {
            realtime: Vec::new(),
            reliable: VecDeque::new(),
            config,
            dropped_stale_realtime: 0,
            dropped_realtime_capacity: 0,
            rejected_reliable_capacity: 0,
        }
    }

    pub fn push(&mut self, envelope: DataEnvelope, now_ms: u64) -> SchedulerPushResult {
        if envelope.header.lane.is_realtime() {
            if is_stale(&envelope, now_ms) {
                self.dropped_stale_realtime += 1;
                return SchedulerPushResult::DroppedStaleRealtime;
            }

            let insert_at = self
                .realtime
                .binary_search_by(|queued| realtime_send_order(queued, &envelope))
                .unwrap_or_else(|index| index);
            self.realtime.insert(insert_at, envelope);

            if self.realtime.len() > self.config.max_realtime_queued {
                self.realtime.pop();
                self.dropped_realtime_capacity += 1;
                return SchedulerPushResult::DroppedRealtimeForCapacity;
            }
        } else {
            if self.reliable.len() >= self.config.max_reliable_queued {
                self.rejected_reliable_capacity += 1;
                return SchedulerPushResult::RejectedReliableForCapacity;
            }
            self.reliable.push_back(envelope);
        }

        SchedulerPushResult::Queued
    }

    pub fn pop_next(&mut self, now_ms: u64) -> Option<DataEnvelope> {
        while let Some(envelope) = self.realtime.first() {
            if !is_stale(envelope, now_ms) {
                break;
            }

            self.realtime.remove(0);
            self.dropped_stale_realtime += 1;
        }

        if !self.realtime.is_empty() {
            return Some(self.realtime.remove(0));
        }

        self.reliable.pop_front()
    }

    pub fn stats(&self) -> SchedulerStats {
        SchedulerStats {
            queued_realtime: self.realtime.len(),
            queued_reliable: self.reliable.len(),
            dropped_stale_realtime: self.dropped_stale_realtime,
            dropped_realtime_capacity: self.dropped_realtime_capacity,
            rejected_reliable_capacity: self.rejected_reliable_capacity,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.realtime.is_empty() && self.reliable.is_empty()
    }
}

impl Default for LaneScheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectProgress {
    pub object_id: u64,
    pub received_chunks: u32,
    pub total_chunks: u32,
    pub received_bytes: u64,
    pub total_size: u64,
    pub is_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedObject {
    pub object_id: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectTransferError {
    MissingChunkMetadata,
    WrongObject { expected: u64, actual: u64 },
    InvalidChunkIndex { chunk_index: u32, total_chunks: u32 },
    InvalidOffset { expected: u64, actual: u64 },
    TotalSizeMismatch { expected: u64, actual: u64 },
    ChecksumMismatch { expected: u32, actual: u32 },
    Cancelled,
}

impl fmt::Display for ObjectTransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ObjectTransferError::MissingChunkMetadata => write!(f, "missing chunk metadata"),
            ObjectTransferError::WrongObject { expected, actual } => {
                write!(f, "wrong object id: expected {expected}, got {actual}")
            }
            ObjectTransferError::InvalidChunkIndex {
                chunk_index,
                total_chunks,
            } => write!(
                f,
                "invalid chunk index {chunk_index} for total chunk count {total_chunks}"
            ),
            ObjectTransferError::InvalidOffset { expected, actual } => {
                write!(f, "invalid chunk offset: expected {expected}, got {actual}")
            }
            ObjectTransferError::TotalSizeMismatch { expected, actual } => {
                write!(f, "total size mismatch: expected {expected}, got {actual}")
            }
            ObjectTransferError::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "checksum mismatch: expected {expected:#x}, got {actual:#x}"
                )
            }
            ObjectTransferError::Cancelled => write!(f, "object transfer cancelled"),
        }
    }
}

impl Error for ObjectTransferError {}

pub struct ReliableObjectAssembler {
    object_id: u64,
    total_chunks: u32,
    total_size: u64,
    expected_checksum_crc32: Option<u32>,
    chunks: Vec<Option<Vec<u8>>>,
    received_indices: HashSet<u32>,
    received_bytes: u64,
    cancelled: bool,
}

impl ReliableObjectAssembler {
    pub fn new(
        object_id: u64,
        total_chunks: u32,
        total_size: u64,
        expected_checksum_crc32: Option<u32>,
    ) -> Self {
        Self {
            object_id,
            total_chunks,
            total_size,
            expected_checksum_crc32,
            chunks: vec![None; total_chunks as usize],
            received_indices: HashSet::new(),
            received_bytes: 0,
            cancelled: false,
        }
    }

    pub fn push_chunk(
        &mut self,
        envelope: DataEnvelope,
    ) -> Result<ObjectProgress, ObjectTransferError> {
        if self.cancelled {
            return Err(ObjectTransferError::Cancelled);
        }

        let chunk = envelope
            .header
            .chunk
            .ok_or(ObjectTransferError::MissingChunkMetadata)?;
        self.validate_chunk(&chunk, envelope.payload.len() as u64)?;

        if self.received_indices.insert(chunk.chunk_index) {
            self.received_bytes += envelope.payload.len() as u64;
            self.chunks[chunk.chunk_index as usize] = Some(envelope.payload);
        }

        Ok(self.progress())
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    pub fn progress(&self) -> ObjectProgress {
        ObjectProgress {
            object_id: self.object_id,
            received_chunks: self.received_indices.len() as u32,
            total_chunks: self.total_chunks,
            received_bytes: self.received_bytes,
            total_size: self.total_size,
            is_complete: self.received_indices.len() as u32 == self.total_chunks,
        }
    }

    pub fn finish(self) -> Result<CompletedObject, ObjectTransferError> {
        if self.cancelled {
            return Err(ObjectTransferError::Cancelled);
        }

        let mut bytes = Vec::with_capacity(self.total_size as usize);
        for chunk in self.chunks {
            let chunk = chunk.ok_or(ObjectTransferError::TotalSizeMismatch {
                expected: self.total_size,
                actual: bytes.len() as u64,
            })?;
            bytes.extend_from_slice(&chunk);
        }

        if bytes.len() as u64 != self.total_size {
            return Err(ObjectTransferError::TotalSizeMismatch {
                expected: self.total_size,
                actual: bytes.len() as u64,
            });
        }

        if let Some(expected) = self.expected_checksum_crc32 {
            let actual = checksum_crc32(&bytes);
            if actual != expected {
                return Err(ObjectTransferError::ChecksumMismatch { expected, actual });
            }
        }

        Ok(CompletedObject {
            object_id: self.object_id,
            bytes,
        })
    }

    fn validate_chunk(
        &self,
        chunk: &ChunkInfo,
        payload_len: u64,
    ) -> Result<(), ObjectTransferError> {
        if chunk.object_id != self.object_id {
            return Err(ObjectTransferError::WrongObject {
                expected: self.object_id,
                actual: chunk.object_id,
            });
        }

        if chunk.total_chunks != self.total_chunks || chunk.chunk_index >= self.total_chunks {
            return Err(ObjectTransferError::InvalidChunkIndex {
                chunk_index: chunk.chunk_index,
                total_chunks: chunk.total_chunks,
            });
        }

        if chunk.total_size != self.total_size {
            return Err(ObjectTransferError::TotalSizeMismatch {
                expected: self.total_size,
                actual: chunk.total_size,
            });
        }

        let expected_offset = self
            .chunks
            .iter()
            .take(chunk.chunk_index as usize)
            .map(|maybe_chunk| maybe_chunk.as_ref().map_or(0, |chunk| chunk.len() as u64))
            .sum::<u64>();

        if chunk.offset < expected_offset {
            return Err(ObjectTransferError::InvalidOffset {
                expected: expected_offset,
                actual: chunk.offset,
            });
        }

        if chunk.offset + payload_len > self.total_size {
            return Err(ObjectTransferError::TotalSizeMismatch {
                expected: self.total_size,
                actual: chunk.offset + payload_len,
            });
        }

        Ok(())
    }
}

pub fn checksum_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectChunkSpec {
    pub object_id: u64,
    pub stream_id: u32,
    pub sequence_number: u64,
    pub timestamp_ms: u64,
    pub chunk_index: u32,
    pub total_chunks: u32,
    pub offset: u64,
    pub total_size: u64,
    pub checksum_crc32: Option<u32>,
}

pub fn object_chunk_envelope(spec: ObjectChunkSpec, payload: Vec<u8>) -> DataEnvelope {
    DataEnvelope::reliable_object_chunk(
        ContentKind::FileChunk,
        spec.stream_id,
        spec.sequence_number,
        spec.timestamp_ms,
        ChunkInfo {
            object_id: spec.object_id,
            chunk_index: spec.chunk_index,
            total_chunks: spec.total_chunks,
            offset: spec.offset,
            total_size: spec.total_size,
        },
        spec.checksum_crc32,
        payload,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ContentKind;

    #[test]
    fn builds_video_realtime_envelope() {
        let envelope = make_realtime_envelope(
            RealtimePacketSpec::video(7, 11, 1_000, 1_016),
            vec![1, 2, 3],
        );

        assert_eq!(envelope.header.lane, DataLane::RealtimeVideo);
        assert_eq!(envelope.header.kind, ContentKind::VideoH265);
        assert_eq!(envelope.header.priority, DataPriority::Realtime);
        assert_eq!(envelope.header.deadline_ms, Some(1_016));
        assert_eq!(envelope.payload, vec![1, 2, 3]);
    }

    #[test]
    fn stale_detection_only_applies_to_realtime_drop_lanes() {
        let realtime =
            make_realtime_envelope(RealtimePacketSpec::audio(1, 2, 1_000, 1_010), vec![9]);
        let reliable = DataEnvelope::reliable_object_chunk(
            ContentKind::FileChunk,
            1,
            2,
            1_000,
            ChunkInfo {
                object_id: 1,
                chunk_index: 0,
                total_chunks: 1,
                offset: 0,
                total_size: 1,
            },
            None,
            vec![9],
        );

        assert!(is_stale(&realtime, 1_011));
        assert!(!is_stale(&realtime, 1_010));
        assert!(!is_stale(&reliable, 9_999));
    }

    #[test]
    fn realtime_order_prefers_priority_then_deadline_then_sequence() {
        let earlier =
            make_realtime_envelope(RealtimePacketSpec::video(1, 10, 1_000, 1_010), vec![]);
        let later = make_realtime_envelope(RealtimePacketSpec::video(1, 9, 1_000, 1_020), vec![]);

        assert_eq!(realtime_send_order(&earlier, &later), Ordering::Less);

        let bulk = DataEnvelope::reliable_object_chunk(
            ContentKind::FileChunk,
            1,
            1,
            1_000,
            ChunkInfo {
                object_id: 1,
                chunk_index: 0,
                total_chunks: 1,
                offset: 0,
                total_size: 0,
            },
            None,
            vec![],
        );

        assert_eq!(realtime_send_order(&earlier, &bulk), Ordering::Less);
    }

    #[test]
    fn realtime_packets_preempt_bulk_packets() {
        let realtime =
            make_realtime_envelope(RealtimePacketSpec::video(1, 1, 1_000, 1_016), vec![]);
        let bulk = DataEnvelope::reliable_object_chunk(
            ContentKind::FileChunk,
            1,
            1,
            1_000,
            ChunkInfo {
                object_id: 2,
                chunk_index: 0,
                total_chunks: 1,
                offset: 0,
                total_size: 0,
            },
            None,
            vec![],
        );

        assert!(should_preempt_bulk(&realtime, &bulk));
        assert!(!should_preempt_bulk(&bulk, &realtime));
    }

    #[test]
    fn scheduler_sends_realtime_before_reliable_objects() {
        let mut scheduler = LaneScheduler::new();
        let bulk = object_chunk_envelope(
            ObjectChunkSpec {
                object_id: 9,
                stream_id: 1,
                sequence_number: 1,
                timestamp_ms: 1_000,
                chunk_index: 0,
                total_chunks: 1,
                offset: 0,
                total_size: 4,
                checksum_crc32: None,
            },
            vec![9, 9, 9, 9],
        );
        let realtime =
            make_realtime_envelope(RealtimePacketSpec::video(1, 2, 1_000, 1_016), vec![1]);

        scheduler.push(bulk.clone(), 1_000);
        scheduler.push(realtime.clone(), 1_000);

        assert_eq!(scheduler.pop_next(1_000), Some(realtime));
        assert_eq!(scheduler.pop_next(1_000), Some(bulk));
        assert!(scheduler.is_empty());
    }

    #[test]
    fn scheduler_orders_realtime_by_priority_deadline_then_sequence() {
        let mut scheduler = LaneScheduler::new();
        let later =
            make_realtime_envelope(RealtimePacketSpec::video(1, 20, 1_000, 1_040), vec![20]);
        let earlier =
            make_realtime_envelope(RealtimePacketSpec::video(1, 30, 1_000, 1_020), vec![30]);
        let control = DataEnvelope::new(
            DataLane::InteractiveControl,
            ContentKind::Control,
            1,
            1,
            1_000,
            vec![1],
        );

        scheduler.push(later.clone(), 1_000);
        scheduler.push(control.clone(), 1_000);
        scheduler.push(earlier.clone(), 1_000);

        assert_eq!(scheduler.pop_next(1_000), Some(earlier));
        assert_eq!(scheduler.pop_next(1_000), Some(later));
        assert_eq!(scheduler.pop_next(1_000), Some(control));
    }

    #[test]
    fn scheduler_drops_stale_realtime_without_blocking_reliable_objects() {
        let mut scheduler = LaneScheduler::new();
        let stale = make_realtime_envelope(RealtimePacketSpec::video(1, 1, 1_000, 1_010), vec![1]);
        let bulk = object_chunk_envelope(
            ObjectChunkSpec {
                object_id: 9,
                stream_id: 1,
                sequence_number: 2,
                timestamp_ms: 1_000,
                chunk_index: 0,
                total_chunks: 1,
                offset: 0,
                total_size: 1,
                checksum_crc32: None,
            },
            vec![2],
        );

        scheduler.push(stale, 1_011);
        scheduler.push(bulk.clone(), 1_011);

        assert_eq!(scheduler.stats().dropped_stale_realtime, 1);
        assert_eq!(scheduler.pop_next(1_011), Some(bulk));
        assert!(scheduler.is_empty());
    }

    #[test]
    fn scheduler_drops_realtime_that_expires_while_queued() {
        let mut scheduler = LaneScheduler::new();
        let stale = make_realtime_envelope(RealtimePacketSpec::video(1, 1, 1_000, 1_010), vec![1]);
        let fresh = make_realtime_envelope(RealtimePacketSpec::video(1, 2, 1_000, 1_020), vec![2]);

        scheduler.push(stale, 1_000);
        scheduler.push(fresh.clone(), 1_000);

        assert_eq!(scheduler.pop_next(1_011), Some(fresh));
        assert_eq!(scheduler.stats().dropped_stale_realtime, 1);
    }

    #[test]
    fn scheduler_rejects_reliable_objects_when_bulk_queue_is_full() {
        let mut scheduler = LaneScheduler::with_config(LaneSchedulerConfig {
            max_realtime_queued: 8,
            max_reliable_queued: 1,
        });
        let first = object_chunk_envelope(
            ObjectChunkSpec {
                object_id: 9,
                stream_id: 1,
                sequence_number: 1,
                timestamp_ms: 1_000,
                chunk_index: 0,
                total_chunks: 1,
                offset: 0,
                total_size: 1,
                checksum_crc32: None,
            },
            vec![1],
        );
        let second = object_chunk_envelope(
            ObjectChunkSpec {
                object_id: 10,
                stream_id: 1,
                sequence_number: 2,
                timestamp_ms: 1_000,
                chunk_index: 0,
                total_chunks: 1,
                offset: 0,
                total_size: 1,
                checksum_crc32: None,
            },
            vec![2],
        );

        assert_eq!(
            scheduler.push(first.clone(), 1_000),
            SchedulerPushResult::Queued
        );
        assert_eq!(
            scheduler.push(second, 1_000),
            SchedulerPushResult::RejectedReliableForCapacity
        );

        assert_eq!(scheduler.stats().queued_reliable, 1);
        assert_eq!(scheduler.stats().rejected_reliable_capacity, 1);
        assert_eq!(scheduler.pop_next(1_000), Some(first));
    }

    #[test]
    fn scheduler_drops_lowest_urgency_realtime_when_realtime_queue_is_full() {
        let mut scheduler = LaneScheduler::with_config(LaneSchedulerConfig {
            max_realtime_queued: 2,
            max_reliable_queued: 8,
        });
        let urgent = make_realtime_envelope(RealtimePacketSpec::video(1, 1, 1_000, 1_010), vec![1]);
        let mid = make_realtime_envelope(RealtimePacketSpec::video(1, 2, 1_000, 1_020), vec![2]);
        let late = make_realtime_envelope(RealtimePacketSpec::video(1, 3, 1_000, 1_030), vec![3]);

        assert_eq!(scheduler.push(late, 1_000), SchedulerPushResult::Queued);
        assert_eq!(
            scheduler.push(urgent.clone(), 1_000),
            SchedulerPushResult::Queued
        );
        assert_eq!(
            scheduler.push(mid.clone(), 1_000),
            SchedulerPushResult::DroppedRealtimeForCapacity
        );

        assert_eq!(scheduler.stats().queued_realtime, 2);
        assert_eq!(scheduler.stats().dropped_realtime_capacity, 1);
        assert_eq!(scheduler.pop_next(1_000), Some(urgent));
        assert_eq!(scheduler.pop_next(1_000), Some(mid));
        assert!(scheduler.is_empty());
    }

    #[test]
    fn assembles_complete_object_transfer() {
        let bytes = b"hello reliable world".to_vec();
        let checksum = checksum_crc32(&bytes);
        let mut assembler = ReliableObjectAssembler::new(5, 2, bytes.len() as u64, Some(checksum));

        let progress = assembler
            .push_chunk(object_chunk_envelope(
                ObjectChunkSpec {
                    object_id: 5,
                    stream_id: 1,
                    sequence_number: 1,
                    timestamp_ms: 1_000,
                    chunk_index: 0,
                    total_chunks: 2,
                    offset: 0,
                    total_size: bytes.len() as u64,
                    checksum_crc32: Some(checksum),
                },
                bytes[..6].to_vec(),
            ))
            .expect("first chunk should be accepted");
        assert_eq!(progress.received_chunks, 1);
        assert!(!progress.is_complete);

        let progress = assembler
            .push_chunk(object_chunk_envelope(
                ObjectChunkSpec {
                    object_id: 5,
                    stream_id: 1,
                    sequence_number: 2,
                    timestamp_ms: 1_000,
                    chunk_index: 1,
                    total_chunks: 2,
                    offset: 6,
                    total_size: bytes.len() as u64,
                    checksum_crc32: Some(checksum),
                },
                bytes[6..].to_vec(),
            ))
            .expect("second chunk should be accepted");
        assert!(progress.is_complete);

        let completed = assembler.finish().expect("object should finish");
        assert_eq!(completed.object_id, 5);
        assert_eq!(completed.bytes, bytes);
    }

    #[test]
    fn duplicate_chunks_do_not_advance_progress_twice() {
        let mut assembler = ReliableObjectAssembler::new(5, 1, 3, None);
        let chunk = object_chunk_envelope(
            ObjectChunkSpec {
                object_id: 5,
                stream_id: 1,
                sequence_number: 1,
                timestamp_ms: 1_000,
                chunk_index: 0,
                total_chunks: 1,
                offset: 0,
                total_size: 3,
                checksum_crc32: None,
            },
            vec![1, 2, 3],
        );

        let first = assembler
            .push_chunk(chunk.clone())
            .expect("first chunk should be accepted");
        let duplicate = assembler
            .push_chunk(chunk)
            .expect("duplicate chunk should be ignored");

        assert_eq!(first.received_chunks, 1);
        assert_eq!(duplicate.received_chunks, 1);
        assert_eq!(duplicate.received_bytes, 3);
    }

    #[test]
    fn missing_chunk_prevents_finish() {
        let mut assembler = ReliableObjectAssembler::new(5, 2, 4, None);
        assembler
            .push_chunk(object_chunk_envelope(
                ObjectChunkSpec {
                    object_id: 5,
                    stream_id: 1,
                    sequence_number: 1,
                    timestamp_ms: 1_000,
                    chunk_index: 0,
                    total_chunks: 2,
                    offset: 0,
                    total_size: 4,
                    checksum_crc32: None,
                },
                vec![1, 2],
            ))
            .expect("first chunk should be accepted");

        assert!(matches!(
            assembler.finish(),
            Err(ObjectTransferError::TotalSizeMismatch { .. })
        ));
    }

    #[test]
    fn cancel_rejects_more_chunks_and_finish() {
        let mut assembler = ReliableObjectAssembler::new(5, 1, 1, None);
        assembler.cancel();

        assert_eq!(
            assembler
                .push_chunk(object_chunk_envelope(
                    ObjectChunkSpec {
                        object_id: 5,
                        stream_id: 1,
                        sequence_number: 1,
                        timestamp_ms: 1_000,
                        chunk_index: 0,
                        total_chunks: 1,
                        offset: 0,
                        total_size: 1,
                        checksum_crc32: None,
                    },
                    vec![1],
                ))
                .expect_err("cancelled transfer should reject chunks"),
            ObjectTransferError::Cancelled
        );
        assert_eq!(
            assembler
                .finish()
                .expect_err("cancelled transfer should not finish"),
            ObjectTransferError::Cancelled
        );
    }

    #[test]
    fn checksum_failure_is_reported() {
        let mut assembler = ReliableObjectAssembler::new(5, 1, 3, Some(0));
        assembler
            .push_chunk(object_chunk_envelope(
                ObjectChunkSpec {
                    object_id: 5,
                    stream_id: 1,
                    sequence_number: 1,
                    timestamp_ms: 1_000,
                    chunk_index: 0,
                    total_chunks: 1,
                    offset: 0,
                    total_size: 3,
                    checksum_crc32: Some(0),
                },
                vec![1, 2, 3],
            ))
            .expect("chunk should be accepted");

        assert!(matches!(
            assembler.finish(),
            Err(ObjectTransferError::ChecksumMismatch { .. })
        ));
    }
}
