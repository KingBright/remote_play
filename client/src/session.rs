use crate::{
    ClipboardRuntimeControl, FileTransferRuntimeControl, HostStats, audio_player::AudioPlayerEvent,
};
use protocol::{
    AudioStreamConfig, ContentKind, PayloadType, RtpPacket, remote_microphone_audio_stream_id,
    remote_system_audio_stream_id,
};
use remote_core::jitter_buffer::JitterBuffer;
use remote_core::media_plane::{audio_stream_config_from_envelope, realtime_data_to_rtp};
use remote_core::net::{MultiplexedPacket, UdpReceiver};
use remote_core::stats::Statistics;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq)]
pub enum ClientSessionEvent {
    HostTelemetry {
        fps: f32,
        encode_latency_ms: f32,
        jitter_ms: f32,
        bitrate_kbps: u32,
        rtt_ms: f32,
        e2e_latency_ms: f32,
        decode_latency_ms: f32,
    },
    MediaReceived {
        session_id: u32,
        payload_type: u8,
    },
}

pub struct ClientSessionReceiverConfig {
    pub bind_addr: SocketAddr,
    pub udp_receiver: UdpReceiver,
    pub stats: Arc<Statistics>,
    pub active_session_id: Arc<AtomicU32>,
    pub host_stats: Arc<RwLock<HostStats>>,
    pub audio_tx: mpsc::Sender<AudioPlayerEvent>,
    pub decode_tx: mpsc::Sender<(RtpPacket, u32)>,
    pub clipboard_control: Option<ClipboardRuntimeControl>,
    pub file_transfer_control: Option<FileTransferRuntimeControl>,
    pub session_event_tx: Option<mpsc::UnboundedSender<ClientSessionEvent>>,
}

