use protocol::{
    AudioStreamConfig, ContentKind, DataEnvelope, DataLane, PayloadType, RtpHeader, RtpPacket,
};
use std::error::Error;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

const VIDEO_DEADLINE_DELTA_MS: u64 = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaPlaneError {
    UnsupportedPayloadType(u8),
    UnsupportedEnvelopeKind {
        lane: DataLane,
        kind: ContentKind,
    },
    InvalidAudioStreamConfigPayload,
    AudioStreamConfigStreamMismatch {
        header_stream_id: u32,
        config_stream_id: u32,
    },
    SequenceOutOfRange(u64),
    TimestampOutOfRange(u64),
}

impl fmt::Display for MediaPlaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaPlaneError::UnsupportedPayloadType(payload_type) => {
                write!(f, "unsupported RTP payload type {payload_type}")
            }
            MediaPlaneError::UnsupportedEnvelopeKind { lane, kind } => {
                write!(
                    f,
                    "unsupported media envelope kind {kind:?} on lane {lane:?}"
                )
            }
            MediaPlaneError::InvalidAudioStreamConfigPayload => {
                write!(f, "invalid audio stream config payload")
            }
            MediaPlaneError::AudioStreamConfigStreamMismatch {
                header_stream_id,
                config_stream_id,
            } => write!(
                f,
                "audio stream config id {config_stream_id} does not match envelope stream id {header_stream_id}"
            ),
            MediaPlaneError::SequenceOutOfRange(sequence_number) => {
                write!(
                    f,
                    "media sequence number {sequence_number} exceeds u16 range"
                )
            }
            MediaPlaneError::TimestampOutOfRange(timestamp_ms) => {
                write!(f, "media timestamp {timestamp_ms} exceeds u32 range")
            }
        }
    }
}

impl Error for MediaPlaneError {}

pub fn rtp_to_realtime_data(packet: &RtpPacket) -> Result<DataEnvelope, MediaPlaneError> {
    rtp_to_realtime_data_at(packet, now_ms())
}

pub fn rtp_to_realtime_data_at(
    packet: &RtpPacket,
    now_ms: u64,
) -> Result<DataEnvelope, MediaPlaneError> {
    let sequence_number = packet.header.sequence_number as u64;
    let stream_id = packet.header.ssrc;

    match packet.header.payload_type {
        payload_type if payload_type == PayloadType::VideoH265 as u8 => {
            let timestamp_ms = unwrap_wrapped_millis(packet.header.timestamp, now_ms);
            Ok(DataEnvelope::realtime_video(
                stream_id,
                sequence_number,
                timestamp_ms,
                timestamp_ms.wrapping_add(VIDEO_DEADLINE_DELTA_MS),
                packet.payload.clone(),
            ))
        }
        payload_type if payload_type == PayloadType::AudioOpus as u8 => {
            Ok(DataEnvelope::realtime_audio(
                stream_id,
                sequence_number,
                packet.header.timestamp as u64,
                packet.payload.clone(),
            ))
        }
        payload_type => Err(MediaPlaneError::UnsupportedPayloadType(payload_type)),
    }
}

pub fn audio_stream_config_to_envelope(
    config: &AudioStreamConfig,
    sequence_number: u64,
    timestamp_ms: u64,
) -> Result<DataEnvelope, MediaPlaneError> {
    DataEnvelope::audio_stream_config(config, sequence_number, timestamp_ms)
        .map_err(|_| MediaPlaneError::InvalidAudioStreamConfigPayload)
}

pub fn audio_stream_config_from_envelope(
    envelope: &DataEnvelope,
) -> Result<AudioStreamConfig, MediaPlaneError> {
    if envelope.header.lane != DataLane::InteractiveControl
        || envelope.header.kind != ContentKind::AudioStreamConfig
    {
        return Err(MediaPlaneError::UnsupportedEnvelopeKind {
            lane: envelope.header.lane,
            kind: envelope.header.kind,
        });
    }

    let config = AudioStreamConfig::decode(&envelope.payload)
        .map_err(|_| MediaPlaneError::InvalidAudioStreamConfigPayload)?;
    if config.stream_id != envelope.header.stream_id {
        return Err(MediaPlaneError::AudioStreamConfigStreamMismatch {
            header_stream_id: envelope.header.stream_id,
            config_stream_id: config.stream_id,
        });
    }
    Ok(config)
}

pub fn realtime_data_to_rtp(envelope: DataEnvelope) -> Result<RtpPacket, MediaPlaneError> {
    let payload_type = match (envelope.header.lane, envelope.header.kind) {
        (DataLane::RealtimeVideo, ContentKind::VideoH265) => PayloadType::VideoH265 as u8,
        (DataLane::RealtimeAudio, ContentKind::AudioOpus) => PayloadType::AudioOpus as u8,
        (lane, kind) => {
            return Err(MediaPlaneError::UnsupportedEnvelopeKind { lane, kind });
        }
    };

    let sequence_number = u16::try_from(envelope.header.sequence_number)
        .map_err(|_| MediaPlaneError::SequenceOutOfRange(envelope.header.sequence_number))?;
    let timestamp = envelope.header.timestamp_ms as u32;

    Ok(RtpPacket {
        header: RtpHeader {
            version: 2,
            payload_type,
            sequence_number,
            timestamp,
            ssrc: envelope.header.stream_id,
        },
        payload: envelope.payload,
    })
}

