use crate::data_plane::{ObjectChunkSpec, checksum_crc32, object_chunk_envelope};
use crate::data_plane::{ObjectProgress, ObjectTransferError, ReliableObjectAssembler};
use protocol::{ClipboardBundle, ClipboardItem, ContentKind, DataEnvelope};
use std::error::Error;
use std::fmt;

pub const DEFAULT_CLIPBOARD_CHUNK_PAYLOAD_LEN: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardItemClass {
    Text,
    Image,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardSyncPolicy {
    pub allow_text: bool,
    pub allow_images: bool,
    pub allow_files: bool,
    pub max_text_bytes: usize,
    pub max_image_bytes: usize,
    pub max_file_bytes: usize,
    pub max_bundle_bytes: usize,
}

impl Default for ClipboardSyncPolicy {
    fn default() -> Self {
        Self {
            allow_text: true,
            allow_images: true,
            allow_files: false,
            max_text_bytes: 1024 * 1024,
            max_image_bytes: 64 * 1024 * 1024,
            max_file_bytes: 256 * 1024 * 1024,
            max_bundle_bytes: 512 * 1024 * 1024,
        }
    }
}

impl ClipboardSyncPolicy {
    pub fn with_files_allowed(mut self) -> Self {
        self.allow_files = true;
        self
    }

    pub fn validate_bundle(&self, bundle: &ClipboardBundle) -> Result<(), ClipboardPlaneError> {
        for (item_index, item) in bundle.items.iter().enumerate() {
            match item {
                ClipboardItem::Text(text) => {
                    self.validate_item_size(
                        item_index,
                        ClipboardItemClass::Text,
                        self.allow_text,
                        text.text.len(),
                        self.max_text_bytes,
                    )?;
                }
                ClipboardItem::Image(image) => {
                    self.validate_item_size(
                        item_index,
                        ClipboardItemClass::Image,
                        self.allow_images,
                        image.bytes.len(),
                        self.max_image_bytes,
                    )?;
                }
                ClipboardItem::File(file) => {
                    self.validate_item_size(
                        item_index,
                        ClipboardItemClass::File,
                        self.allow_files,
                        file.bytes.len(),
                        self.max_file_bytes,
                    )?;
                }
            }
        }

        let encoded = bundle.encode().map_err(ClipboardPlaneError::Encode)?;
        if encoded.len() > self.max_bundle_bytes {
            return Err(ClipboardPlaneError::BundleTooLarge {
                actual_bytes: encoded.len(),
                limit_bytes: self.max_bundle_bytes,
            });
        }

        Ok(())
    }

    fn validate_item_size(
        &self,
        item_index: usize,
        item_class: ClipboardItemClass,
        allowed: bool,
        actual_bytes: usize,
        limit_bytes: usize,
    ) -> Result<(), ClipboardPlaneError> {
        if !allowed {
            return Err(ClipboardPlaneError::DisallowedItem {
                item_index,
                item_class,
            });
        }

        if actual_bytes > limit_bytes {
            return Err(ClipboardPlaneError::ItemTooLarge {
                item_index,
                item_class,
                actual_bytes,
                limit_bytes,
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardEnvelopeSpec {
    pub object_id: u64,
    pub stream_id: u32,
    pub first_sequence_number: u64,
    pub timestamp_ms: u64,
    pub max_chunk_payload_len: usize,
}

impl ClipboardEnvelopeSpec {
    pub fn new(
        object_id: u64,
        stream_id: u32,
        first_sequence_number: u64,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            object_id,
            stream_id,
            first_sequence_number,
            timestamp_ms,
            max_chunk_payload_len: DEFAULT_CLIPBOARD_CHUNK_PAYLOAD_LEN,
        }
    }
}

#[derive(Debug)]
pub enum ClipboardPlaneError {
    EmptyBundle,
    InvalidImage {
        item_index: usize,
    },
    InvalidFileName {
        item_index: usize,
    },
    InvalidChunkPayloadLen,
    TooManyChunks {
        chunk_count: usize,
    },
    DisallowedItem {
        item_index: usize,
        item_class: ClipboardItemClass,
    },
    ItemTooLarge {
        item_index: usize,
        item_class: ClipboardItemClass,
        actual_bytes: usize,
        limit_bytes: usize,
    },
    BundleTooLarge {
        actual_bytes: usize,
        limit_bytes: usize,
    },
    UnexpectedContentKind(ContentKind),
    MissingChunkMetadata,
    Encode(bincode::Error),
    Decode(bincode::Error),
    ObjectTransfer(ObjectTransferError),
}

impl fmt::Display for ClipboardPlaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClipboardPlaneError::EmptyBundle => write!(f, "clipboard bundle has no items"),
            ClipboardPlaneError::InvalidImage { item_index } => {
                write!(
                    f,
                    "clipboard image item {item_index} is missing MIME type or bytes"
                )
            }
            ClipboardPlaneError::InvalidFileName { item_index } => {
                write!(f, "clipboard file item {item_index} is missing a file name")
            }
            ClipboardPlaneError::InvalidChunkPayloadLen => {
                write!(
                    f,
                    "clipboard chunk payload length must be greater than zero"
                )
            }
            ClipboardPlaneError::TooManyChunks { chunk_count } => {
                write!(
                    f,
                    "clipboard bundle requires too many chunks: {chunk_count}"
                )
            }
            ClipboardPlaneError::DisallowedItem {
                item_index,
                item_class,
            } => write!(
                f,
                "clipboard item {item_index} of type {item_class:?} is not allowed by policy"
            ),
            ClipboardPlaneError::ItemTooLarge {
                item_index,
                item_class,
                actual_bytes,
                limit_bytes,
            } => write!(
                f,
                "clipboard item {item_index} of type {item_class:?} is too large: {actual_bytes} > {limit_bytes} bytes"
            ),
            ClipboardPlaneError::BundleTooLarge {
                actual_bytes,
                limit_bytes,
            } => write!(
                f,
                "clipboard bundle is too large: {actual_bytes} > {limit_bytes} bytes"
            ),
            ClipboardPlaneError::UnexpectedContentKind(kind) => {
                write!(f, "unexpected clipboard content kind: {kind:?}")
            }
            ClipboardPlaneError::MissingChunkMetadata => {
                write!(f, "missing clipboard chunk metadata")
            }
            ClipboardPlaneError::Encode(err) => {
                write!(f, "failed to encode clipboard bundle: {err}")
            }
            ClipboardPlaneError::Decode(err) => {
                write!(f, "failed to decode clipboard bundle: {err}")
            }
            ClipboardPlaneError::ObjectTransfer(err) => {
                write!(f, "clipboard object transfer failed: {err}")
            }
        }
    }
}

impl Error for ClipboardPlaneError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ClipboardPlaneError::Encode(err) | ClipboardPlaneError::Decode(err) => Some(err),
            ClipboardPlaneError::ObjectTransfer(err) => Some(err),
            _ => None,
        }
    }
}