pub fn spawn_client_session_receiver(
    config: ClientSessionReceiverConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let ClientSessionReceiverConfig {
            bind_addr,
            udp_receiver,
            stats,
            active_session_id,
            host_stats,
            audio_tx,
            decode_tx,
            clipboard_control,
            file_transfer_control,
            session_event_tx,
        } = config;

        println!("Client listening on {}", bind_addr);
        let mut media_handler = MediaPacketHandler::new(
            stats,
            active_session_id,
            host_stats.clone(),
            audio_tx,
            decode_tx,
            session_event_tx.clone(),
        );

        loop {
            match udp_receiver.recv().await {
                Ok(MultiplexedPacket::Rtp(packet, _addr)) => {
                    let packet_size = packet.payload.len() as u64 + 12;
                    media_handler.handle(packet, packet_size).await;
                }
                Ok(MultiplexedPacket::Control(msg, _addr)) => {
                    match msg {
                        protocol::ControlMessage::Pong {
                            client_send_ts,
                            host_recv_ts,
                            host_send_ts,
                        } => {
                            let now_ms = (std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis()
                                & 0xFFFFFFFFFFFFFFFF) as u64;
                            let rtt = (now_ms.saturating_sub(client_send_ts) as f32).clamp(0.1, 1000.0);
                            let offset = ((host_recv_ts as f64 - client_send_ts as f64)
                                + (host_send_ts as f64 - now_ms as f64))
                                / 2.0;

                            media_handler.update_clock_sync(rtt, offset);
                        }
                        protocol::ControlMessage::HostTelemetry {
                            fps,
                            encode_latency_ms,
                            jitter_ms,
                            bitrate_kbps,
                        } => {
                            let (current_rtt, current_e2e, current_dec) = {
                                let mut hs = host_stats.write().unwrap();
                                hs.fps = fps;
                                hs.latency = encode_latency_ms;
                                hs.jitter = jitter_ms;
                                hs.bitrate_kbps = bitrate_kbps;
                                hs.updated_at = Some(std::time::Instant::now());
                                (hs.rtt_ms, hs.e2e_latency_ms, hs.decode_latency_ms)
                            };
                            if let Some(tx) = &session_event_tx {
                                let _ = tx.send(ClientSessionEvent::HostTelemetry {
                                    fps,
                                    encode_latency_ms,
                                    jitter_ms,
                                    bitrate_kbps,
                                    rtt_ms: current_rtt,
                                    e2e_latency_ms: current_e2e,
                                    decode_latency_ms: current_dec,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                Ok(MultiplexedPacket::Data(envelope, _addr)) => {
                    match envelope.header.kind {
                        ContentKind::ClipboardBundle => {
                            if let Some(control) = &clipboard_control {
                                control.route_inbound(envelope);
                            }
                            continue;
                        }
                        ContentKind::FileManifest
                        | ContentKind::FileChunk
                        | ContentKind::FileControl => {
                            if let Some(control) = &file_transfer_control {
                                control.route_inbound(envelope);
                            }
                            continue;
                        }
                        ContentKind::AudioStreamConfig => {
                            match audio_stream_config_from_envelope(&envelope) {
                                Ok(config) => {
                                    media_handler.handle_audio_stream_config(config).await;
                                }
                                Err(e) => {
                                    eprintln!("Ignoring invalid audio stream config: {}", e);
                                }
                            }
                            continue;
                        }
                        _ => {}
                    }

                    let packet_size = envelope.payload.len() as u64
                        + protocol::COMPACT_REALTIME_HEADER_LEN as u64;
                    match realtime_data_to_rtp(envelope) {
                        Ok(packet) => {
                            media_handler.handle(packet, packet_size).await;
                        }
                        Err(e) => {
                            eprintln!("Ignoring unsupported data-plane media packet: {}", e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("UDP Receive Error: {}", e);
                }
            }
        }
    })
}

struct MediaPacketHandler {
    stats_net: Arc<Statistics>,
    receiver_session_id: Arc<AtomicU32>,
    host_stats: Arc<RwLock<HostStats>>,
    audio_tx: mpsc::Sender<AudioPlayerEvent>,
    decode_tx: mpsc::Sender<(RtpPacket, u32)>,
    session_event_tx: Option<mpsc::UnboundedSender<ClientSessionEvent>>,
    video_jitter_buffer: JitterBuffer,
    video_expected_seq_init: bool,
    last_video_ssrc: u32,
    last_seq: u16,
    packets_expected: u64,
    packets_lost: u64,
    loss_rate_pct: f32,
    last_loss_calc: Instant,
    audio_stream_session_id: u32,
    audio_stream_ids: HashSet<u32>,
    clock_offset_ms: f64,
    rtt_ms: f32,
    smooth_e2e_ms: f32,
}

impl MediaPacketHandler {
    fn new(
        stats_net: Arc<Statistics>,
        receiver_session_id: Arc<AtomicU32>,
        host_stats: Arc<RwLock<HostStats>>,
        audio_tx: mpsc::Sender<AudioPlayerEvent>,
        decode_tx: mpsc::Sender<(RtpPacket, u32)>,
        session_event_tx: Option<mpsc::UnboundedSender<ClientSessionEvent>>,
    ) -> Self {
        Self {
            stats_net,
            receiver_session_id,
            host_stats,
            audio_tx,
            decode_tx,
            session_event_tx,
            video_jitter_buffer: JitterBuffer::new(0),
            video_expected_seq_init: false,
            last_video_ssrc: 0,
            last_seq: 0,
            packets_expected: 0,
            packets_lost: 0,
            loss_rate_pct: 0.0,
            last_loss_calc: Instant::now(),
            audio_stream_session_id: 0,
            audio_stream_ids: HashSet::new(),
            clock_offset_ms: 0.0,
            rtt_ms: 0.0,
            smooth_e2e_ms: 0.0,
        }
    }

    pub fn update_clock_sync(&mut self, rtt: f32, offset: f64) {
        if self.rtt_ms <= 0.01 {
            self.rtt_ms = rtt;
            self.clock_offset_ms = offset;
        } else {
            self.rtt_ms = self.rtt_ms * 0.8 + rtt * 0.2;
            self.clock_offset_ms = self.clock_offset_ms * 0.8 + offset * 0.2;
        }
        if let Ok(mut hs) = self.host_stats.write() {
            hs.rtt_ms = self.rtt_ms;
        }
    }

    async fn handle(&mut self, packet: RtpPacket, packet_size: u64) {
        self.stats_net.udp_packets_recv.fetch_add(1, Relaxed);
        self.stats_net
            .udp_bytes_recv
            .fetch_add(packet_size, Relaxed);

        let current_session = self.receiver_session_id.load(Relaxed);
        self.sync_audio_session(current_session);
        if current_session != 0 {
            if packet.header.payload_type == PayloadType::AudioOpus as u8
                && !self.audio_stream_ids.contains(&packet.header.ssrc)
            {
                return;
            }
            if packet.header.payload_type == PayloadType::VideoH265 as u8
                && packet.header.ssrc != current_session
            {
                return;
            }
        }

        self.emit_media_received(&packet, current_session);

        if packet.header.payload_type == PayloadType::AudioOpus as u8 {
            let _ = self.audio_tx.send(AudioPlayerEvent::Packet(packet)).await;
        } else if packet.header.payload_type == PayloadType::VideoH265 as u8 {
            self.handle_video(packet);
        }
    }

    async fn handle_audio_stream_config(&mut self, config: AudioStreamConfig) {
        let current_session = self.receiver_session_id.load(Relaxed);
        self.sync_audio_session(current_session);
        if current_session != 0
            && config.stream_id != remote_microphone_audio_stream_id(current_session)
            && config.stream_id != remote_system_audio_stream_id(current_session)
        {
            return;
        }

        self.audio_stream_ids.insert(config.stream_id);
        let _ = self
            .audio_tx
            .send(AudioPlayerEvent::StreamConfig(config))
            .await;
    }

    fn sync_audio_session(&mut self, current_session: u32) {
        if self.audio_stream_session_id == current_session {
            return;
        }

        self.audio_stream_session_id = current_session;
        self.audio_stream_ids.clear();
        if current_session != 0 {
            self.audio_stream_ids
                .insert(remote_microphone_audio_stream_id(current_session));
        }
    }

    fn emit_media_received(&self, packet: &RtpPacket, current_session: u32) {
        let Some(tx) = &self.session_event_tx else {
            return;
        };
        let session_id = match packet.header.payload_type {
            payload_type if payload_type == PayloadType::VideoH265 as u8 => packet.header.ssrc,
            payload_type
                if payload_type == PayloadType::AudioOpus as u8 && current_session != 0 =>
            {
                current_session
            }
            _ => return,
        };
        let _ = tx.send(ClientSessionEvent::MediaReceived {
            session_id,
            payload_type: packet.header.payload_type,
        });
    }

    fn handle_video(&mut self, packet: RtpPacket) {
        let seq = packet.header.sequence_number;
        if self.last_seq > 0 {
            let diff = seq.wrapping_sub(self.last_seq);
            if diff > 1 && diff < 1000 {
                self.packets_lost += (diff - 1) as u64;
            }
            self.packets_expected += (diff as u64).max(1);
        }
        self.last_seq = seq;

        if self.last_loss_calc.elapsed() >= Duration::from_secs(1) {
            if self.packets_expected > 0 {
                let rate = (self.packets_lost as f32 / self.packets_expected as f32 * 100.0).clamp(0.0, 100.0);
                self.loss_rate_pct = self.loss_rate_pct * 0.7 + rate * 0.3;
            } else {
                self.loss_rate_pct = 0.0;
            }
            self.packets_expected = 0;
            self.packets_lost = 0;
            self.last_loss_calc = Instant::now();
        }

        if packet.header.ssrc != self.last_video_ssrc {
            self.last_video_ssrc = packet.header.ssrc;
            self.video_expected_seq_init = false;
        }

        if !self.video_expected_seq_init {
            self.video_jitter_buffer = JitterBuffer::new(packet.header.sequence_number);
            self.video_expected_seq_init = true;
        }
        self.video_jitter_buffer.push(packet);
        self.stats_net
            .video_jitter_buffer_push
            .fetch_add(1, Relaxed);

        while let Some(ordered_pkt) = self.video_jitter_buffer.pop() {
            let recv_time = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                & 0xFFFFFFFF) as u32;

            if self.rtt_ms > 0.0 && ordered_pkt.header.timestamp > 0 {
                let capture_in_client_time = (ordered_pkt.header.timestamp as f64 - self.clock_offset_ms) as f32;
                let sample_e2e = (recv_time as f32 - capture_in_client_time + (self.rtt_ms / 2.0)).clamp(0.5, 300.0);
                if self.smooth_e2e_ms <= 0.01 {
                    self.smooth_e2e_ms = sample_e2e;
                } else {
                    self.smooth_e2e_ms = self.smooth_e2e_ms * 0.9 + sample_e2e * 0.1;
                }
            }

            if let Ok(mut hs) = self.host_stats.write() {
                hs.e2e_latency_ms = self.smooth_e2e_ms;
                hs.rtt_ms = self.rtt_ms;
                hs.packet_loss_rate = self.loss_rate_pct;
                hs.jitter_buffer_depth = self.video_jitter_buffer.len();
                if self.loss_rate_pct > 3.0 {
                    hs.link_status = "🟡 网络丢包波动";
                    hs.last_anomaly_reason = Some(format!("丢包率 {:.1}%", self.loss_rate_pct));
                } else if self.rtt_ms > 80.0 {
                    hs.link_status = "🟡 传输延迟偏高";
                    hs.last_anomaly_reason = Some(format!("网络 RTT {:.1}ms", self.rtt_ms));
                } else {
                    hs.link_status = "🟢 流畅极佳";
                    hs.last_anomaly_reason = None;
                }
            }

            self.stats_net.video_jitter_buffer_pop.fetch_add(1, Relaxed);
            let _ = self.decode_tx.try_send((ordered_pkt, recv_time));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{ControlMessage, RtpHeader};
    use remote_core::net::UdpMultiplexer;
    use std::time::Duration;

    fn video_packet(session_id: u32) -> RtpPacket {
        RtpPacket {
            header: RtpHeader {
                version: 2,
                payload_type: PayloadType::VideoH265 as u8,
                sequence_number: 1,
                timestamp: 100,
                ssrc: session_id,
            },
            payload: vec![1, 2, 3],
        }
    }

    fn test_media_handler(
        active_session_id: Arc<AtomicU32>,
        session_event_tx: mpsc::UnboundedSender<ClientSessionEvent>,
    ) -> MediaPacketHandler {
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (decode_tx, _decode_rx) = mpsc::channel(4);
        let host_stats = Arc::new(RwLock::new(HostStats::default()));
        MediaPacketHandler::new(
            Statistics::new(),
            active_session_id,
            host_stats,
            audio_tx,
            decode_tx,
            Some(session_event_tx),
        )
    }

    #[tokio::test]
    async fn media_handler_emits_media_received_after_session_filtering() {
        let active_session_id = Arc::new(AtomicU32::new(7));
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut handler = test_media_handler(active_session_id, event_tx);

        handler.handle(video_packet(7), 16).await;

        assert_eq!(
            event_rx.try_recv().unwrap(),
            ClientSessionEvent::MediaReceived {
                session_id: 7,
                payload_type: PayloadType::VideoH265 as u8,
            }
        );
    }

    #[tokio::test]
    async fn media_handler_does_not_emit_filtered_old_session_media() {
        let active_session_id = Arc::new(AtomicU32::new(7));
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut handler = test_media_handler(active_session_id, event_tx);

        handler.handle(video_packet(8), 16).await;

        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn receiver_emits_host_telemetry_event() {
        let client_mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client_mux.local_addr().unwrap();
        let (_client_sender, udp_receiver) = client_mux.split();
        let sender_mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _sender_rx) = sender_mux.split();
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (decode_tx, _decode_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let host_stats = Arc::new(RwLock::new(HostStats::default()));
        let receiver = spawn_client_session_receiver(ClientSessionReceiverConfig {
            bind_addr: client_addr,
            udp_receiver,
            stats: Statistics::new(),
            active_session_id: Arc::new(AtomicU32::new(0)),
            host_stats: host_stats.clone(),
            audio_tx,
            decode_tx,
            clipboard_control: None,
            file_transfer_control: None,
            session_event_tx: Some(event_tx),
        });

        sender
            .send_control(
                &ControlMessage::HostTelemetry {
                    fps: 60.0,
                    encode_latency_ms: 4.0,
                    jitter_ms: 1.0,
                    bitrate_kbps: 8_000,
                },
                client_addr,
            )
            .await
            .unwrap();

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
                .await
                .unwrap()
                .unwrap(),
            ClientSessionEvent::HostTelemetry {
                fps: 60.0,
                encode_latency_ms: 4.0,
                jitter_ms: 1.0,
                bitrate_kbps: 8_000,
                rtt_ms: 0.0,
                e2e_latency_ms: 0.0,
                decode_latency_ms: 0.0,
            }
        );
        assert!(
            host_stats.read().unwrap().updated_at.is_some(),
            "receiving telemetry must record its local arrival time",
        );
        receiver.abort();
    }
}
