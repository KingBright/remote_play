use crate::session_crypto::{MULTIPLEX_ENCRYPTED, SessionCrypto};
use protocol::{CompactRealtimeError, ControlMessage, DataEnvelope, RtpPacket};
use std::error::Error;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
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

        let _ = socket2_sock.set_recv_buffer_size(4 * 1024 * 1024);
        let _ = socket2_sock.set_send_buffer_size(4 * 1024 * 1024);
        #[cfg(target_os = "macos")]
        {
            let _ = socket2_sock.set_tos_v4(0xB8);
        }
        socket2_sock.set_nonblocking(true)?;
        socket2_sock.bind(&std_addr.into())?;

        let std_socket: std::net::UdpSocket = socket2_sock.into();
        let socket = UdpSocket::from_std(std_socket)?;

        Ok(Self {
            socket: Arc::new(socket),
        })
    }

    pub fn split(&self) -> (UdpSender, UdpReceiver) {
        let crypto = Arc::new(OnceLock::new());
        (
            UdpSender {
                socket: self.socket.clone(),
                crypto: crypto.clone(),
            },
            UdpReceiver {
                socket: self.socket.clone(),
                fragments: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
                crypto,
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
    crypto: Arc<OnceLock<SessionCrypto>>,
}
impl UdpSender {
    pub fn install_crypto(&self, crypto: SessionCrypto) -> bool {
        self.crypto.set(crypto).is_ok()
    }

    async fn send_multiplexed(
        &self,
        header: u8,
        bytes: &[u8],
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if let Some(crypto) = self.crypto.get() {
            let mut inner = Vec::with_capacity(1 + bytes.len());
            inner.push(header);
            inner.extend_from_slice(bytes);
            let sealed = crypto.seal(&inner)?;
            return self.send_raw(MULTIPLEX_ENCRYPTED, &sealed, target).await;
        }
        self.send_raw(header, bytes, target).await
    }

    async fn send_raw(
        &self,
        header: u8,
        bytes: &[u8],
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let max_payload = MAX_FRAGMENT_PAYLOAD;
        if bytes.len() > MAX_FRAGMENT_PAYLOAD * MAX_FRAGMENT_CHUNKS {
            return Err("UDP message exceeds fragment size limit".into());
        }
        if bytes.len() <= max_payload {
            let mut buf = [0u8; 1401];
            buf[0] = header;
            buf[1..1 + bytes.len()].copy_from_slice(bytes);
            self.socket.send_to(&buf[..1 + bytes.len()], target).await?;
        } else {
            // Fragment the packet
            // Format: 0x03, original_header(1), fragment_id(4), chunk_idx(2), total_chunks(2), data...
            let fragment_id = rand::random::<u32>();
            let chunks = bytes.chunks(max_payload);
            let total_chunks = chunks.len() as u16;

            let mut buf = [0u8; 1410];
            buf[0] = 0x03; // Fragment multiplex header
            buf[1] = header;
            buf[2..6].copy_from_slice(&fragment_id.to_be_bytes());
            buf[8..10].copy_from_slice(&total_chunks.to_be_bytes());

            for (i, chunk) in chunks.enumerate() {
                buf[6..8].copy_from_slice(&(i as u16).to_be_bytes());
                buf[10..10 + chunk.len()].copy_from_slice(chunk);
                self.socket
                    .send_to(&buf[..10 + chunk.len()], target)
                    .await?;
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
        // Session handshake must remain plaintext so peers can derive the AEAD key.
        if matches!(
            msg,
            ControlMessage::SessionHello { .. }
                | ControlMessage::SessionAccept { .. }
                | ControlMessage::SessionReject { .. }
        ) {
            return self.send_raw(0x02, &bytes, target).await;
        }
        self.send_multiplexed(0x02, &bytes, target).await
    }

    pub async fn send_data(
        &self,
        envelope: &DataEnvelope,
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.send_data_with_timing(envelope, None, target).await
    }

    pub async fn send_data_with_timing(
        &self,
        envelope: &DataEnvelope,
        timing: Option<protocol::FrameTimingCheckpoints>,
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        match envelope.encode_compact_realtime_with_timing(timing) {
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
const MAX_FRAGMENT_PAYLOAD: usize = 1400;
const MAX_FRAGMENT_CHUNKS: usize = 4096;

type FragmentChunks = Vec<Option<Vec<u8>>>;
// Fragment IDs are only unique within one sender and multiplexed content type.
type FragmentKey = (SocketAddr, u8, u32);
type FragmentMap = HashMap<FragmentKey, FragmentEntry>;

#[derive(Debug)]
struct FragmentEntry {
    received_chunks: u16,
    received_bytes: usize,
    chunks: FragmentChunks,
    created_at: Instant,
}

impl FragmentEntry {
    fn new(total_chunks: u16, now: Instant) -> Self {
        Self {
            received_chunks: 0,
            received_bytes: 0,
            chunks: vec![None; total_chunks as usize],
            created_at: now,
        }
    }
}

pub struct UdpReceiver {
    socket: Arc<UdpSocket>,
    fragments: Arc<Mutex<FragmentMap>>,
    crypto: Arc<OnceLock<SessionCrypto>>,
}

#[derive(Debug)]
pub enum MultiplexedPacket {
    Rtp(RtpPacket, SocketAddr),
    Control(ControlMessage, SocketAddr),
    Data(DataEnvelope, SocketAddr),
    DataWithTiming(
        DataEnvelope,
        Option<protocol::FrameTimingCheckpoints>,
        SocketAddr,
    ),
}

impl UdpReceiver {
    pub fn install_crypto(&self, crypto: SessionCrypto) -> bool {
        self.crypto.set(crypto).is_ok()
    }

    fn decode_payload(
        &self,
        buf: &[u8],
        len: usize,
        addr: SocketAddr,
    ) -> Result<Option<MultiplexedPacket>, Box<dyn Error + Send + Sync>> {
        if len == 0 {
            return Ok(None);
        }
        match buf[0] {
            MULTIPLEX_ENCRYPTED => {
                let Some(crypto) = self.crypto.get() else {
                    return Err("encrypted packet received without session crypto".into());
                };
                let opened = crypto.open(&buf[1..len])?;
                self.decode_payload(&opened, opened.len(), addr)
            }
            0x01 => {
                let rtp = RtpPacket::decode(&buf[1..len])?;
                Ok(Some(MultiplexedPacket::Rtp(rtp, addr)))
            }
            0x02 => {
                let msg = ControlMessage::decode(&buf[1..len])?;
                Ok(Some(MultiplexedPacket::Control(msg, addr)))
            }
            0x04 => {
                let envelope = DataEnvelope::decode(&buf[1..len])?;
                Ok(Some(MultiplexedPacket::Data(envelope, addr)))
            }
            0x05 => {
                let (envelope, timing) =
                    DataEnvelope::decode_compact_realtime_with_timing(&buf[1..len])?;
                if timing.is_some() {
                    Ok(Some(MultiplexedPacket::DataWithTiming(
                        envelope, timing, addr,
                    )))
                } else {
                    Ok(Some(MultiplexedPacket::Data(envelope, addr)))
                }
            }
            _ => Err("Unknown UDP multiplexing header".into()),
        }
    }

    fn cleanup_expired_fragments(fragments: &mut FragmentMap, now: Instant) {
        fragments.retain(|_, entry| now.duration_since(entry.created_at) <= MAX_FRAGMENT_AGE);
    }

    fn enforce_fragment_cache_limit(fragments: &mut FragmentMap, protected_id: FragmentKey) {
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
        // Keep the established receive-buffer layout: the small-buffer variant
        // regressed small-packet loopback performance on the macOS target.
        let mut buf = [0u8; 65536];
        loop {
            let (len, addr) = self.socket.recv_from(&mut buf).await?;
            if len == 0 {
                continue;
            }
            if len > MAX_FRAGMENT_PAYLOAD + 10 {
                return Err(if buf[0] == 0x03 {
                    "Invalid UDP fragment payload size"
                } else {
                    "UDP datagram exceeds size limit"
                }
                .into());
            }
            match buf[0] {
                0x03 => {
                    if len < 10 {
                        continue;
                    }
                    let header = buf[1];
                    let fragment_id = u32::from_be_bytes(buf[2..6].try_into().unwrap());
                    let chunk_idx = u16::from_be_bytes(buf[6..8].try_into().unwrap());
                    let total_chunks = u16::from_be_bytes(buf[8..10].try_into().unwrap());
                    let fragment_key = (addr, header, fragment_id);

                    if total_chunks == 0 || total_chunks as usize > MAX_FRAGMENT_CHUNKS {
                        return Err("Invalid UDP fragment total chunk count".into());
                    }

                    if chunk_idx >= total_chunks {
                        return Err("Invalid UDP fragment index".into());
                    }
                    if len == 10 || len - 10 > MAX_FRAGMENT_PAYLOAD {
                        return Err("Invalid UDP fragment payload size".into());
                    }
                    if !matches!(header, 0x01 | 0x02 | 0x04 | 0x05 | MULTIPLEX_ENCRYPTED) {
                        return Err("Invalid UDP fragment multiplexing header".into());
                    }

                    let mut fragments = self.fragments.lock().await;
                    let now = Instant::now();
                    Self::cleanup_expired_fragments(&mut fragments, now);

                    let entry = match fragments.entry(fragment_key) {
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
                        entry.received_bytes += len - 10;
                    }

                    if entry.received_chunks == total_chunks {
                        let entry = fragments.remove(&fragment_key).expect("completed fragment");
                        drop(fragments);
                        let mut assembled = Vec::with_capacity(1 + entry.received_bytes);
                        assembled.push(header);
                        for chunk in entry.chunks.into_iter().flatten() {
                            assembled.extend_from_slice(&chunk);
                        }
                        if let Some(packet) =
                            self.decode_payload(&assembled, assembled.len(), addr)?
                        {
                            return Ok(packet);
                        }
                    } else {
                        Self::enforce_fragment_cache_limit(&mut fragments, fragment_key);
                    }
                }
                _ => {
                    if let Some(packet) = self.decode_payload(&buf[..len], len, addr)? {
                        return Ok(packet);
                    }
                }
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
    ) -> UdpSocket {
        let raw_sender = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("raw sender should bind");
        send_fragment_from(
            &raw_sender,
            target,
            0x01,
            fragment_id,
            chunk_idx,
            total_chunks,
            data,
        )
        .await;
        raw_sender
    }

    async fn send_fragment_from(
        raw_sender: &UdpSocket,
        target: SocketAddr,
        header: u8,
        fragment_id: u32,
        chunk_idx: u16,
        total_chunks: u16,
        data: &[u8],
    ) {
        let mut bytes = Vec::with_capacity(10 + data.len());
        bytes.push(0x03);
        bytes.push(header);
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

    fn fragment_key(id: u32) -> FragmentKey {
        ("127.0.0.1:39271".parse().unwrap(), 0x01, id)
    }

    #[tokio::test]
    async fn receive_remains_usable_after_cancellation() {
        let (left, right) = bind_pair().await;
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        assert!(
            timeout(Duration::from_millis(5), receiver.recv())
                .await
                .is_err()
        );
        let packet = rtp_packet(32);
        sender
            .send_rtp(&packet, right.local_addr().unwrap())
            .await
            .unwrap();
        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::Rtp(actual, _) => assert_eq!(actual, packet),
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    #[tokio::test]
    async fn same_fragment_id_from_different_peers_is_isolated() {
        let (_, right) = bind_pair().await;
        let target = right.local_addr().unwrap();
        let (_, receiver) = right.split();
        let first = rtp_packet(64);
        let mut second = first.clone();
        second.payload.fill(9);
        let a = first.encode().unwrap();
        let b = second.encode().unwrap();
        let sender_a = send_raw_fragment(target, 7, 1, 2, &a[40..]).await;
        let sender_b = send_raw_fragment(target, 7, 0, 2, &b[..40]).await;
        send_fragment_from(&sender_a, target, 0x01, 7, 0, 2, &a[..40]).await;
        send_fragment_from(&sender_b, target, 0x01, 7, 1, 2, &b[40..]).await;
        for (expected, sender) in [(first, sender_a), (second, sender_b)] {
            match recv_with_timeout(&receiver).await {
                MultiplexedPacket::Rtp(packet, addr) => {
                    assert_eq!(packet, expected);
                    assert_eq!(addr, sender.local_addr().unwrap());
                }
                packet => panic!("unexpected packet: {packet:?}"),
            }
        }
        assert!(receiver.fragments.lock().await.is_empty());
    }

    #[tokio::test]
    async fn invalid_fragment_sizes_are_rejected_before_allocating() {
        let (_, right) = bind_pair().await;
        let target = right.local_addr().unwrap();
        let (_, receiver) = right.split();
        for (count, payload, error) in [
            (u16::MAX, vec![1], "Invalid UDP fragment total chunk count"),
            (
                2,
                vec![1; MAX_FRAGMENT_PAYLOAD + 1],
                "Invalid UDP fragment payload size",
            ),
            (2, vec![], "Invalid UDP fragment payload size"),
        ] {
            send_raw_fragment(target, 1, 0, count, &payload).await;
            assert_eq!(recv_error(&receiver).await, error);
            assert!(receiver.fragments.lock().await.is_empty());
        }
    }

    #[tokio::test]
    async fn sender_rejects_unrepresentable_message_sizes() {
        let (left, right) = bind_pair().await;
        let (sender, _) = left.split();
        let payload = vec![0; MAX_FRAGMENT_PAYLOAD * MAX_FRAGMENT_CHUNKS + 1];
        let error = sender
            .send_raw(0x01, &payload, right.local_addr().unwrap())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "UDP message exceeds fragment size limit");
    }

    #[test]
    fn cleanup_expired_fragments_removes_old_entries() {
        let now = Instant::now();
        let mut fragments = FragmentMap::new();
        fragments.insert(
            fragment_key(1),
            fragment_entry(2, now - MAX_FRAGMENT_AGE - Duration::from_millis(1)),
        );
        fragments.insert(fragment_key(2), fragment_entry(2, now));

        UdpReceiver::cleanup_expired_fragments(&mut fragments, now);

        assert!(!fragments.contains_key(&fragment_key(1)));
        assert!(fragments.contains_key(&fragment_key(2)));
    }

    #[test]
    fn enforce_fragment_cache_limit_removes_oldest_unprotected_entries() {
        let now = Instant::now();
        let mut fragments = FragmentMap::new();

        for id in 0..(MAX_FRAGMENT_CACHE_ENTRIES as u32 + 2) {
            fragments.insert(
                fragment_key(id),
                fragment_entry(2, now + Duration::from_millis(id as u64)),
            );
        }

        UdpReceiver::enforce_fragment_cache_limit(&mut fragments, fragment_key(0));

        assert_eq!(fragments.len(), MAX_FRAGMENT_CACHE_ENTRIES);
        assert!(fragments.contains_key(&fragment_key(0)));
        assert!(!fragments.contains_key(&fragment_key(1)));
        assert!(!fragments.contains_key(&fragment_key(2)));
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
    async fn encrypted_control_roundtrips_after_crypto_install() {
        let (left, right) = bind_pair().await;
        let right_addr = right
            .socket
            .local_addr()
            .expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        let psk = b"test-psk";
        let salt = b"salt-16-bytes!!!!";
        sender.install_crypto(crate::SessionCrypto::from_psk(psk, salt).unwrap());
        receiver.install_crypto(crate::SessionCrypto::from_psk(psk, salt).unwrap());

        sender
            .send_control(&ControlMessage::Heartbeat, right_addr)
            .await
            .expect("encrypted control send should succeed");

        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::Control(ControlMessage::Heartbeat, _) => {}
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

        let raw_sender = send_raw_fragment(receiver_addr, 7, 0, 2, b"first").await;
        send_fragment_from(&raw_sender, receiver_addr, 0x01, 7, 1, 3, b"second").await;

        assert_eq!(
            recv_error(&receiver).await,
            "Mismatched UDP fragment total chunk count"
        );
    }

    #[tokio::test]
    async fn sends_and_receives_data_envelopes_with_timing() {
        let (left, right) = bind_pair().await;
        let right_addr = right
            .socket
            .local_addr()
            .expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        let envelope = data_envelope(32);
        let mut timing = protocol::FrameTimingCheckpoints::new(123_456_000);
        timing.encode_queue_ts_us = 100;
        timing.encode_done_ts_us = 200;
        timing.packetize_ts_us = 300;
        timing.send_ts_us = 400;

        sender
            .send_data_with_timing(&envelope, Some(timing), right_addr)
            .await
            .expect("data send with timing should succeed");

        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::DataWithTiming(decoded, decoded_timing, _) => {
                assert_eq!(decoded, envelope);
                assert_eq!(decoded_timing, Some(timing));
            }
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    #[tokio::test]
    async fn reassembles_fragmented_data_envelopes_with_timing() {
        let (left, right) = bind_pair().await;
        let right_addr = right
            .socket
            .local_addr()
            .expect("right should have local addr");
        let (sender, _) = left.split();
        let (_, receiver) = right.split();
        let envelope = data_envelope(5_000);
        let mut timing = protocol::FrameTimingCheckpoints::new(999_888_000);
        timing.encode_queue_ts_us = 150;
        timing.encode_done_ts_us = 250;
        timing.packetize_ts_us = 350;
        timing.send_ts_us = 450;

        sender
            .send_data_with_timing(&envelope, Some(timing), right_addr)
            .await
            .expect("fragmented data send with timing should succeed");

        match recv_with_timeout(&receiver).await {
            MultiplexedPacket::DataWithTiming(decoded, decoded_timing, _) => {
                assert_eq!(decoded, envelope);
                assert_eq!(decoded_timing, Some(timing));
            }
            other => panic!("unexpected packet: {other:?}"),
        }
    }
}
