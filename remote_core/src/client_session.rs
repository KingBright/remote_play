use crate::jitter_buffer::JitterBuffer;
use crate::media_plane::{audio_stream_config_from_envelope, realtime_data_to_rtp};
use crate::net::{MultiplexedPacket, UdpReceiver};
use crate::stats::Statistics;
use protocol::{
    AudioStreamConfig, ContentKind, FrameTimingCheckpoints, PayloadType, PipelineTelemetryReport,
    RtpPacket, remote_microphone_audio_stream_id, remote_system_audio_stream_id,
};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub trait EnvelopeIngress: Send + Sync {
    fn route_inbound(&self, envelope: protocol::DataEnvelope);
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioIngressEvent {
    StreamConfig(AudioStreamConfig),
    Packet(RtpPacket),
}

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

#[derive(Default, Clone, Debug, PartialEq)]
pub struct HostStats {
    pub fps: f32,
    pub latency: f32,
    pub jitter: f32,
    pub bitrate_kbps: u32,
    pub rtt_ms: f32,
    pub e2e_latency_ms: f32,
    pub decode_latency_ms: f32,
    pub packet_loss_rate: f32,
    pub jitter_buffer_depth: usize,
    pub decode_errors: u64,
    pub link_status: &'static str,
    pub last_anomaly_reason: Option<String>,
    pub updated_at: Option<Instant>,
    pub pipeline_report: Option<PipelineTelemetryReport>,
    pub latest_timing: Option<FrameTimingCheckpoints>,
}

#[derive(Default)]
struct HostStatsExtra {
    link_status: &'static str,
    last_anomaly_reason: Option<String>,
    updated_at: Option<Instant>,
    pipeline_report: Option<PipelineTelemetryReport>,
    latest_timing: Option<FrameTimingCheckpoints>,
}

/// Lock-free numeric telemetry with a mutex only for the occasional report/string fields.
pub struct SharedHostStats {
    fps: AtomicU32,
    latency: AtomicU32,
    jitter: AtomicU32,
    bitrate_kbps: AtomicU32,
    rtt_ms: AtomicU32,
    e2e_latency_ms: AtomicU32,
    decode_latency_ms: AtomicU32,
    packet_loss_rate: AtomicU32,
    jitter_buffer_depth: AtomicU32,
    decode_errors: AtomicU64,
    clock_offset_us: AtomicI64,
    extra: Mutex<HostStatsExtra>,
}

impl Default for SharedHostStats {
    fn default() -> Self {
        Self {
            fps: AtomicU32::new(0),
            latency: AtomicU32::new(0),
            jitter: AtomicU32::new(0),
            bitrate_kbps: AtomicU32::new(0),
            rtt_ms: AtomicU32::new(0),
            e2e_latency_ms: AtomicU32::new(0),
            decode_latency_ms: AtomicU32::new(0),
            packet_loss_rate: AtomicU32::new(0),
            jitter_buffer_depth: AtomicU32::new(0),
            decode_errors: AtomicU64::new(0),
            clock_offset_us: AtomicI64::new(0),
            extra: Mutex::new(HostStatsExtra::default()),
        }
    }
}

impl SharedHostStats {
    pub fn snapshot(&self) -> HostStats {
        let extra = self.extra.lock().unwrap_or_else(|err| err.into_inner());
        HostStats {
            fps: f32::from_bits(self.fps.load(Relaxed)),
            latency: f32::from_bits(self.latency.load(Relaxed)),
            jitter: f32::from_bits(self.jitter.load(Relaxed)),
            bitrate_kbps: self.bitrate_kbps.load(Relaxed),
            rtt_ms: f32::from_bits(self.rtt_ms.load(Relaxed)),
            e2e_latency_ms: f32::from_bits(self.e2e_latency_ms.load(Relaxed)),
            decode_latency_ms: f32::from_bits(self.decode_latency_ms.load(Relaxed)),
            packet_loss_rate: f32::from_bits(self.packet_loss_rate.load(Relaxed)),
            jitter_buffer_depth: self.jitter_buffer_depth.load(Relaxed) as usize,
            decode_errors: self.decode_errors.load(Relaxed),
            link_status: extra.link_status,
            last_anomaly_reason: extra.last_anomaly_reason.clone(),
            updated_at: extra.updated_at,
            pipeline_report: extra.pipeline_report.clone(),
            latest_timing: extra.latest_timing,
        }
    }

    pub fn apply_host_telemetry(
        &self,
        fps: f32,
        encode_latency_ms: f32,
        jitter_ms: f32,
        bitrate_kbps: u32,
    ) {
        self.fps.store(fps.to_bits(), Relaxed);
        self.latency.store(encode_latency_ms.to_bits(), Relaxed);
        self.jitter.store(jitter_ms.to_bits(), Relaxed);
        self.bitrate_kbps.store(bitrate_kbps, Relaxed);
        if let Ok(mut extra) = self.extra.lock() {
            extra.updated_at = Some(Instant::now());
        }
    }

    pub fn set_rtt(&self, rtt_ms: f32) {
        self.rtt_ms.store(rtt_ms.to_bits(), Relaxed);
    }

    pub fn set_clock_offset_us(&self, offset_us: i64) {
        self.clock_offset_us.store(offset_us, Relaxed);
    }

    pub fn clock_offset_us(&self) -> i64 {
        self.clock_offset_us.load(Relaxed)
    }

    pub fn set_decode_latency(&self, decode_latency_ms: f32) {
        self.decode_latency_ms
            .store(decode_latency_ms.to_bits(), Relaxed);
    }

    pub fn set_pipeline_report(&self, report: PipelineTelemetryReport) {
        if let Ok(mut extra) = self.extra.lock() {
            extra.pipeline_report = Some(report);
        }
    }

    pub fn set_latest_timing(&self, timing: FrameTimingCheckpoints) {
        if let Ok(mut extra) = self.extra.lock() {
            extra.latest_timing = Some(timing);
        }
    }

    pub fn reset(&self) {
        self.fps.store(0, Relaxed);
        self.latency.store(0, Relaxed);
        self.jitter.store(0, Relaxed);
        self.bitrate_kbps.store(0, Relaxed);
        self.rtt_ms.store(0, Relaxed);
        self.e2e_latency_ms.store(0, Relaxed);
        self.decode_latency_ms.store(0, Relaxed);
        self.packet_loss_rate.store(0, Relaxed);
        self.jitter_buffer_depth.store(0, Relaxed);
        self.decode_errors.store(0, Relaxed);
        self.clock_offset_us.store(0, Relaxed);
        if let Ok(mut extra) = self.extra.lock() {
            *extra = HostStatsExtra::default();
        }
    }

    fn update_video_link(
        &self,
        e2e_latency_ms: f32,
        rtt_ms: f32,
        packet_loss_rate: f32,
        jitter_buffer_depth: usize,
    ) {
        self.e2e_latency_ms.store(e2e_latency_ms.to_bits(), Relaxed);
        self.rtt_ms.store(rtt_ms.to_bits(), Relaxed);
        self.packet_loss_rate
            .store(packet_loss_rate.to_bits(), Relaxed);
        self.jitter_buffer_depth
            .store(jitter_buffer_depth as u32, Relaxed);
        if let Ok(mut extra) = self.extra.lock() {
            if packet_loss_rate > 3.0 {
                extra.link_status = "🟡 网络丢包波动";
                extra.last_anomaly_reason = Some(format!("丢包率 {:.1}%", packet_loss_rate));
            } else if rtt_ms > 80.0 {
                extra.link_status = "🟡 传输延迟偏高";
                extra.last_anomaly_reason = Some(format!("网络 RTT {:.1}ms", rtt_ms));
            } else {
                extra.link_status = "🟢 流畅极佳";
                extra.last_anomaly_reason = None;
            }
        }
    }
}

pub struct ClientSessionReceiverConfig {
    pub bind_addr: SocketAddr,
    pub udp_receiver: UdpReceiver,
    pub stats: Arc<Statistics>,
    pub active_session_id: Arc<AtomicU32>,
    pub host_stats: Arc<SharedHostStats>,
    pub audio_tx: mpsc::Sender<AudioIngressEvent>,
    pub decode_tx: mpsc::Sender<(RtpPacket, FrameTimingCheckpoints)>,
    pub clipboard_control: Option<Arc<dyn EnvelopeIngress>>,
    pub file_transfer_control: Option<Arc<dyn EnvelopeIngress>>,
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
            let wait = media_handler.video_jitter_buffer.next_ready_in();
            let received = tokio::select! {
                packet = udp_receiver.recv() => packet,
                _ = async {
                    match wait {
                        Some(delay) => tokio::time::sleep(delay).await,
                        None => std::future::pending().await,
                    }
                } => {
                    media_handler.drain_video();
                    continue;
                }
            };
            match received {
                Ok(MultiplexedPacket::Rtp(packet, _addr)) => {
                    let packet_size = packet.payload.len() as u64 + 12;
                    media_handler.handle(packet, packet_size).await;
                }
                Ok(MultiplexedPacket::Control(msg, _addr)) => match msg {
                    protocol::ControlMessage::Pong {
                        client_send_ts,
                        host_recv_ts,
                        host_send_ts,
                    } => {
                        let now_ms = crate::session_crypto::now_unix_ms();
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
                        host_stats.apply_host_telemetry(
                            fps,
                            encode_latency_ms,
                            jitter_ms,
                            bitrate_kbps,
                        );
                        let snap = host_stats.snapshot();
                        if let Some(tx) = &session_event_tx {
                            let _ = tx.send(ClientSessionEvent::HostTelemetry {
                                fps,
                                encode_latency_ms,
                                jitter_ms,
                                bitrate_kbps,
                                rtt_ms: snap.rtt_ms,
                                e2e_latency_ms: snap.e2e_latency_ms,
                                decode_latency_ms: snap.decode_latency_ms,
                            });
                        }
                    }
                    protocol::ControlMessage::PipelineTelemetry(report) => {
                        host_stats.set_pipeline_report(*report);
                    }
                    _ => {}
                },
                Ok(MultiplexedPacket::Data(envelope, _addr)) => {
                    if route_reliable_envelope(
                        &envelope,
                        clipboard_control.as_deref(),
                        file_transfer_control.as_deref(),
                    ) {
                        continue;
                    }
                    if envelope.header.kind == ContentKind::AudioStreamConfig {
                        match audio_stream_config_from_envelope(&envelope) {
                            Ok(config) => media_handler.handle_audio_stream_config(config).await,
                            Err(e) => eprintln!("Ignoring invalid audio stream config: {}", e),
                        }
                        continue;
                    }
                    let packet_size = envelope.payload.len() as u64
                        + protocol::COMPACT_REALTIME_HEADER_LEN as u64;
                    match realtime_data_to_rtp(envelope) {
                        Ok(packet) => media_handler.handle(packet, packet_size).await,
                        Err(e) => {
                            eprintln!("Ignoring unsupported data-plane media packet: {}", e);
                        }
                    }
                }
                Ok(MultiplexedPacket::DataWithTiming(envelope, timing, _addr)) => {
                    if route_reliable_envelope(
                        &envelope,
                        clipboard_control.as_deref(),
                        file_transfer_control.as_deref(),
                    ) {
                        continue;
                    }
                    if envelope.header.kind == ContentKind::AudioStreamConfig {
                        match audio_stream_config_from_envelope(&envelope) {
                            Ok(config) => media_handler.handle_audio_stream_config(config).await,
                            Err(e) => eprintln!("Ignoring invalid audio stream config: {}", e),
                        }
                        continue;
                    }
                    let extra_len = if timing.is_some() {
                        protocol::HOST_TIMING_WIRE_LEN
                    } else {
                        0
                    };
                    let packet_size = envelope.payload.len() as u64
                        + protocol::COMPACT_REALTIME_HEADER_LEN as u64
                        + extra_len as u64;
                    match realtime_data_to_rtp(envelope) {
                        Ok(packet) => {
                            if let Some(host_timing) = timing {
                                media_handler
                                    .handle_with_host_timing(packet, packet_size, host_timing)
                                    .await;
                            } else {
                                media_handler.handle(packet, packet_size).await;
                            }
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

fn route_reliable_envelope(
    envelope: &protocol::DataEnvelope,
    clipboard_control: Option<&dyn EnvelopeIngress>,
    file_transfer_control: Option<&dyn EnvelopeIngress>,
) -> bool {
    match envelope.header.kind {
        ContentKind::ClipboardBundle => {
            if let Some(control) = clipboard_control {
                control.route_inbound(envelope.clone());
            }
            true
        }
        ContentKind::FileManifest | ContentKind::FileChunk | ContentKind::FileControl => {
            if let Some(control) = file_transfer_control {
                control.route_inbound(envelope.clone());
            }
            true
        }
        _ => false,
    }
}

struct MediaPacketHandler {
    stats_net: Arc<Statistics>,
    receiver_session_id: Arc<AtomicU32>,
    host_stats: Arc<SharedHostStats>,
    audio_tx: mpsc::Sender<AudioIngressEvent>,
    decode_tx: mpsc::Sender<(RtpPacket, FrameTimingCheckpoints)>,
    session_event_tx: Option<mpsc::UnboundedSender<ClientSessionEvent>>,
    video_jitter_buffer: JitterBuffer,
    video_expected_seq_init: bool,
    last_video_ssrc: u32,
    last_seq: Option<u16>,
    packets_expected: u64,
    packets_lost: u64,
    loss_rate_pct: f32,
    last_loss_calc: Instant,
    audio_stream_session_id: u32,
    audio_stream_ids: HashSet<u32>,
    pending_audio_configs: Vec<AudioStreamConfig>,
    clock_offset_ms: f64,
    rtt_ms: f32,
    smooth_e2e_ms: f32,
}

impl MediaPacketHandler {
    fn new(
        stats_net: Arc<Statistics>,
        receiver_session_id: Arc<AtomicU32>,
        host_stats: Arc<SharedHostStats>,
        audio_tx: mpsc::Sender<AudioIngressEvent>,
        decode_tx: mpsc::Sender<(RtpPacket, FrameTimingCheckpoints)>,
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
            last_seq: None,
            packets_expected: 0,
            packets_lost: 0,
            loss_rate_pct: 0.0,
            last_loss_calc: Instant::now(),
            audio_stream_session_id: 0,
            audio_stream_ids: HashSet::new(),
            pending_audio_configs: Vec::with_capacity(2),
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
        self.host_stats.set_rtt(self.rtt_ms);
        self.host_stats
            .set_clock_offset_us((self.clock_offset_ms * 1000.0) as i64);
    }

    pub async fn handle(&mut self, packet: RtpPacket, packet_size: u64) {
        // Legacy RTP carries only the low 32 bits of epoch milliseconds.
        // Expand near the synchronized host clock before deriving stage offsets.
        let host_now_ms = (crate::timing::quanta_now_us() / 1000)
            .saturating_add_signed(self.clock_offset_ms.round() as i64);
        let capture_ts_us = expand_legacy_timestamp(packet.header.timestamp, host_now_ms) * 1000;
        let host_timing = FrameTimingCheckpoints::new(capture_ts_us);
        self.handle_with_host_timing(packet, packet_size, host_timing)
            .await;
    }

    pub async fn handle_with_host_timing(
        &mut self,
        packet: RtpPacket,
        packet_size: u64,
        host_timing: FrameTimingCheckpoints,
    ) {
        let current_session = self.receiver_session_id.load(Relaxed);
        let clock_offset_us = (self.clock_offset_ms * 1000.0) as i64;
        let tracker = crate::timing::ClientFrameTracker::start(
            host_timing,
            crate::timing::global_clock().clone(),
            clock_offset_us,
        );
        let mut timing = tracker.finish();
        timing.jitter_enter_ts_us = timing.recv_ts_us;
        self.handle_timed(packet, packet_size, timing, current_session)
            .await;
    }

    pub async fn handle_timed(
        &mut self,
        packet: RtpPacket,
        packet_size: u64,
        timing: FrameTimingCheckpoints,
        current_session: u32,
    ) {
        self.sync_audio_session(current_session);
        self.stats_net.udp_packets_recv.fetch_add(1, Relaxed);
        self.stats_net
            .udp_bytes_recv
            .fetch_add(packet_size, Relaxed);

        match packet.header.payload_type {
            payload_type if payload_type == PayloadType::VideoH265 as u8 => {
                if current_session != 0 && packet.header.ssrc != current_session {
                    return;
                }
                self.emit_media_received(&packet, current_session);
                self.handle_video_timed(packet, timing);
            }
            payload_type if payload_type == PayloadType::AudioOpus as u8 => {
                if current_session != 0 && !self.audio_stream_ids.contains(&packet.header.ssrc) {
                    return;
                }
                self.emit_media_received(&packet, current_session);
                self.flush_audio_configs();
                if self
                    .pending_audio_configs
                    .iter()
                    .any(|config| config.stream_id == packet.header.ssrc)
                {
                    self.stats_net.audio_ingress_dropped.fetch_add(1, Relaxed);
                    return;
                }
                if self
                    .audio_tx
                    .try_send(AudioIngressEvent::Packet(packet))
                    .is_err()
                {
                    self.stats_net.audio_ingress_dropped.fetch_add(1, Relaxed);
                }
            }
            _ => {}
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
        self.pending_audio_configs
            .retain(|pending| pending.stream_id != config.stream_id);
        // A host has at most a microphone stream and a system/mixed stream.
        if self.pending_audio_configs.len() == 2 {
            self.pending_audio_configs.remove(0);
        }
        self.pending_audio_configs.push(config);
        self.flush_audio_configs();
    }

    fn flush_audio_configs(&mut self) {
        while let Some(config) = self.pending_audio_configs.pop() {
            if let Err(error) = self
                .audio_tx
                .try_send(AudioIngressEvent::StreamConfig(config))
            {
                if let AudioIngressEvent::StreamConfig(config) = error.into_inner() {
                    self.pending_audio_configs.push(config);
                }
                break;
            }
        }
    }

    fn sync_audio_session(&mut self, current_session: u32) {
        if self.audio_stream_session_id == current_session {
            return;
        }

        self.audio_stream_session_id = current_session;
        self.audio_stream_ids.clear();
        self.pending_audio_configs.clear();
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

    fn handle_video_timed(&mut self, packet: RtpPacket, timing: FrameTimingCheckpoints) {
        if packet.header.ssrc != self.last_video_ssrc {
            self.last_video_ssrc = packet.header.ssrc;
            self.video_expected_seq_init = false;
            self.last_seq = None;
            self.packets_expected = 0;
            self.packets_lost = 0;
            self.loss_rate_pct = 0.0;
            self.smooth_e2e_ms = 0.0;
            self.last_loss_calc = Instant::now();
        }

        if !self.video_expected_seq_init {
            self.video_jitter_buffer = JitterBuffer::new(packet.header.sequence_number);
            self.video_expected_seq_init = true;
        }
        self.video_jitter_buffer.push_with_timing(packet, timing);
        self.stats_net
            .video_jitter_buffer_push
            .fetch_add(1, Relaxed);
        self.drain_video();
    }

    fn drain_video(&mut self) {
        while let Some((ordered_pkt, timing)) = self.video_jitter_buffer.pop_with_timing() {
            let seq = ordered_pkt.header.sequence_number;
            let advance = self
                .last_seq
                .map_or(1, |last| seq.wrapping_sub(last) as u64);
            self.packets_expected += advance;
            self.packets_lost += advance.saturating_sub(1);
            self.last_seq = Some(seq);
            if self.last_loss_calc.elapsed() >= Duration::from_secs(1) {
                let rate = self.packets_lost as f32 / self.packets_expected.max(1) as f32 * 100.0;
                self.loss_rate_pct = self.loss_rate_pct * 0.7 + rate * 0.3;
                self.packets_expected = 0;
                self.packets_lost = 0;
                self.last_loss_calc = Instant::now();
            }

            if self.rtt_ms > 0.0 && timing.recv_ts_us > 0 && timing.recv_ts_us < u32::MAX {
                // Checkpoints already contain clock-corrected offsets from capture.
                let sample_e2e = timing.recv_ts_us as f32 / 1000.0;
                if self.smooth_e2e_ms <= 0.01 {
                    self.smooth_e2e_ms = sample_e2e;
                } else {
                    self.smooth_e2e_ms = self.smooth_e2e_ms * 0.9 + sample_e2e * 0.1;
                }
            }

            self.host_stats.update_video_link(
                self.smooth_e2e_ms,
                self.rtt_ms,
                self.loss_rate_pct,
                self.video_jitter_buffer.len(),
            );

            self.stats_net.video_jitter_buffer_pop.fetch_add(1, Relaxed);
            if self.decode_tx.try_send((ordered_pkt, timing)).is_err() {
                self.stats_net
                    .video_decode_queue_dropped
                    .fetch_add(1, Relaxed);
            }
        }
    }
}

fn expand_legacy_timestamp(timestamp_ms: u32, host_now_ms: u64) -> u64 {
    let age_ms = (host_now_ms as u32).wrapping_sub(timestamp_ms) as i32;
    host_now_ms.saturating_add_signed(-i64::from(age_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_capture_timestamp_expands_across_wraparound() {
        let reference = (1u64 << 32) + 5;
        assert_eq!(
            expand_legacy_timestamp(u32::MAX - 4, reference),
            reference - 10
        );
        assert_eq!(expand_legacy_timestamp(10, reference), reference + 5);
    }
    use crate::net::UdpMultiplexer;
    use protocol::{ControlMessage, RtpHeader};
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
        let (decode_tx, _decode_rx) = mpsc::channel::<(RtpPacket, FrameTimingCheckpoints)>(4);
        let host_stats = Arc::new(SharedHostStats::default());
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
        let (decode_tx, _decode_rx) = mpsc::channel::<(RtpPacket, FrameTimingCheckpoints)>(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let host_stats = Arc::new(SharedHostStats::default());
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
            host_stats.snapshot().updated_at.is_some(),
            "receiving telemetry must record its local arrival time",
        );
        receiver.abort();
    }

    #[tokio::test]
    async fn media_handler_preserves_host_timing_checkpoints() {
        let active_session_id = Arc::new(AtomicU32::new(7));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (decode_tx, mut decode_rx) = mpsc::channel::<(RtpPacket, FrameTimingCheckpoints)>(4);
        let host_stats = Arc::new(SharedHostStats::default());
        let mut handler = MediaPacketHandler::new(
            Statistics::new(),
            active_session_id,
            host_stats,
            audio_tx,
            decode_tx,
            Some(event_tx),
        );

        let mut host_timing = FrameTimingCheckpoints::new(100_000_000);
        host_timing.encode_queue_ts_us = 1200;
        host_timing.encode_done_ts_us = 3400;
        host_timing.packetize_ts_us = 3800;
        host_timing.send_ts_us = 4200;

        let pkt = video_packet(7);
        handler
            .handle_with_host_timing(pkt, 1024, host_timing)
            .await;

        let (received_pkt, received_timing) = decode_rx.try_recv().expect("decode packet received");
        assert_eq!(received_pkt.header.ssrc, 7);
        assert_eq!(received_timing.capture_ts_us, 100_000_000);
        assert_eq!(received_timing.encode_queue_ts_us, 1200);
        assert_eq!(received_timing.encode_done_ts_us, 3400);
        assert_eq!(received_timing.packetize_ts_us, 3800);
        assert_eq!(received_timing.send_ts_us, 4200);
        assert!(received_timing.recv_ts_us >= 4200);
        assert_eq!(
            received_timing.jitter_enter_ts_us,
            received_timing.recv_ts_us
        );
    }

    #[test]
    fn shared_host_stats_snapshot_is_lock_free_for_numerics() {
        let stats = SharedHostStats::default();
        stats.apply_host_telemetry(120.0, 3.5, 0.4, 8000);
        stats.set_rtt(2.5);
        let snap = stats.snapshot();
        assert_eq!(snap.fps, 120.0);
        assert_eq!(snap.latency, 3.5);
        assert_eq!(snap.bitrate_kbps, 8000);
        assert_eq!(snap.rtt_ms, 2.5);
    }

    #[tokio::test]
    async fn audio_backpressure_does_not_block_media_ingress() {
        let (event_tx, _) = mpsc::unbounded_channel();
        let mut handler = test_media_handler(Arc::new(AtomicU32::new(7)), event_tx);
        let (audio_tx, _audio_rx) = mpsc::channel(1);
        handler.audio_tx = audio_tx;
        let mut packet = video_packet(remote_microphone_audio_stream_id(7));
        packet.header.payload_type = PayloadType::AudioOpus as u8;
        handler.handle(packet.clone(), 16).await;
        tokio::time::timeout(Duration::from_millis(100), handler.handle(packet, 16))
            .await
            .expect("a full audio queue must not stall UDP ingress");
        assert_eq!(handler.stats_net.audio_ingress_dropped.load(Relaxed), 1);
    }

    #[tokio::test]
    async fn audio_configuration_is_deferred_without_blocking_or_reordering_its_packets() {
        let (event_tx, _) = mpsc::unbounded_channel();
        let mut handler = test_media_handler(Arc::new(AtomicU32::new(7)), event_tx);
        let (audio_tx, mut audio_rx) = mpsc::channel(1);
        handler.audio_tx = audio_tx;
        let id = remote_microphone_audio_stream_id(7);
        let mut packet = video_packet(id);
        packet.header.payload_type = PayloadType::AudioOpus as u8;
        handler.handle(packet.clone(), 16).await;
        let config = AudioStreamConfig::remote_microphone(id, 48_000, 1, 20);
        tokio::time::timeout(
            Duration::from_millis(100),
            handler.handle_audio_stream_config(config.clone()),
        )
        .await
        .expect("configuration must not block ingress");
        assert!(matches!(
            audio_rx.try_recv().unwrap(),
            AudioIngressEvent::Packet(_)
        ));
        handler.handle(packet.clone(), 16).await;
        assert_eq!(
            audio_rx.try_recv().unwrap(),
            AudioIngressEvent::StreamConfig(config)
        );
        handler.handle(packet.clone(), 16).await;
        assert_eq!(
            audio_rx.try_recv().unwrap(),
            AudioIngressEvent::Packet(packet)
        );
        assert!(handler.pending_audio_configs.is_empty());
    }

    #[tokio::test]
    async fn reordered_packets_and_sequence_zero_do_not_inflate_loss() {
        let (event_tx, _) = mpsc::unbounded_channel();
        let mut handler = test_media_handler(Arc::new(AtomicU32::new(7)), event_tx);
        for sequence_number in [u16::MAX, 1, 0, 2, 2] {
            let mut packet = video_packet(7);
            packet.header.sequence_number = sequence_number;
            handler.handle(packet, 16).await;
        }
        assert_eq!(handler.packets_expected, 4);
        assert_eq!(handler.packets_lost, 0);
        assert_eq!(handler.last_seq, Some(2));
    }

    #[tokio::test]
    async fn receiver_releases_a_gap_without_another_datagram() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let addr = mux.local_addr().unwrap();
        let (_, udp_receiver) = mux.split();
        let sender_mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = sender_mux.split();
        let (audio_tx, _audio_rx) = mpsc::channel(1);
        let (decode_tx, mut decode_rx) = mpsc::channel(4);
        let task = spawn_client_session_receiver(ClientSessionReceiverConfig {
            bind_addr: addr,
            udp_receiver,
            stats: Statistics::new(),
            active_session_id: Arc::new(AtomicU32::new(7)),
            host_stats: Arc::new(SharedHostStats::default()),
            audio_tx,
            decode_tx,
            clipboard_control: None,
            file_transfer_control: None,
            session_event_tx: None,
        });
        for sequence_number in [10, 12] {
            let mut packet = video_packet(7);
            packet.header.sequence_number = sequence_number;
            sender.send_rtp(&packet, addr).await.unwrap();
        }
        for expected in [10, 12] {
            let packet = tokio::time::timeout(Duration::from_millis(250), decode_rx.recv()).await;
            if packet.is_err() {
                task.abort();
            }
            assert_eq!(packet.unwrap().unwrap().0.header.sequence_number, expected);
        }
        task.abort();
        let _ = task.await;
    }
}
