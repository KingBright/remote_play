use protocol::{CompactRealtimeError, ControlMessage, DataEnvelope, RtpPacket};
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

pub const DEFAULT_CONTROL_PORT: u16 = 39271;

pub struct UdpMultiplexer {
    socket: Arc<UdpSocket>,
}

impl UdpMultiplexer {
    pub async fn bind(addr: &str) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let std_addr: SocketAddr = addr.parse()?;
        let socket2_sock = socket2::Socket::new(
            if std_addr.is_ipv4() {
                socket2::Domain::IPV4
            } else {
                socket2::Domain::IPV6
            },
            socket2::Type::DGRAM,
            None,
        )?;

        let _ = socket2_sock.set_recv_buffer_size(2 * 1024 * 1024);
        let _ = socket2_sock.set_send_buffer_size(2 * 1024 * 1024);
        socket2_sock.set_nonblocking(true)?;
        socket2_sock.bind(&std_addr.into())?;

        let std_socket: std::net::UdpSocket = socket2_sock.into();
        let socket = UdpSocket::from_std(std_socket)?;

        Ok(Self {
            socket: Arc::new(socket),
        })
    }

    pub fn split(&self) -> (UdpSender, UdpReceiver) {
        (
            UdpSender {
                socket: self.socket.clone(),
            },
            UdpReceiver {
                socket: self.socket.clone(),
                fragments: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            },
        )
    }

    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.socket.local_addr()
    }
}

#[derive(Clone)]
pub struct UdpSender {
    socket: Arc<UdpSocket>,
}
impl UdpSender {
    async fn send_multiplexed(
        &self,
        header: u8,
        bytes: &[u8],
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let max_payload = 1400;
        if bytes.len() <= max_payload {
            let mut buf = Vec::with_capacity(bytes.len() + 1);
            buf.push(header);
            buf.extend_from_slice(bytes);
            self.socket.send_to(&buf, target).await?;
        } else {
            // Fragment the packet
            // Format: 0x03, original_header(1), fragment_id(4), chunk_idx(2), total_chunks(2), data...
            let fragment_id = rand::random::<u32>();
            let chunks = bytes.chunks(max_payload);
            let total_chunks = chunks.len() as u16;

            for (i, chunk) in chunks.enumerate() {
                let mut buf = Vec::with_capacity(chunk.len() + 10);
                buf.push(0x03); // Fragment multiplex header
                buf.push(header);
                buf.extend_from_slice(&fragment_id.to_be_bytes());
                buf.extend_from_slice(&(i as u16).to_be_bytes());
                buf.extend_from_slice(&total_chunks.to_be_bytes());
                buf.extend_from_slice(chunk);
                self.socket.send_to(&buf, target).await?;

                // Pace the burst: sleep every 10 packets (~14KB) to avoid OS buffer overflow.
                // 100us usually takes ~1ms on macOS. 10 sleeps = ~10ms for a 150KB I-frame.
                if i % 10 == 9 {
                    tokio::time::sleep(std::time::Duration::from_micros(100)).await;
                }
            }
        }
        Ok(())
    }

    pub async fn send_rtp(
        &self,
        packet: &RtpPacket,
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let bytes = packet.encode()?;
        self.send_multiplexed(0x01, &bytes, target).await
    }

    pub async fn send_control(
        &self,
        msg: &ControlMessage,
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let bytes = msg.encode()?;
        self.send_multiplexed(0x02, &bytes, target).await
    }

    pub async fn send_data(
        &self,
        envelope: &DataEnvelope,
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        match envelope.encode_compact_realtime() {
            Ok(bytes) => self.send_multiplexed(0x05, &bytes, target).await,
            Err(
                CompactRealtimeError::NonRealtimeLane(_)
                | CompactRealtimeError::UnsupportedRealtimeMetadata,
            ) => {
                let bytes = envelope.encode()?;
                self.send_multiplexed(0x04, &bytes, target).await
            }
            Err(err) => Err(Box::new(err)),
        }
    }
}

