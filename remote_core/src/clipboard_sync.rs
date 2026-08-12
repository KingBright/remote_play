use crate::clipboard_plane::{
    ClipboardBundleAssembler, ClipboardEnvelopeSpec, ClipboardPlaneError, ClipboardSyncPolicy,
    clipboard_bundle_to_envelopes_with_policy,
};
use crate::data_plane::checksum_crc32;
use crate::traits::ClipboardProvider;
use protocol::{ClipboardBundle, ContentKind, DataEnvelope};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardSyncConfig {
    pub policy: ClipboardSyncPolicy,
    pub stream_id: u32,
    pub next_object_id: u64,
    pub next_sequence_number: u64,
    pub chunk_payload_len: usize,
}

impl Default for ClipboardSyncConfig {
    fn default() -> Self {
        Self {
            policy: ClipboardSyncPolicy::default(),
            stream_id: 1,
            next_object_id: 1,
            next_sequence_number: 1,
            chunk_payload_len: crate::clipboard_plane::DEFAULT_CLIPBOARD_CHUNK_PAYLOAD_LEN,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardOutgoingTransfer {
    pub bundle_id: u64,
    pub content_crc32: u32,
    pub envelopes: Vec<DataEnvelope>,
}

#[derive(Debug)]
pub enum ClipboardSyncError {
    Provider(Box<dyn Error + Send + Sync>),
    Plane(ClipboardPlaneError),
    Encode(bincode::Error),
    MissingChunkMetadata,
    UnexpectedContentKind(ContentKind),
}

impl fmt::Display for ClipboardSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClipboardSyncError::Provider(err) => write!(f, "clipboard provider failed: {err}"),
            ClipboardSyncError::Plane(err) => write!(f, "clipboard data plane failed: {err}"),
            ClipboardSyncError::Encode(err) => write!(f, "clipboard encoding failed: {err}"),
            ClipboardSyncError::MissingChunkMetadata => {
                write!(f, "clipboard data envelope is missing chunk metadata")
            }
            ClipboardSyncError::UnexpectedContentKind(kind) => {
                write!(f, "unexpected clipboard sync content kind: {kind:?}")
            }
        }
    }
}

impl Error for ClipboardSyncError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ClipboardSyncError::Provider(err) => Some(err.as_ref()),
            ClipboardSyncError::Plane(err) => Some(err),
            ClipboardSyncError::Encode(err) => Some(err),
            ClipboardSyncError::MissingChunkMetadata
            | ClipboardSyncError::UnexpectedContentKind(_) => None,
        }
    }
}

impl From<ClipboardPlaneError> for ClipboardSyncError {
    fn from(value: ClipboardPlaneError) -> Self {
        Self::Plane(value)
    }
}

impl From<bincode::Error> for ClipboardSyncError {
    fn from(value: bincode::Error) -> Self {
        Self::Encode(value)
    }
}

pub struct ClipboardSyncEndpoint<P> {
    provider: P,
    config: ClipboardSyncConfig,
    inbound: HashMap<u64, ClipboardBundleAssembler>,
    last_sent_content_crc32: Option<u32>,
    last_applied_content_crc32: Option<u32>,
}

impl<P: ClipboardProvider> ClipboardSyncEndpoint<P> {
    pub fn new(provider: P, config: ClipboardSyncConfig) -> Self {
        Self {
            provider,
            config,
            inbound: HashMap::new(),
            last_sent_content_crc32: None,
            last_applied_content_crc32: None,
        }
    }