impl From<ObjectTransferError> for ClipboardPlaneError {
    fn from(value: ObjectTransferError) -> Self {
        Self::ObjectTransfer(value)
    }
}

pub fn clipboard_bundle_to_envelopes(
    bundle: &ClipboardBundle,
    spec: ClipboardEnvelopeSpec,
) -> Result<Vec<DataEnvelope>, ClipboardPlaneError> {
    validate_clipboard_bundle(bundle)?;
    if spec.max_chunk_payload_len == 0 {
        return Err(ClipboardPlaneError::InvalidChunkPayloadLen);
    }

    let encoded = bundle.encode().map_err(ClipboardPlaneError::Encode)?;
    let chunk_count = encoded.len().div_ceil(spec.max_chunk_payload_len);
    let total_chunks = u32::try_from(chunk_count)
        .map_err(|_| ClipboardPlaneError::TooManyChunks { chunk_count })?;
    let checksum = checksum_crc32(&encoded);
    let mut envelopes = Vec::with_capacity(chunk_count);

    for (chunk_index, payload) in encoded.chunks(spec.max_chunk_payload_len).enumerate() {
        let chunk_index_u32 = u32::try_from(chunk_index)
            .map_err(|_| ClipboardPlaneError::TooManyChunks { chunk_count })?;
        let mut envelope = object_chunk_envelope(
            ObjectChunkSpec {
                object_id: spec.object_id,
                stream_id: spec.stream_id,
                sequence_number: spec.first_sequence_number + u64::from(chunk_index_u32),
                timestamp_ms: spec.timestamp_ms,
                chunk_index: chunk_index_u32,
                total_chunks,
                offset: (chunk_index * spec.max_chunk_payload_len) as u64,
                total_size: encoded.len() as u64,
                checksum_crc32: Some(checksum),
            },
            payload.to_vec(),
        );
        envelope.header.kind = ContentKind::ClipboardBundle;
        envelopes.push(envelope);
    }

    Ok(envelopes)
}

pub fn clipboard_bundle_to_envelopes_with_policy(
    bundle: &ClipboardBundle,
    spec: ClipboardEnvelopeSpec,
    policy: ClipboardSyncPolicy,
) -> Result<Vec<DataEnvelope>, ClipboardPlaneError> {
    validate_clipboard_bundle(bundle)?;
    policy.validate_bundle(bundle)?;
    clipboard_bundle_to_envelopes(bundle, spec)
}