fn unwrap_wrapped_millis(wrapped_ms: u32, now_ms: u64) -> u64 {
    let low_now = now_ms as u32;
    let signed_delta = wrapped_ms.wrapping_sub(low_now) as i32 as i64;

    if signed_delta >= 0 {
        now_ms.saturating_add(signed_delta as u64)
    } else {
        now_ms.saturating_sub(signed_delta.unsigned_abs())
    }
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

    fn packet(payload_type: u8, sequence_number: u16, timestamp: u32, ssrc: u32) -> RtpPacket {
        RtpPacket {
            header: RtpHeader {
                version: 2,
                payload_type,
                sequence_number,
                timestamp,
                ssrc,
            },
            payload: vec![1, 2, 3, 4],
        }
    }

    #[test]
    fn video_rtp_roundtrips_through_realtime_data_envelope() {
        let now_ms = 10_000;
        let rtp = packet(PayloadType::VideoH265 as u8, 42, 1_000, 77);

        let envelope = rtp_to_realtime_data_at(&rtp, now_ms).expect("video RTP should adapt");
        assert_eq!(envelope.header.lane, DataLane::RealtimeVideo);
        assert_eq!(envelope.header.kind, ContentKind::VideoH265);
        assert_eq!(envelope.header.stream_id, rtp.header.ssrc);
        assert_eq!(
            envelope.header.sequence_number,
            rtp.header.sequence_number as u64
        );
        assert_eq!(envelope.header.timestamp_ms as u32, rtp.header.timestamp);
        assert_eq!(envelope.header.deadline_ms, Some(1_016));

        let decoded = realtime_data_to_rtp(envelope).expect("video data should adapt back");
        assert_eq!(decoded, rtp);
    }

    #[test]
    fn audio_rtp_roundtrips_through_realtime_data_envelope() {
        let rtp = packet(PayloadType::AudioOpus as u8, 12, 960, 78);

        let envelope = rtp_to_realtime_data(&rtp).expect("audio RTP should adapt");
        assert_eq!(envelope.header.lane, DataLane::RealtimeAudio);
        assert_eq!(envelope.header.kind, ContentKind::AudioOpus);
        assert_eq!(envelope.header.timestamp_ms, rtp.header.timestamp as u64);
        assert_eq!(envelope.header.deadline_ms, None);

        let decoded = realtime_data_to_rtp(envelope).expect("audio data should adapt back");
        assert_eq!(decoded, rtp);
    }

    #[test]
    fn audio_stream_configs_roundtrip_through_interactive_envelopes() {
        let configs = [
            AudioStreamConfig::remote_system(80, 48_000, 2, 20),
            AudioStreamConfig::remote_microphone(81, 48_000, 1, 20),
            AudioStreamConfig::viewer_microphone_talkback(82, 48_000, 1, 20),
        ];

        for (index, config) in configs.iter().enumerate() {
            let envelope = audio_stream_config_to_envelope(config, index as u64, 10_000)
                .expect("audio stream config should adapt");
            assert_eq!(envelope.header.lane, DataLane::InteractiveControl);
            assert_eq!(envelope.header.kind, ContentKind::AudioStreamConfig);
            assert_eq!(envelope.header.stream_id, config.stream_id);

            let decoded = audio_stream_config_from_envelope(&envelope)
                .expect("audio stream config should decode");
            assert_eq!(decoded, *config);
        }
    }

    #[test]
    fn audio_stream_config_rejects_wrong_envelope_kind() {
        let envelope = DataEnvelope::realtime_audio(1, 2, 3, vec![1, 2, 3]);

        assert_eq!(
            audio_stream_config_from_envelope(&envelope)
                .expect_err("audio packet should not decode as stream config"),
            MediaPlaneError::UnsupportedEnvelopeKind {
                lane: DataLane::RealtimeAudio,
                kind: ContentKind::AudioOpus,
            }
        );
    }

    #[test]
    fn audio_stream_config_rejects_stream_id_mismatch() {
        let config = AudioStreamConfig::remote_microphone(40, 48_000, 1, 20);
        let mut envelope = audio_stream_config_to_envelope(&config, 0, 10_000)
            .expect("audio stream config should adapt");
        envelope.header.stream_id = 41;

        assert_eq!(
            audio_stream_config_from_envelope(&envelope)
                .expect_err("stream id mismatch should fail"),
            MediaPlaneError::AudioStreamConfigStreamMismatch {
                header_stream_id: 41,
                config_stream_id: 40,
            }
        );
    }

    #[test]
    fn rejects_non_media_rtp_payload_types() {
        let rtp = packet(42, 1, 1_000, 1);

        assert_eq!(
            rtp_to_realtime_data(&rtp).expect_err("unknown payload type should fail"),
            MediaPlaneError::UnsupportedPayloadType(42)
        );
    }

    #[test]
    fn rejects_data_envelopes_that_do_not_carry_media() {
        let envelope = DataEnvelope::new(
            DataLane::InteractiveControl,
            ContentKind::Control,
            1,
            1,
            1_000,
            vec![],
        );

        assert_eq!(
            realtime_data_to_rtp(envelope).expect_err("control data should fail"),
            MediaPlaneError::UnsupportedEnvelopeKind {
                lane: DataLane::InteractiveControl,
                kind: ContentKind::Control
            }
        );
    }

    #[test]
    fn video_timestamp_unwraps_near_current_time_across_u32_wrap() {
        let now_ms = u32::MAX as u64 + 20;
        let rtp = packet(PayloadType::VideoH265 as u8, 7, 10, 77);

        let envelope = rtp_to_realtime_data_at(&rtp, now_ms).expect("video RTP should adapt");

        assert_eq!(envelope.header.timestamp_ms, u32::MAX as u64 + 11);
        assert_eq!(envelope.header.timestamp_ms as u32, rtp.header.timestamp);
        assert_eq!(envelope.header.deadline_ms, Some(u32::MAX as u64 + 27));
        assert_eq!(realtime_data_to_rtp(envelope).unwrap(), rtp);
    }
}