use std::collections::{HashMap, hash_map::Entry};
use tokio::sync::Mutex;

const MAX_FRAGMENT_AGE: Duration = Duration::from_secs(2);
const MAX_FRAGMENT_CACHE_ENTRIES: usize = 10;

type FragmentChunks = Vec<Option<Vec<u8>>>;
type FragmentMap = HashMap<u32, FragmentEntry>;

#[derive(Debug)]
struct FragmentEntry {
    received_chunks: u16,
    chunks: FragmentChunks,
    created_at: Instant,
}

impl FragmentEntry {
    fn new(total_chunks: u16, now: Instant) -> Self {
        Self {
            received_chunks: 0,
            chunks: vec![None; total_chunks as usize],
            created_at: now,
        }
    }
}

pub struct UdpReceiver {
    socket: Arc<UdpSocket>,
    fragments: Arc<Mutex<FragmentMap>>,
}

#[derive(Debug)]
pub enum MultiplexedPacket {
    Rtp(RtpPacket, SocketAddr),
    Control(ControlMessage, SocketAddr),
    Data(DataEnvelope, SocketAddr),
}

impl UdpReceiver {
    fn cleanup_expired_fragments(fragments: &mut FragmentMap, now: Instant) {
        fragments.retain(|_, entry| now.duration_since(entry.created_at) <= MAX_FRAGMENT_AGE);
    }

    fn enforce_fragment_cache_limit(fragments: &mut FragmentMap, protected_id: u32) {
        while fragments.len() > MAX_FRAGMENT_CACHE_ENTRIES {
            let oldest_unprotected_id = fragments
                .iter()
                .filter(|(id, _)| **id != protected_id)
                .min_by_key(|(_, entry)| entry.created_at)
                .map(|(id, _)| *id);

            if let Some(id) = oldest_unprotected_id {
                fragments.remove(&id);
            } else {
                break;
            }
        }
    }