pub struct ClipboardBundleAssembler {
    inner: ReliableObjectAssembler,
}

impl ClipboardBundleAssembler {
    pub fn from_first_chunk(envelope: &DataEnvelope) -> Result<Self, ClipboardPlaneError> {
        ensure_clipboard_bundle(envelope)?;
        let chunk = envelope
            .header
            .chunk
            .as_ref()
            .ok_or(ClipboardPlaneError::MissingChunkMetadata)?;
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
    ) -> Result<ObjectProgress, ClipboardPlaneError> {
        ensure_clipboard_bundle(&envelope)?;
        self.inner.push_chunk(envelope).map_err(Into::into)
    }

    pub fn finish(self) -> Result<ClipboardBundle, ClipboardPlaneError> {
        let completed = self.inner.finish()?;
        ClipboardBundle::decode(&completed.bytes).map_err(ClipboardPlaneError::Decode)
    }
}

pub fn validate_clipboard_bundle(bundle: &ClipboardBundle) -> Result<(), ClipboardPlaneError> {
    if bundle.items.is_empty() {
        return Err(ClipboardPlaneError::EmptyBundle);
    }

    for (item_index, item) in bundle.items.iter().enumerate() {
        match item {
            ClipboardItem::Text(_) => {}
            ClipboardItem::Image(image) if image.mime_type.is_empty() || image.bytes.is_empty() => {
                return Err(ClipboardPlaneError::InvalidImage { item_index });
            }
            ClipboardItem::Image(_) => {}
            ClipboardItem::File(file) if file.name.is_empty() => {
                return Err(ClipboardPlaneError::InvalidFileName { item_index });
            }
            ClipboardItem::File(_) => {}
        }
    }

    Ok(())
}