    pub fn provider(&self) -> &P {
        &self.provider
    }

    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }

    pub fn config(&self) -> ClipboardSyncConfig {
        self.config
    }

    pub fn last_sent_content_crc32(&self) -> Option<u32> {
        self.last_sent_content_crc32
    }

    pub fn last_applied_content_crc32(&self) -> Option<u32> {
        self.last_applied_content_crc32
    }

    pub async fn poll_outgoing(
        &mut self,
        timestamp_ms: u64,
    ) -> Result<Option<ClipboardOutgoingTransfer>, ClipboardSyncError> {
        let Some(bundle) = self
            .provider
            .read_clipboard(self.config.policy)
            .await
            .map_err(ClipboardSyncError::Provider)?
        else {
            return Ok(None);
        };

        let content_crc32 = clipboard_content_crc32(&bundle)?;
        if Some(content_crc32) == self.last_sent_content_crc32
            || Some(content_crc32) == self.last_applied_content_crc32
        {
            return Ok(None);
        }

        let spec = ClipboardEnvelopeSpec {
            object_id: self.config.next_object_id,
            stream_id: self.config.stream_id,
            first_sequence_number: self.config.next_sequence_number,
            timestamp_ms,
            max_chunk_payload_len: self.config.chunk_payload_len,
        };
        let envelopes =
            clipboard_bundle_to_envelopes_with_policy(&bundle, spec, self.config.policy)?;

        self.config.next_object_id = self.config.next_object_id.saturating_add(1);
        self.config.next_sequence_number = self
            .config
            .next_sequence_number
            .saturating_add(envelopes.len() as u64);
        self.last_sent_content_crc32 = Some(content_crc32);

        Ok(Some(ClipboardOutgoingTransfer {
            bundle_id: bundle.bundle_id,
            content_crc32,
            envelopes,
        }))
    }

    pub async fn receive_envelope(
        &mut self,
        envelope: DataEnvelope,
    ) -> Result<Option<ClipboardBundle>, ClipboardSyncError> {
        if envelope.header.kind != ContentKind::ClipboardBundle {
            return Err(ClipboardSyncError::UnexpectedContentKind(
                envelope.header.kind,
            ));
        }

        let object_id = envelope
            .header
            .chunk
            .as_ref()
            .map(|chunk| chunk.object_id)
            .ok_or(ClipboardSyncError::MissingChunkMetadata)?;

        let assembler = match self.inbound.entry(object_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(ClipboardBundleAssembler::from_first_chunk(&envelope)?)
            }
        };
        let progress = assembler.push_chunk(envelope)?;
        if !progress.is_complete {
            return Ok(None);
        }

        let assembler = self
            .inbound
            .remove(&object_id)
            .expect("complete clipboard assembler should still be present");
        let bundle = assembler.finish()?;
        let content_crc32 = clipboard_content_crc32(&bundle)?;

        if Some(content_crc32) == self.last_sent_content_crc32
            || Some(content_crc32) == self.last_applied_content_crc32
        {
            return Ok(None);
        }

        self.provider
            .write_clipboard(&bundle, self.config.policy)
            .await
            .map_err(ClipboardSyncError::Provider)?;
        self.last_applied_content_crc32 = Some(content_crc32);
        Ok(Some(bundle))
    }
}