    pub async fn recv(&self) -> Result<MultiplexedPacket, Box<dyn Error + Send + Sync>> {
        loop {
            let mut buf = vec![0u8; 65536];
            let (len, addr) = self.socket.recv_from(&mut buf).await?;
            if len == 0 {
                continue;
            }
            match buf[0] {
                0x01 => {
                    let rtp = RtpPacket::decode(&buf[1..len])?;
                    return Ok(MultiplexedPacket::Rtp(rtp, addr));
                }
                0x02 => {
                    let msg = ControlMessage::decode(&buf[1..len])?;
                    return Ok(MultiplexedPacket::Control(msg, addr));
                }
                0x04 => {
                    let envelope = DataEnvelope::decode(&buf[1..len])?;
                    return Ok(MultiplexedPacket::Data(envelope, addr));
                }
                0x05 => {
                    let envelope = DataEnvelope::decode_compact_realtime(&buf[1..len])?;
                    return Ok(MultiplexedPacket::Data(envelope, addr));
                }
                0x03 => {
                    if len < 10 {
                        continue;
                    }
                    let header = buf[1];
                    let fragment_id = u32::from_be_bytes(buf[2..6].try_into().unwrap());
                    let chunk_idx = u16::from_be_bytes(buf[6..8].try_into().unwrap());
                    let total_chunks = u16::from_be_bytes(buf[8..10].try_into().unwrap());

                    if total_chunks == 0 {
                        return Err("Invalid UDP fragment total chunk count".into());
                    }

                    if chunk_idx >= total_chunks {
                        return Err("Invalid UDP fragment index".into());
                    }

                    let mut fragments = self.fragments.lock().await;
                    let now = Instant::now();
                    Self::cleanup_expired_fragments(&mut fragments, now);

                    let entry = match fragments.entry(fragment_id) {
                        Entry::Occupied(entry) => {
                            if entry.get().chunks.len() != total_chunks as usize {
                                return Err("Mismatched UDP fragment total chunk count".into());
                            }
                            entry.into_mut()
                        }
                        Entry::Vacant(entry) => entry.insert(FragmentEntry::new(total_chunks, now)),
                    };

                    if entry.chunks[chunk_idx as usize].is_none() {
                        entry.chunks[chunk_idx as usize] = Some(buf[10..len].to_vec());
                        entry.received_chunks += 1;
                    }

                    if entry.received_chunks == total_chunks {
                        let mut full_data = Vec::new();
                        for chunk in entry.chunks.iter() {
                            full_data.extend_from_slice(chunk.as_ref().unwrap());
                        }
                        fragments.remove(&fragment_id);

                        // Parse full_data
                        match header {
                            0x01 => {
                                let rtp = RtpPacket::decode(&full_data)?;
                                return Ok(MultiplexedPacket::Rtp(rtp, addr));
                            }
                            0x02 => {
                                let msg = ControlMessage::decode(&full_data)?;
                                return Ok(MultiplexedPacket::Control(msg, addr));
                            }
                            0x04 => {
                                let envelope = DataEnvelope::decode(&full_data)?;
                                return Ok(MultiplexedPacket::Data(envelope, addr));
                            }
                            0x05 => {
                                let envelope = DataEnvelope::decode_compact_realtime(&full_data)?;
                                return Ok(MultiplexedPacket::Data(envelope, addr));
                            }
                            _ => {
                                return Err(
                                    "Unknown UDP multiplexing header inside fragment".into()
                                );
                            }
                        }
                    } else {
                        Self::enforce_fragment_cache_limit(&mut fragments, fragment_id);
                    }
                }
                _ => return Err("Unknown UDP multiplexing header".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_plane::{realtime_data_to_rtp, rtp_to_realtime_data};
    use protocol::{ChunkInfo, ContentKind, DataEnvelope, PayloadType, RtpHeader, RtpPacket};
    use std::time::Duration;
    use tokio::time::timeout;

    fn rtp_packet(payload_len: usize) -> RtpPacket {
        RtpPacket {
            header: RtpHeader {
                version: 2,
                payload_type: 96,
                sequence_number: 42,
                timestamp: 123_456,
                ssrc: 99,
            },
            payload: (0..payload_len).map(|i| (i % 251) as u8).collect(),
        }
    }

    fn data_envelope(payload_len: usize) -> DataEnvelope {
        DataEnvelope::realtime_video(
            99,
            42,
            123_456,
            123_472,
            (0..payload_len).map(|i| (i % 251) as u8).collect(),
        )
    }

    fn audio_packet(payload_len: usize) -> RtpPacket {
        RtpPacket {
            header: RtpHeader {
                version: 2,
                payload_type: PayloadType::AudioOpus as u8,
                sequence_number: 43,
                timestamp: 960,
                ssrc: 100,
            },
            payload: (0..payload_len).map(|i| (i % 251) as u8).collect(),
        }
    }

    fn reliable_envelope(payload_len: usize) -> DataEnvelope {
        DataEnvelope::reliable_object_chunk(
            ContentKind::FileChunk,
            7,
            44,
            123_456,
            ChunkInfo {
                object_id: 99,
                chunk_index: 0,
                total_chunks: 1,
                offset: 0,
                total_size: payload_len as u64,
            },
            None,
            (0..payload_len).map(|i| (i % 251) as u8).collect(),
        )
    }

    async fn bind_pair() -> (UdpMultiplexer, UdpMultiplexer) {
        let left = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("left socket should bind");
        let right = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("right socket should bind");
        (left, right)
    }

    async fn recv_with_timeout(receiver: &UdpReceiver) -> MultiplexedPacket {
        timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("receive should not time out")
            .expect("packet should decode")
    }

    async fn send_raw_fragment(
        target: SocketAddr,
        fragment_id: u32,
        chunk_idx: u16,
        total_chunks: u16,
        data: &[u8],
    ) {
        let raw_sender = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("raw sender should bind");
        let mut bytes = Vec::with_capacity(10 + data.len());
        bytes.push(0x03);
        bytes.push(0x01);
        bytes.extend_from_slice(&fragment_id.to_be_bytes());
        bytes.extend_from_slice(&chunk_idx.to_be_bytes());
        bytes.extend_from_slice(&total_chunks.to_be_bytes());
        bytes.extend_from_slice(data);

        raw_sender
            .send_to(&bytes, target)
            .await
            .expect("raw fragment send should succeed");
    }

    async fn recv_error(receiver: &UdpReceiver) -> String {
        timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("receive should not time out")
            .expect_err("packet should fail")
            .to_string()
    }

    fn fragment_entry(total_chunks: u16, created_at: Instant) -> FragmentEntry {
        FragmentEntry::new(total_chunks, created_at)
    }

    #[test]
    fn cleanup_expired_fragments_removes_old_entries() {
        let now = Instant::now();
        let mut fragments = FragmentMap::new();
        fragments.insert(
            1,
            fragment_entry(2, now - MAX_FRAGMENT_AGE - Duration::from_millis(1)),
        );
        fragments.insert(2, fragment_entry(2, now));

        UdpReceiver::cleanup_expired_fragments(&mut fragments, now);

        assert!(!fragments.contains_key(&1));
        assert!(fragments.contains_key(&2));
    }

    #[test]
    fn enforce_fragment_cache_limit_removes_oldest_unprotected_entries() {
        let now = Instant::now();
        let mut fragments = FragmentMap::new();

        for id in 0..(MAX_FRAGMENT_CACHE_ENTRIES as u32 + 2) {
            fragments.insert(
                id,
                fragment_entry(2, now + Duration::from_millis(id as u64)),
            );
        }

        UdpReceiver::enforce_fragment_cache_limit(&mut fragments, 0);

        assert_eq!(fragments.len(), MAX_FRAGMENT_CACHE_ENTRIES);
        assert!(fragments.contains_key(&0));
        assert!(!fragments.contains_key(&1));
        assert!(!fragments.contains_key(&2));
    }

    #[tokio::test]
    async fn sends_and_receives_control_messages() {
        let (left, right) = bind_pair().await;
        let left_addr = left
            .socket
            .local_addr()
            .expect("left should have local addr");
        let right_addr = right
            .socket
            .local_addr()
            .expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();

        sender
            .send_control(&ControlMessage::Heartbeat, right_addr)
            .await
            .expect("control send should succeed");

        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::Control(ControlMessage::Heartbeat, addr) => {
                assert_eq!(addr, left_addr);
            }
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sends_and_receives_rtp_packets() {
        let (left, right) = bind_pair().await;
        let right_addr = right
            .socket
            .local_addr()
            .expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        let packet = rtp_packet(32);

        sender
            .send_rtp(&packet, right_addr)
            .await
            .expect("rtp send should succeed");

        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::Rtp(decoded, _) => assert_eq!(decoded, packet),
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    #[tokio::test]
    async fn reassembles_fragmented_rtp_packets() {
        let (left, right) = bind_pair().await;
        let right_addr = right
            .socket
            .local_addr()
            .expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        let packet = rtp_packet(5_000);

        sender
            .send_rtp(&packet, right_addr)
            .await
            .expect("fragmented rtp send should succeed");

        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::Rtp(decoded, _) => assert_eq!(decoded, packet),
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sends_and_receives_data_envelopes() {
        let (left, right) = bind_pair().await;
        let right_addr = right
            .socket
            .local_addr()
            .expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        let envelope = data_envelope(32);

        sender
            .send_data(&envelope, right_addr)
            .await
            .expect("data send should succeed");

        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::Data(decoded, _) => assert_eq!(decoded, envelope),
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    #[tokio::test]
    async fn reassembles_fragmented_data_envelopes() {
        let (left, right) = bind_pair().await;
        let right_addr = right
            .socket
            .local_addr()
            .expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        let envelope = data_envelope(5_000);

        sender
            .send_data(&envelope, right_addr)
            .await
            .expect("fragmented data send should succeed");

        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::Data(decoded, _) => assert_eq!(decoded, envelope),
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sends_and_receives_video_media_data_plane_packets() {
        let (left, right) = bind_pair().await;
        let right_addr = right
            .socket
            .local_addr()
            .expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        let packet = rtp_packet(512);
        let envelope = rtp_to_realtime_data(&packet).expect("video packet should adapt");

        sender
            .send_data(&envelope, right_addr)
            .await
            .expect("media data send should succeed");

        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::Data(decoded, _) => {
                let decoded_packet =
                    realtime_data_to_rtp(decoded).expect("media data should adapt back");
                assert_eq!(decoded_packet, packet);
            }
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sends_and_receives_audio_media_data_plane_packets_without_deadline() {
        let (left, right) = bind_pair().await;
        let right_addr = right
            .socket
            .local_addr()
            .expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        let packet = audio_packet(96);
        let envelope = rtp_to_realtime_data(&packet).expect("audio packet should adapt");
        assert_eq!(envelope.header.deadline_ms, None);

        sender
            .send_data(&envelope, right_addr)
            .await
            .expect("media data send should succeed");

        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::Data(decoded, _) => {
                assert_eq!(decoded.header.deadline_ms, None);
                let decoded_packet =
                    realtime_data_to_rtp(decoded).expect("media data should adapt back");
                assert_eq!(decoded_packet, packet);
            }
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sends_and_receives_reliable_object_envelopes_on_full_data_path() {
        let (left, right) = bind_pair().await;
        let right_addr = right
            .socket
            .local_addr()
            .expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        let envelope = reliable_envelope(512);

        sender
            .send_data(&envelope, right_addr)
            .await
            .expect("reliable object data send should succeed");

        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::Data(decoded, _) => assert_eq!(decoded, envelope),
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    #[tokio::test]
    async fn returns_error_for_unknown_header() {
        let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("receiver should bind");
        let receiver_addr = receiver_mux
            .socket
            .local_addr()
            .expect("receiver should have local addr");
        let (_, receiver) = receiver_mux.split();
        let raw_sender = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("raw sender should bind");

        raw_sender
            .send_to(&[0xff, 0x00], receiver_addr)
            .await
            .expect("raw send should succeed");

        let err = timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("receive should not time out")
            .expect_err("unknown header should fail");

        assert_eq!(err.to_string(), "Unknown UDP multiplexing header");
    }

    #[tokio::test]
    async fn returns_error_for_zero_fragment_count() {
        let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("receiver should bind");
        let receiver_addr = receiver_mux
            .socket
            .local_addr()
            .expect("receiver should have local addr");
        let (_, receiver) = receiver_mux.split();

        send_raw_fragment(receiver_addr, 1, 0, 0, b"bad").await;

        assert_eq!(
            recv_error(&receiver).await,
            "Invalid UDP fragment total chunk count"
        );
    }

    #[tokio::test]
    async fn returns_error_for_out_of_range_fragment_index() {
        let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("receiver should bind");
        let receiver_addr = receiver_mux
            .socket
            .local_addr()
            .expect("receiver should have local addr");
        let (_, receiver) = receiver_mux.split();

        send_raw_fragment(receiver_addr, 1, 1, 1, b"bad").await;

        assert_eq!(recv_error(&receiver).await, "Invalid UDP fragment index");
    }

    #[tokio::test]
    async fn returns_error_for_mismatched_fragment_count() {
        let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("receiver should bind");
        let receiver_addr = receiver_mux
            .socket
            .local_addr()
            .expect("receiver should have local addr");
        let (_, receiver) = receiver_mux.split();

        send_raw_fragment(receiver_addr, 7, 0, 2, b"first").await;
        send_raw_fragment(receiver_addr, 7, 1, 3, b"second").await;

        assert_eq!(
            recv_error(&receiver).await,
            "Mismatched UDP fragment total chunk count"
        );
    }
}