fn ensure_clipboard_bundle(envelope: &DataEnvelope) -> Result<(), ClipboardPlaneError> {
    if envelope.header.kind != ContentKind::ClipboardBundle {
        return Err(ClipboardPlaneError::UnexpectedContentKind(
            envelope.header.kind,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{MultiplexedPacket, UdpMultiplexer};
    use protocol::{ClipboardFile, ClipboardImage, ClipboardText, DataLane, ReliabilityMode};
    use std::time::Duration;
    use tokio::time::timeout;

    fn bundle_with_text_image_and_file() -> ClipboardBundle {
        ClipboardBundle::new(
            9,
            vec![
                ClipboardItem::Text(ClipboardText {
                    text: "remote text".to_string(),
                }),
                ClipboardItem::Image(ClipboardImage {
                    mime_type: "image/png".to_string(),
                    width: Some(4),
                    height: Some(2),
                    bytes: vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4],
                }),
                ClipboardItem::File(ClipboardFile {
                    name: "clip.txt".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    bytes: b"file over clipboard".to_vec(),
                }),
            ],
        )
    }

    #[test]
    fn clipboard_bundle_splits_to_reliable_envelopes_and_reassembles() {
        let bundle = bundle_with_text_image_and_file();
        let spec = ClipboardEnvelopeSpec {
            object_id: 77,
            stream_id: 3,
            first_sequence_number: 20,
            timestamp_ms: 1_000,
            max_chunk_payload_len: 24,
        };

        let envelopes = clipboard_bundle_to_envelopes(&bundle, spec).expect("bundle should split");

        assert!(envelopes.len() > 1);
        for (index, envelope) in envelopes.iter().enumerate() {
            assert_eq!(envelope.header.lane, DataLane::Reliable);
            assert_eq!(envelope.header.kind, ContentKind::ClipboardBundle);
            assert_eq!(envelope.header.reliability, ReliabilityMode::Reliable);
            assert_eq!(
                envelope.header.sequence_number,
                spec.first_sequence_number + index as u64
            );
            assert!(envelope.header.reliability_info.is_some());
        }

        let mut assembler = ClipboardBundleAssembler::from_first_chunk(&envelopes[0])
            .expect("assembler should initialize");
        for envelope in envelopes {
            assembler
                .push_chunk(envelope)
                .expect("clipboard chunk should assemble");
        }

        let decoded = assembler.finish().expect("clipboard should finish");
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn clipboard_text_bundle_can_use_default_chunk_size() {
        let bundle = ClipboardBundle::text(10, "plain text");
        let envelopes =
            clipboard_bundle_to_envelopes(&bundle, ClipboardEnvelopeSpec::new(1, 2, 3, 4))
                .expect("text clipboard should split");

        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].header.kind, ContentKind::ClipboardBundle);
    }

    #[test]
    fn clipboard_bundle_rejects_empty_bundle() {
        let err = clipboard_bundle_to_envelopes(
            &ClipboardBundle::new(1, vec![]),
            ClipboardEnvelopeSpec::new(1, 2, 3, 4),
        )
        .expect_err("empty bundle should be rejected");

        assert!(matches!(err, ClipboardPlaneError::EmptyBundle));
    }

    #[test]
    fn clipboard_bundle_rejects_invalid_image() {
        let bundle = ClipboardBundle::new(
            1,
            vec![ClipboardItem::Image(ClipboardImage {
                mime_type: "image/png".to_string(),
                width: None,
                height: None,
                bytes: vec![],
            })],
        );
        let err = clipboard_bundle_to_envelopes(&bundle, ClipboardEnvelopeSpec::new(1, 2, 3, 4))
            .expect_err("invalid image should be rejected");

        assert!(matches!(err, ClipboardPlaneError::InvalidImage { .. }));
    }

    #[test]
    fn clipboard_policy_blocks_file_items_until_enabled() {
        let bundle = bundle_with_text_image_and_file();
        let err = clipboard_bundle_to_envelopes_with_policy(
            &bundle,
            ClipboardEnvelopeSpec::new(1, 2, 3, 4),
            ClipboardSyncPolicy::default(),
        )
        .expect_err("file clipboard should require explicit policy");

        assert!(matches!(
            err,
            ClipboardPlaneError::DisallowedItem {
                item_index: 2,
                item_class: ClipboardItemClass::File
            }
        ));
    }

    #[test]
    fn clipboard_policy_allows_file_items_when_enabled() {
        let bundle = bundle_with_text_image_and_file();
        let envelopes = clipboard_bundle_to_envelopes_with_policy(
            &bundle,
            ClipboardEnvelopeSpec::new(1, 2, 3, 4),
            ClipboardSyncPolicy::default().with_files_allowed(),
        )
        .expect("file clipboard should be allowed by policy");

        assert!(!envelopes.is_empty());
    }

    #[test]
    fn clipboard_policy_limits_image_payload_size() {
        let bundle = ClipboardBundle::new(
            1,
            vec![ClipboardItem::Image(ClipboardImage {
                mime_type: "image/png".to_string(),
                width: None,
                height: None,
                bytes: vec![1, 2, 3, 4],
            })],
        );
        let err = clipboard_bundle_to_envelopes_with_policy(
            &bundle,
            ClipboardEnvelopeSpec::new(1, 2, 3, 4),
            ClipboardSyncPolicy {
                max_image_bytes: 3,
                ..ClipboardSyncPolicy::default()
            },
        )
        .expect_err("oversized image should be rejected");

        assert!(matches!(
            err,
            ClipboardPlaneError::ItemTooLarge {
                item_index: 0,
                item_class: ClipboardItemClass::Image,
                actual_bytes: 4,
                limit_bytes: 3
            }
        ));
    }

    #[test]
    fn clipboard_assembler_rejects_non_clipboard_envelope() {
        let mut envelope = clipboard_bundle_to_envelopes(
            &ClipboardBundle::text(1, "text"),
            ClipboardEnvelopeSpec::new(1, 2, 3, 4),
        )
        .expect("text clipboard should split")
        .remove(0);
        envelope.header.kind = ContentKind::FileChunk;

        let err = match ClipboardBundleAssembler::from_first_chunk(&envelope) {
            Ok(_) => panic!("non clipboard envelope should be rejected"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            ClipboardPlaneError::UnexpectedContentKind(ContentKind::FileChunk)
        ));
    }

    #[tokio::test]
    async fn clipboard_bundle_roundtrips_over_udp_data_path() {
        let left = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("left socket should bind");
        let right = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("right socket should bind");
        let right_addr = right.local_addr().expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        let bundle = bundle_with_text_image_and_file();
        let envelopes = clipboard_bundle_to_envelopes(
            &bundle,
            ClipboardEnvelopeSpec {
                object_id: 90,
                stream_id: 7,
                first_sequence_number: 100,
                timestamp_ms: 2_000,
                max_chunk_payload_len: 32,
            },
        )
        .expect("clipboard should split");
        let mut assembler = ClipboardBundleAssembler::from_first_chunk(&envelopes[0])
            .expect("assembler should initialize");

        for envelope in envelopes {
            sender
                .send_data(&envelope, right_addr)
                .await
                .expect("clipboard chunk should send");

            let received = match timeout(Duration::from_secs(2), receiver.recv())
                .await
                .expect("receive should not time out")
                .expect("receive should succeed")
            {
                MultiplexedPacket::Data(envelope, _) => envelope,
                other => panic!("unexpected packet: {other:?}"),
            };
            assembler
                .push_chunk(received)
                .expect("clipboard chunk should assemble");
        }

        let decoded = assembler.finish().expect("clipboard should finish");
        assert_eq!(decoded, bundle);
    }
}