pub fn clipboard_content_crc32(bundle: &ClipboardBundle) -> Result<u32, ClipboardSyncError> {
    let normalized = ClipboardBundle::new(0, bundle.items.clone());
    let encoded = normalized.encode()?;
    Ok(checksum_crc32(&encoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard_provider::MemoryClipboardProvider;
    use crate::traits::ClipboardProvider;
    use protocol::{ClipboardImage, ClipboardItem, ClipboardText};

    fn text_bundle(bundle_id: u64, text: &str) -> ClipboardBundle {
        ClipboardBundle::new(
            bundle_id,
            vec![ClipboardItem::Text(ClipboardText {
                text: text.to_string(),
            })],
        )
    }

    fn image_bundle(bundle_id: u64) -> ClipboardBundle {
        ClipboardBundle::new(
            bundle_id,
            vec![ClipboardItem::Image(ClipboardImage {
                mime_type: "image/png".to_string(),
                width: Some(1),
                height: Some(1),
                bytes: vec![0x89, b'P', b'N', b'G'],
            })],
        )
    }

    #[tokio::test]
    async fn endpoint_polls_provider_into_reliable_clipboard_envelopes() {
        let provider = MemoryClipboardProvider::with_bundle(text_bundle(7, "hello"));
        let mut endpoint = ClipboardSyncEndpoint::new(
            provider,
            ClipboardSyncConfig {
                chunk_payload_len: 8,
                ..ClipboardSyncConfig::default()
            },
        );

        let transfer = endpoint
            .poll_outgoing(1_000)
            .await
            .expect("poll should succeed")
            .expect("clipboard should produce outgoing transfer");

        assert_eq!(transfer.bundle_id, 7);
        assert!(transfer.envelopes.len() > 1);
        assert!(
            transfer
                .envelopes
                .iter()
                .all(|envelope| envelope.header.kind == ContentKind::ClipboardBundle)
        );
        assert_eq!(
            endpoint.config().next_sequence_number,
            1 + transfer.envelopes.len() as u64
        );
        assert_eq!(
            endpoint.last_sent_content_crc32(),
            Some(transfer.content_crc32)
        );
    }

    #[tokio::test]
    async fn endpoint_applies_received_transfer_and_suppresses_echo() {
        let source_provider = MemoryClipboardProvider::with_bundle(text_bundle(1, "remote text"));
        let sink_provider = MemoryClipboardProvider::new();
        let mut source =
            ClipboardSyncEndpoint::new(source_provider, ClipboardSyncConfig::default());
        let mut sink = ClipboardSyncEndpoint::new(sink_provider, ClipboardSyncConfig::default());
        let transfer = source
            .poll_outgoing(2_000)
            .await
            .expect("poll should succeed")
            .expect("source should send clipboard");

        let mut applied = None;
        for envelope in transfer.envelopes {
            applied = sink
                .receive_envelope(envelope)
                .await
                .expect("receive should succeed")
                .or(applied);
        }

        assert_eq!(applied, Some(text_bundle(1, "remote text")));
        let read_back = sink
            .provider_mut()
            .read_clipboard(ClipboardSyncPolicy::default())
            .await
            .expect("sink provider should read");
        assert_eq!(read_back, Some(text_bundle(1, "remote text")));
        assert_eq!(
            sink.poll_outgoing(3_000)
                .await
                .expect("echo poll should succeed"),
            None
        );
    }

    #[tokio::test]
    async fn endpoint_sends_new_local_change_after_remote_apply() {
        let source_provider = MemoryClipboardProvider::with_bundle(text_bundle(1, "first"));
        let mut endpoint =
            ClipboardSyncEndpoint::new(source_provider, ClipboardSyncConfig::default());

        let first = endpoint
            .poll_outgoing(1_000)
            .await
            .expect("first poll should succeed")
            .expect("first clipboard should send");
        endpoint.last_applied_content_crc32 = Some(first.content_crc32);

        endpoint
            .provider_mut()
            .write_clipboard(&text_bundle(2, "second"), ClipboardSyncPolicy::default())
            .await
            .expect("local provider update should succeed");
        let second = endpoint
            .poll_outgoing(2_000)
            .await
            .expect("second poll should succeed")
            .expect("new local clipboard should send");

        assert_ne!(first.content_crc32, second.content_crc32);
    }

    #[tokio::test]
    async fn content_crc_ignores_bundle_id() {
        assert_eq!(
            clipboard_content_crc32(&text_bundle(1, "same")).expect("crc should compute"),
            clipboard_content_crc32(&text_bundle(2, "same")).expect("crc should compute")
        );
        assert_ne!(
            clipboard_content_crc32(&text_bundle(1, "left")).expect("crc should compute"),
            clipboard_content_crc32(&text_bundle(1, "right")).expect("crc should compute")
        );
    }

    #[tokio::test]
    async fn endpoint_roundtrips_image_clipboard() {
        let source_provider = MemoryClipboardProvider::with_bundle(image_bundle(9));
        let sink_provider = MemoryClipboardProvider::new();
        let mut source =
            ClipboardSyncEndpoint::new(source_provider, ClipboardSyncConfig::default());
        let mut sink = ClipboardSyncEndpoint::new(sink_provider, ClipboardSyncConfig::default());
        let transfer = source
            .poll_outgoing(4_000)
            .await
            .expect("image poll should succeed")
            .expect("image clipboard should send");

        let mut applied = None;
        for envelope in transfer.envelopes {
            applied = sink
                .receive_envelope(envelope)
                .await
                .expect("image receive should succeed")
                .or(applied);
        }

        assert_eq!(applied, Some(image_bundle(9)));
    }

    #[tokio::test]
    async fn endpoint_rejects_non_clipboard_envelope() {
        let provider = MemoryClipboardProvider::new();
        let mut endpoint = ClipboardSyncEndpoint::new(provider, ClipboardSyncConfig::default());
        let envelope = DataEnvelope::realtime_video(1, 1, 1_000, 1_016, vec![1]);

        let err = endpoint
            .receive_envelope(envelope)
            .await
            .expect_err("non clipboard envelope should be rejected");

        assert!(matches!(
            err,
            ClipboardSyncError::UnexpectedContentKind(ContentKind::VideoH265)
        ));
    }
}
