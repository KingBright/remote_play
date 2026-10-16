use crate::clock_sync::{ClockContinuity, ClockEstimate, ClockSyncState, ClockSynchronizer};
use crate::jitter_buffer::JitterBuffer;
use crate::media_plane::{audio_stream_config_from_envelope, realtime_data_to_rtp};
use crate::net::{MultiplexedPacket, UdpReceiver, UdpSender};
use crate::stats::Statistics;
use protocol::{
    AudioStreamConfig, ContentKind, FrameTimingCheckpoints, PayloadType, PipelineTelemetryReport,
    RtpPacket, remote_microphone_audio_stream_id, remote_system_audio_stream_id,
};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub trait EnvelopeIngress: Send + Sync {
    fn route_inbound(&self, envelope: protocol::DataEnvelope);
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioIngressEvent {
    /// Local payload boundary for this connection's playback consumers.
    PayloadActivity {
        stream_id: u32,
        active: bool,
    },
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
    CaptureSources {
        request_id: u32,
        sources: Vec<protocol::session::CaptureSourceInfo>,
    },
    SubscriptionAccepted {
        id: u32,
        supports_input: bool,
    },
    CaptureSourceSwitched {
        id: u32,
        request_id: u32,
        source: protocol::session::CaptureSource,
        supports_input: bool,
    },
    HostError {
        request_id: u32,
        reason: String,
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
    latest_host_timing: Option<FrameTimingCheckpoints>,
    clock_estimate: Option<ClockEstimate>,
    e2e_is_valid: bool,
}

// Cached estimates are refreshed by the receiver/workspace actor. This is a
// conservative consumer limit, matching the default measurement freshness window.
const MAX_CLOCK_SAMPLE_AGE_US: u64 = 10_000_000;

fn clock_estimate_is_fresh(estimate: &ClockEstimate, now_us: u64) -> bool {
    estimate.state == ClockSyncState::Synchronized
        && now_us >= estimate.client_time_us
        && estimate
            .sample_age_us
            .saturating_add(now_us - estimate.client_time_us)
            <= MAX_CLOCK_SAMPLE_AGE_US
        && estimate.offset_us.is_finite()
        && estimate.drift_ppm.is_finite()
}

fn fresh_clock_offset_at(estimate: Option<&ClockEstimate>, now_us: u64) -> Option<i64> {
    let estimate = estimate.filter(|estimate| clock_estimate_is_fresh(estimate, now_us))?;
    let offset = estimate.offset_us
        + (now_us - estimate.client_time_us) as f64 * estimate.drift_ppm / 1_000_000.0;
    if offset.is_finite() && offset >= i64::MIN as f64 && offset < i64::MAX as f64 {
        Some(offset.round() as i64)
    } else {
        None
    }
}

fn mapped_receive_offset(
    host_timing: &FrameTimingCheckpoints,
    offset_us: i64,
    now_us: u64,
) -> Option<u32> {
    let elapsed = now_us as i128 + offset_us as i128 - host_timing.capture_ts_us as i128;
    // Zero is the existing "checkpoint missing" sentinel, so do not let a zero
    // receive stage trigger JitterBuffer's unmapped fallback. Never fabricate 1us.
    (host_timing.capture_ts_us > 0
        && elapsed > 0
        && elapsed >= host_timing.send_ts_us as i128
        && elapsed < u32::MAX as i128)
        .then_some(elapsed as u32)
}

fn clock_probe_period(paused: bool) -> Duration {
    Duration::from_secs(if paused { 5 } else { 1 })
}

enum OutboundClockControl {
    Probe(protocol::session_auth::ClockProbe),
    Reply {
        probe: protocol::session_auth::ClockProbe,
        host_recv_us: u64,
    },
}

struct ClockControlSender {
    tx: mpsc::Sender<(OutboundClockControl, SocketAddr)>,
    task: tokio::task::JoinHandle<()>,
}

impl ClockControlSender {
    fn try_send(
        &self,
        message: (OutboundClockControl, SocketAddr),
    ) -> Result<(), mpsc::error::TrySendError<(OutboundClockControl, SocketAddr)>> {
        self.tx.try_send(message)
    }
}

impl Drop for ClockControlSender {
    fn drop(&mut self) {
        // The receiver owns this guard. Return, cancellation and panic all abort
        // the worker instead of detaching a task that can outlive the session.
        self.task.abort();
    }
}

/// A full/blocked clock-control queue must never hold the media receive pump.
/// Queue/scheduling delay belongs to raw RTT and is rejected by low-RTT fitting.
fn spawn_clock_control_sender(sender: UdpSender) -> ClockControlSender {
    let (tx, mut rx) = mpsc::channel::<(OutboundClockControl, SocketAddr)>(8);
    let task = tokio::spawn(async move {
        while let Some((request, addr)) = rx.recv().await {
            let session_id = match &request {
                OutboundClockControl::Probe(probe) | OutboundClockControl::Reply { probe, .. } => {
                    probe.session_id
                }
            };
            if sender.peer_session_id(addr) != Some(session_id) {
                continue;
            }
            let message = match request {
                OutboundClockControl::Probe(probe) => protocol::ControlMessage::ClockProbe(probe),
                OutboundClockControl::Reply {
                    probe,
                    host_recv_us,
                } => protocol::ControlMessage::ClockReply(protocol::session_auth::ClockReply {
                    probe,
                    host_recv_us,
                    host_send_us: crate::timing::quanta_now_us(),
                }),
            };
            // Failure only drops this measurement. It does not touch connection
            // liveness, peer authentication, or audio/video recovery state.
            // An enqueued probe that fails here remains pending only until its
            // normal bounded measurement timeout; it is never reused.
            let _ = tokio::time::timeout(
                Duration::from_millis(250),
                sender.send_control(&message, addr),
            )
            .await;
        }
    });
    ClockControlSender { tx, task }
}

/// Lock-free numeric telemetry with a mutex only for the occasional report/string fields.
pub struct SharedHostStats {
    pub media_pause: crate::media_pause::MediaPauseControl,
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
    extra: Mutex<HostStatsExtra>,
}

impl Default for SharedHostStats {
    fn default() -> Self {
        Self {
            media_pause: Default::default(),
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
            extra: Mutex::new(HostStatsExtra::default()),
        }
    }
}

impl SharedHostStats {
    pub fn snapshot(&self) -> HostStats {
        let extra = self.extra.lock().unwrap_or_else(|err| err.into_inner());
        let e2e_is_valid = extra.e2e_is_valid
            && extra.clock_estimate.as_ref().is_some_and(|estimate| {
                clock_estimate_is_fresh(estimate, crate::timing::quanta_now_us())
            });
        HostStats {
            fps: f32::from_bits(self.fps.load(Relaxed)),
            latency: f32::from_bits(self.latency.load(Relaxed)),
            jitter: f32::from_bits(self.jitter.load(Relaxed)),
            bitrate_kbps: self.bitrate_kbps.load(Relaxed),
            rtt_ms: f32::from_bits(self.rtt_ms.load(Relaxed)),
            e2e_latency_ms: if e2e_is_valid {
                f32::from_bits(self.e2e_latency_ms.load(Relaxed))
            } else {
                0.0
            },
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

    /// Most recent four-timestamp estimate, including raw RTT, host processing,
    /// low-RTT fit diagnostics and separate asymmetry/holdover allowances.
    /// Values remain evaluated at client_time_us; freshness is rechecked on read.
    pub fn clock_estimate(&self) -> Option<ClockEstimate> {
        let mut estimate = self
            .extra
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clock_estimate?;
        let now = crate::timing::quanta_now_us();
        if now < estimate.client_time_us
            || estimate
                .sample_age_us
                .saturating_add(now.saturating_sub(estimate.client_time_us))
                > MAX_CLOCK_SAMPLE_AGE_US
        {
            estimate.state = ClockSyncState::Stale;
        }
        Some(estimate)
    }

    pub fn set_clock_estimate(&self, estimate: Option<ClockEstimate>) {
        if let Ok(mut extra) = self.extra.lock() {
            extra.clock_estimate = estimate;
            if !estimate
                .as_ref()
                .is_some_and(|value| clock_estimate_is_fresh(value, crate::timing::quanta_now_us()))
            {
                extra.e2e_is_valid = false;
                self.e2e_latency_ms.store(0, Relaxed);
            }
        }
    }

    /// Cross-machine latency is always an estimate, and unavailable without a
    /// fresh synchronized mapping. Zero in the legacy snapshot means unavailable.
    pub fn estimated_e2e_latency_ms(&self) -> Option<f32> {
        let extra = self.extra.lock().unwrap_or_else(|err| err.into_inner());
        (extra.e2e_is_valid
            && extra.clock_estimate.as_ref().is_some_and(|estimate| {
                clock_estimate_is_fresh(estimate, crate::timing::quanta_now_us())
            }))
        .then(|| f32::from_bits(self.e2e_latency_ms.load(Relaxed)))
    }

    /// Original host checkpoints for diagnostics, even when no cross-clock
    /// mapping exists and the decoder receives timing marked unavailable.
    pub fn latest_host_timing(&self) -> Option<FrameTimingCheckpoints> {
        self.extra
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .latest_host_timing
    }

    fn set_latest_host_timing(&self, timing: FrameTimingCheckpoints) {
        if let Ok(mut extra) = self.extra.lock() {
            extra.latest_host_timing = Some(timing);
        }
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
        if let Ok(mut extra) = self.extra.lock() {
            *extra = HostStatsExtra::default();
        }
    }

    fn update_video_link(
        &self,
        e2e_latency_ms: Option<f32>,
        rtt_ms: f32,
        packet_loss_rate: f32,
        jitter_buffer_depth: usize,
    ) {
        self.e2e_latency_ms
            .store(e2e_latency_ms.unwrap_or(0.0).to_bits(), Relaxed);
        self.rtt_ms.store(rtt_ms.to_bits(), Relaxed);
        self.packet_loss_rate
            .store(packet_loss_rate.to_bits(), Relaxed);
        self.jitter_buffer_depth
            .store(jitter_buffer_depth as u32, Relaxed);
        if let Ok(mut extra) = self.extra.lock() {
            extra.e2e_is_valid = e2e_latency_ms.is_some();
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
    pub decode_tx: mpsc::Sender<VideoPacket>,
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
        let recovery_sender = udp_receiver.sender();
        let clock_sender = spawn_clock_control_sender(recovery_sender.clone());
        let mut media_handler = MediaPacketHandler::new(
            stats,
            active_session_id,
            host_stats.clone(),
            audio_tx,
            decode_tx,
            session_event_tx.clone(),
        );
        let mut clock_sync = ClockSynchronizer::new();
        let mut clock_continuity = ClockContinuity::new();
        let mut clock_peer: Option<(SocketAddr, [u8; 16])> = None;
        let mut clock_session_id: Option<[u8; 16]> = None;
        let mut next_probe_id = Some(1_u64);
        let mut clock_tick = tokio::time::interval(Duration::from_secs(1));
        clock_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            if let Some((message, target)) = host_stats.media_pause.take_request() {
                let _ = recovery_sender.send_control(&message, target).await;
            }
            if !host_stats.media_pause.is_paused()
                && let Some((target, session_id)) = media_handler.take_keyframe_request()
            {
                let _ = recovery_sender
                    .send_control(
                        &protocol::ControlMessage::RequestKeyframe { session_id },
                        target,
                    )
                    .await;
            }
            let wait = [
                media_handler.video_jitter_buffer.next_ready_in(),
                if host_stats.media_pause.is_paused() {
                    None
                } else {
                    media_handler.recovery_wait()
                },
                host_stats.media_pause.retry_wait(),
            ]
            .into_iter()
            .flatten()
            .min();
            let received = tokio::select! {
                _ = host_stats.media_pause.changed.notified() => {
                    clock_tick.reset_at(tokio::time::Instant::now());
                    continue;
                },
                packet = udp_receiver.recv() => packet,
                _ = clock_tick.tick() => {
                    clock_tick.reset_after(clock_probe_period(host_stats.media_pause.is_paused()));
                    if !clock_continuity.observe(crate::timing::quanta_now_us(), std::time::SystemTime::now()) {
                        clock_sync.invalidate_measurements();
                        media_handler.update_clock_estimate(None);
                    }
                    let peer = media_handler.video_source.and_then(|addr| {
                        recovery_sender.peer_session_id(addr).map(|session_id| (addr, session_id))
                    });
                    if peer != clock_peer {
                        let next_session_id = peer.map(|(_, session_id)| session_id);
                        if next_session_id.is_some() && next_session_id != clock_session_id {
                            clock_sync.reset();
                            next_probe_id = Some(1);
                            clock_session_id = next_session_id;
                        } else {
                            // Route loss/change within an authenticated session
                            // cannot permit probe ID reuse, even across None.
                            clock_sync.invalidate_measurements();
                        }
                        clock_peer = peer;
                        media_handler.update_clock_estimate(None);
                    }
                    if let (Some((addr, session_id)), Some(probe_id)) = (clock_peer, next_probe_id) {
                        let client_send_us = crate::timing::quanta_now_us();
                        if clock_sync.begin_probe(probe_id, client_send_us).is_ok() {
                            next_probe_id = probe_id.checked_add(1);
                            let probe = protocol::session_auth::ClockProbe { session_id, probe_id, client_send_us };
                            if clock_sender.try_send((OutboundClockControl::Probe(probe), addr)).is_err() {
                                clock_sync.cancel_probe(probe_id);
                            }
                        }
                    }
                    media_handler.update_clock_estimate(clock_sync.estimate(crate::timing::quanta_now_us()));
                    continue;
                }
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
                Ok(MultiplexedPacket::ClockProbe(probe, host_recv_us, addr)) => {
                    if recovery_sender.peer_session_id(addr) == Some(probe.session_id) {
                        let _ = clock_sender.try_send((
                            OutboundClockControl::Reply {
                                probe,
                                host_recv_us,
                            },
                            addr,
                        ));
                    }
                }
                Ok(MultiplexedPacket::ClockReply(reply, local_recv_us, addr)) => {
                    if clock_peer == Some((addr, reply.probe.session_id))
                        && recovery_sender.peer_session_id(addr) == Some(reply.probe.session_id)
                        && clock_sync
                            .complete_probe(
                                reply.probe.probe_id,
                                reply.probe.client_send_us,
                                reply.host_recv_us,
                                reply.host_send_us,
                                local_recv_us,
                            )
                            .is_ok()
                    {
                        media_handler.update_clock_estimate(clock_sync.estimate(local_recv_us));
                    }
                }
                Ok(MultiplexedPacket::Rtp(packet, addr)) => {
                    media_handler.remember_video_source(&packet, addr);
                    let packet_size = packet.payload.len() as u64 + 12;
                    media_handler.handle(packet, packet_size).await;
                }
                Ok(MultiplexedPacket::Control(msg, addr)) => match msg {
                    protocol::ControlMessage::MediaPauseState {
                        session_id,
                        revision,
                        paused,
                    } => {
                        host_stats
                            .media_pause
                            .acknowledge(addr, session_id, revision, paused);
                    }
                    // Legacy wall-clock Pong data never authorizes a mapping or RTT update.
                    protocol::ControlMessage::Pong { .. } => {}
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
                    protocol::ControlMessage::Session(command) => {
                        if let Some(tx) = &session_event_tx {
                            match *command {
                                protocol::session::SessionCommand::Sources {
                                    request_id,
                                    sources,
                                } => {
                                    let _ = tx.send(ClientSessionEvent::CaptureSources {
                                        request_id,
                                        sources,
                                    });
                                }
                                protocol::session::SessionCommand::Subscribed {
                                    id,
                                    supports_input,
                                    ..
                                } => {
                                    let _ = tx.send(ClientSessionEvent::SubscriptionAccepted {
                                        id,
                                        supports_input,
                                    });
                                }
                                protocol::session::SessionCommand::SourceSwitched {
                                    id,
                                    request_id,
                                    source,
                                    supports_input,
                                } => {
                                    let _ = tx.send(ClientSessionEvent::CaptureSourceSwitched {
                                        id,
                                        request_id,
                                        source,
                                        supports_input,
                                    });
                                }
                                protocol::session::SessionCommand::Error { request_id, reason } => {
                                    let _ = tx
                                        .send(ClientSessionEvent::HostError { request_id, reason });
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                },
                Ok(MultiplexedPacket::Data(envelope, addr)) => {
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
                        Ok(packet) => {
                            media_handler.remember_video_source(&packet, addr);
                            media_handler.handle(packet, packet_size).await;
                        }
                        Err(e) => {
                            eprintln!("Ignoring unsupported data-plane media packet: {}", e);
                        }
                    }
                }
                Ok(MultiplexedPacket::DataWithTiming(envelope, timing, addr)) => {
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
                            media_handler.remember_video_source(&packet, addr);
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
        ContentKind::ClipboardBundle | ContentKind::ClipboardControl => {
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

/// Internal decoder ingress. The source epoch is fixed before enqueue, never
/// inferred by a decoder after it has consumed a delayed source acknowledgement.
pub type VideoPacket = (RtpPacket, FrameTimingCheckpoints, u64);

pub(crate) struct MediaPacketHandler {
    stats_net: Arc<Statistics>,
    receiver_session_id: Arc<AtomicU32>,
    host_stats: Arc<SharedHostStats>,
    audio_tx: mpsc::Sender<AudioIngressEvent>,
    decode_tx: mpsc::Sender<VideoPacket>,
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
    clock_estimate: Option<ClockEstimate>,
    rtt_ms: f32,
    smooth_e2e_ms: f32,
    video_source: Option<SocketAddr>,
    needs_keyframe: bool,
    last_keyframe_request: Option<Instant>,
    observed_decode_errors: usize,
}

impl MediaPacketHandler {
    pub(crate) fn new(
        stats_net: Arc<Statistics>,
        receiver_session_id: Arc<AtomicU32>,
        host_stats: Arc<SharedHostStats>,
        audio_tx: mpsc::Sender<AudioIngressEvent>,
        decode_tx: mpsc::Sender<VideoPacket>,
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
            clock_estimate: None,
            rtt_ms: 0.0,
            smooth_e2e_ms: 0.0,
            video_source: None,
            needs_keyframe: false,
            last_keyframe_request: None,
            observed_decode_errors: 0,
        }
    }

    pub(crate) fn remember_video_source(&mut self, packet: &RtpPacket, addr: SocketAddr) {
        let session = self.receiver_session_id.load(Relaxed);
        if packet.header.payload_type == PayloadType::VideoH265 as u8
            && session != 0
            && packet.header.ssrc == session
        {
            self.video_source = Some(addr);
        }
    }

    /// Only the authenticated workspace actor calls this after accepting the
    /// matching subscription/resume. Recovery must not depend on a first packet.
    pub(crate) fn activate_video_source(&mut self, target: SocketAddr) {
        self.reset_video_source();
        self.video_source = Some(target);
        self.last_video_ssrc = self.receiver_session_id.load(Relaxed);
    }

    pub(crate) fn suspend_video_source(&mut self) {
        self.reset_video_source();
        self.video_source = None;
        self.last_video_ssrc = 0;
        self.needs_keyframe = false;
    }

    /// A matched source-switch acknowledgement starts a new local video epoch.
    /// Keep audio configuration and route ownership; discard only old video
    /// reorder state. V2 has no wire source revision, so this cannot identify a
    /// delayed old datagram arriving after the acknowledgement.
    pub(crate) fn reset_video_source(&mut self) {
        self.video_jitter_buffer = JitterBuffer::new(0);
        self.video_expected_seq_init = false;
        self.last_seq = None;
        self.packets_expected = 0;
        self.packets_lost = 0;
        self.loss_rate_pct = 0.0;
        self.smooth_e2e_ms = 0.0;
        self.needs_keyframe = true;
        self.last_keyframe_request = None;
        self.stats_net.video_recovery.reset_source(|| ());
    }

    fn recovery_wait(&self) -> Option<Duration> {
        ((self.needs_keyframe
            || (self.stats_net.video_recovery.tracks_output()
                && self.stats_net.video_recovery.is_waiting()))
            && self.video_source.is_some())
        .then(|| {
            Duration::from_millis(250).saturating_sub(
                self.last_keyframe_request
                    .map_or(Duration::from_millis(250), |t| t.elapsed()),
            )
        })
    }

    pub(crate) fn take_keyframe_request(&mut self) -> Option<(SocketAddr, u32)> {
        let errors = self.stats_net.video_decode_errors.load(Relaxed);
        if errors != self.observed_decode_errors {
            self.observed_decode_errors = errors;
            if !self.stats_net.video_recovery.tracks_output() {
                self.needs_keyframe = true;
            }
        }
        let session = self.receiver_session_id.load(Relaxed);
        if session == 0 || session != self.last_video_ssrc {
            self.needs_keyframe = false;
            self.video_source = None;
            self.last_keyframe_request = None;
            return None;
        }
        if self.recovery_wait()? != Duration::ZERO {
            return None;
        }
        self.last_keyframe_request = Some(Instant::now());
        self.stats_net.video_keyframe_requests.fetch_add(1, Relaxed);
        Some((self.video_source?, self.last_video_ssrc))
    }

    pub fn update_clock_estimate(&mut self, estimate: Option<ClockEstimate>) {
        self.clock_estimate = estimate;
        self.rtt_ms = estimate.map_or(0.0, |value| {
            value.latest_sample.network_rtt_us as f32 / 1_000.0
        });
        self.host_stats.set_rtt(self.rtt_ms);
        self.host_stats.set_clock_estimate(estimate);
        if fresh_clock_offset_at(self.clock_estimate.as_ref(), crate::timing::quanta_now_us())
            .is_none()
        {
            self.smooth_e2e_ms = 0.0;
        }
    }

    pub async fn handle(&mut self, packet: RtpPacket, packet_size: u64) {
        // The compact RTP timestamp can only be expanded with a valid mapping.
        // Without one, do not fabricate host time from the local wall-clock epoch.
        let now_us = crate::timing::quanta_now_us();
        let capture_ts_us = fresh_clock_offset_at(self.clock_estimate.as_ref(), now_us)
            .and_then(|offset| u64::try_from(now_us as i128 + offset as i128).ok())
            .map(|host_now_us| {
                expand_legacy_timestamp(packet.header.timestamp, host_now_us / 1_000)
                    .saturating_mul(1_000)
            })
            .unwrap_or(0);
        self.handle_with_host_timing(
            packet,
            packet_size,
            FrameTimingCheckpoints::new(capture_ts_us),
        )
        .await;
    }

    pub async fn handle_with_host_timing(
        &mut self,
        packet: RtpPacket,
        packet_size: u64,
        host_timing: FrameTimingCheckpoints,
    ) {
        let current_session = self.receiver_session_id.load(Relaxed);
        let now_us = crate::timing::quanta_now_us();
        if packet.header.payload_type == PayloadType::VideoH265 as u8
            && (current_session == 0 || packet.header.ssrc == current_session)
        {
            self.host_stats.set_latest_host_timing(host_timing);
        }
        let recv_offset = fresh_clock_offset_at(self.clock_estimate.as_ref(), now_us)
            .and_then(|offset| mapped_receive_offset(&host_timing, offset, now_us));
        let mut timing = if let Some(recv_offset) = recv_offset {
            let mut timing = host_timing;
            timing.recv_ts_us = recv_offset;
            timing.jitter_enter_ts_us = recv_offset;
            timing
        } else {
            // capture=0 is the existing unavailable-timing marker. Jitter queue
            // residency and actual decoder costs still use local monotonic time.
            FrameTimingCheckpoints::new(0)
        };
        // Receive is the first client checkpoint; never trust peer client stages.
        timing.jitter_exit_ts_us = 0;
        timing.decode_enter_ts_us = 0;
        timing.decode_done_ts_us = 0;
        timing.render_submit_ts_us = 0;
        timing.render_done_ts_us = 0;
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
        if self.host_stats.media_pause.is_paused() {
            return;
        }
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
            self.needs_keyframe = false;
            self.last_keyframe_request = None;
        }

        if !self.video_expected_seq_init {
            self.needs_keyframe = !crate::media_plane::is_hevc_keyframe(&packet.payload);
            self.video_jitter_buffer = JitterBuffer::new(packet.header.sequence_number);
            self.video_expected_seq_init = true;
        }
        self.video_jitter_buffer.push_with_timing(packet, timing);
        self.stats_net
            .video_jitter_buffer_push
            .fetch_add(1, Relaxed);
        self.drain_video();
    }

    pub(crate) fn drain_video(&mut self) {
        while let Some((ordered_pkt, mut timing)) = self.video_jitter_buffer.pop_with_timing() {
            let seq = ordered_pkt.header.sequence_number;
            let advance = self
                .last_seq
                .map_or(1, |last| seq.wrapping_sub(last) as u64);
            self.packets_expected += advance;
            self.packets_lost += advance.saturating_sub(1);
            if advance > 1 {
                self.needs_keyframe = true;
            }
            if crate::media_plane::is_hevc_keyframe(&ordered_pkt.payload) {
                self.needs_keyframe = false;
            }
            self.last_seq = Some(seq);
            if self.last_loss_calc.elapsed() >= Duration::from_secs(1) {
                let rate = self.packets_lost as f32 / self.packets_expected.max(1) as f32 * 100.0;
                self.loss_rate_pct = self.loss_rate_pct * 0.7 + rate * 0.3;
                self.packets_expected = 0;
                self.packets_lost = 0;
                self.last_loss_calc = Instant::now();
            }

            let mapping_fresh =
                fresh_clock_offset_at(self.clock_estimate.as_ref(), crate::timing::quanta_now_us())
                    .is_some();
            let estimated_e2e =
                if mapping_fresh && timing.capture_ts_us > 0 && timing.recv_ts_us < u32::MAX {
                    // Capture -> receive is a cross-machine estimate, not a measured
                    // presentation latency. Never infer validity from RTT > 0.
                    let sample_e2e = timing.recv_ts_us as f32 / 1_000.0;
                    if self.smooth_e2e_ms <= 0.01 {
                        self.smooth_e2e_ms = sample_e2e;
                    } else {
                        self.smooth_e2e_ms = self.smooth_e2e_ms * 0.9 + sample_e2e * 0.1;
                    }
                    Some(self.smooth_e2e_ms)
                } else {
                    self.smooth_e2e_ms = 0.0;
                    // A fit may expire while a packet waits in the jitter queue.
                    timing = FrameTimingCheckpoints::new(0);
                    None
                };
            self.host_stats.update_video_link(
                estimated_e2e,
                self.rtt_ms,
                self.loss_rate_pct,
                self.video_jitter_buffer.len(),
            );

            self.stats_net.video_jitter_buffer_pop.fetch_add(1, Relaxed);
            let source_epoch = self.stats_net.video_recovery.snapshot().epoch.source;
            if self
                .decode_tx
                .try_send((ordered_pkt, timing, source_epoch))
                .is_err()
            {
                self.needs_keyframe = true;
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

    fn synchronized_estimate(offset_us: i64) -> ClockEstimate {
        let now = crate::timing::quanta_now_us();
        let mut sync = ClockSynchronizer::new();
        for id in 0..6 {
            let t1 = now - 6_000_000 + id * 1_000_000;
            let t2 = (t1 as i128 + offset_us as i128 + 1_000) as u64;
            sync.begin_probe(id, t1).unwrap();
            sync.complete_probe(id, t1, t2, t2 + 100, t1 + 2_100)
                .unwrap();
        }
        sync.estimate(now).unwrap()
    }

    fn test_media_handler(
        active_session_id: Arc<AtomicU32>,
        session_event_tx: mpsc::UnboundedSender<ClientSessionEvent>,
    ) -> MediaPacketHandler {
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (decode_tx, _decode_rx) = mpsc::channel::<VideoPacket>(4);
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
        let (decode_tx, _decode_rx) = mpsc::channel::<VideoPacket>(4);
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
                &ControlMessage::Pong {
                    client_send_ts: 1,
                    host_recv_ts: 9_999_999,
                    host_send_ts: 9_999_999,
                },
                client_addr,
            )
            .await
            .unwrap();
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
        assert!(host_stats.clock_estimate().is_none());
        assert!(host_stats.estimated_e2e_latency_ms().is_none());
        assert_eq!(host_stats.snapshot().rtt_ms, 0.0);
        receiver.abort();
    }

    #[tokio::test]
    async fn media_handler_preserves_host_timing_checkpoints() {
        let active_session_id = Arc::new(AtomicU32::new(7));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (decode_tx, mut decode_rx) = mpsc::channel::<VideoPacket>(4);
        let host_stats = Arc::new(SharedHostStats::default());
        let mut handler = MediaPacketHandler::new(
            Statistics::new(),
            active_session_id,
            host_stats.clone(),
            audio_tx,
            decode_tx,
            Some(event_tx),
        );

        handler.update_clock_estimate(Some(synchronized_estimate(5_000_000)));
        let capture_us = crate::timing::quanta_now_us() + 5_000_000 - 10_000;
        let mut host_timing = FrameTimingCheckpoints::new(capture_us);
        host_timing.encode_queue_ts_us = 1200;
        host_timing.encode_done_ts_us = 3400;
        host_timing.packetize_ts_us = 3800;
        host_timing.send_ts_us = 4200;

        let pkt = video_packet(7);
        handler
            .handle_with_host_timing(pkt, 1024, host_timing)
            .await;

        let (received_pkt, received_timing, _) =
            decode_rx.try_recv().expect("decode packet received");
        assert_eq!(received_pkt.header.ssrc, 7);
        assert_eq!(received_timing.capture_ts_us, capture_us);
        assert_eq!(received_timing.encode_queue_ts_us, 1200);
        assert_eq!(received_timing.encode_done_ts_us, 3400);
        assert_eq!(received_timing.packetize_ts_us, 3800);
        assert_eq!(received_timing.send_ts_us, 4200);
        assert!(received_timing.recv_ts_us >= 10_000);
        assert!(host_stats.estimated_e2e_latency_ms().is_some());
        assert_eq!(host_stats.latest_host_timing(), Some(host_timing));
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
    async fn video_recovery_requests_are_throttled_and_stop_at_a_keyframe_or_session_end() {
        let session = Arc::new(AtomicU32::new(7));
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (decode_tx, _decode_rx) = mpsc::channel(8);
        let mut handler = MediaPacketHandler::new(
            Statistics::new(),
            session.clone(),
            Arc::new(SharedHostStats::default()),
            audio_tx,
            decode_tx,
            None,
        );
        let host = "127.0.0.1:49373".parse().unwrap();
        let mut packet = video_packet(7);
        packet.header.sequence_number = 5; // Initial IDR was lost.
        packet.payload = vec![0, 0, 1, 0x02, 1];
        handler.remember_video_source(&packet, host);
        handler.handle(packet.clone(), 16).await;
        assert_eq!(handler.take_keyframe_request(), Some((host, 7)));
        assert_eq!(handler.take_keyframe_request(), None);
        handler.last_keyframe_request = Some(Instant::now() - Duration::from_millis(251));
        assert_eq!(handler.take_keyframe_request(), Some((host, 7)));
        packet.header.sequence_number = 6;
        packet.payload = vec![0, 0, 1, 0x26, 1];
        handler.handle(packet.clone(), 16).await;
        assert_eq!(handler.recovery_wait(), None);
        // Receiving an IDR does not prove it decoded. A decoder failure must
        // rearm recovery even without a sequence gap.
        handler.stats_net.video_decode_errors.fetch_add(1, Relaxed);
        handler
            .stats_net
            .video_recovery
            .require_keyframe(handler.stats_net.video_recovery.snapshot().epoch.source);
        handler.last_keyframe_request = Some(Instant::now() - Duration::from_millis(251));
        assert_eq!(handler.take_keyframe_request(), Some((host, 7)));
        assert_eq!(handler.take_keyframe_request(), None);
        packet.header.sequence_number = 8;
        packet.payload = vec![0, 0, 1, 0x02, 1];
        handler.handle(packet, 16).await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        handler.drain_video();
        assert!(handler.needs_keyframe);
        session.store(0, Relaxed);
        assert_eq!(handler.take_keyframe_request(), None);
        assert_eq!(handler.recovery_wait(), None);
    }

    #[tokio::test]
    async fn decoder_reference_loss_rearms_recovery_without_a_codec_error() {
        let stats = Statistics::new();
        stats.video_recovery.track_output();
        let id = Arc::new(AtomicU32::new(7));
        let (audio_tx, _audio_rx) = mpsc::channel(2);
        let (decode_tx, mut decoded) = mpsc::channel(2);
        let mut h = MediaPacketHandler::new(
            stats.clone(),
            id.clone(),
            Arc::new(SharedHostStats::default()),
            audio_tx,
            decode_tx,
            None,
        );
        let host = "127.0.0.1:49373".parse().unwrap();
        let mut p = video_packet(7);
        p.header.sequence_number = 1;
        p.payload = vec![0, 0, 1, 0x26, 1];
        h.remember_video_source(&p, host);
        h.handle(p.clone(), 16).await;
        decoded.try_recv().unwrap();
        assert_eq!(h.take_keyframe_request(), None);
        // A full decoder queue can discard the IDR after ingress saw it.
        // Skipping dependent frames need not increment video_decode_errors.
        stats
            .video_recovery
            .require_keyframe(stats.video_recovery.snapshot().epoch.source);
        assert_eq!(stats.video_decode_errors.load(Relaxed), 0);
        assert_eq!(h.take_keyframe_request(), Some((host, 7)));
        assert_eq!(h.take_keyframe_request(), None); // Existing 250 ms rate limit.
        p.header.sequence_number = 2;
        h.handle(p, 16).await;
        decoded.try_recv().unwrap();
        h.last_keyframe_request = Some(Instant::now() - Duration::from_millis(251));
        assert_eq!(
            h.take_keyframe_request(),
            Some((host, 7)),
            "receiving IDR is not decoder recovery"
        );
        assert_eq!(stats.video_keyframe_requests.load(Relaxed), 2);
        id.store(0, Relaxed);
        h.last_keyframe_request = None;
        assert_eq!(
            h.take_keyframe_request(),
            None,
            "closed sessions cannot request recovery"
        );
    }

    #[tokio::test]
    async fn accepted_source_recovers_without_a_packet_and_stops_after_the_only_output() {
        let stats = Statistics::new();
        stats.video_recovery.track_output();
        let session = Arc::new(AtomicU32::new(7));
        let (audio, _) = mpsc::channel(1);
        let (video, mut received) = mpsc::channel(4);
        let mut handler = MediaPacketHandler::new(
            stats.clone(),
            session.clone(),
            Arc::new(SharedHostStats::default()),
            audio,
            video,
            None,
        );
        let host = "127.0.0.1:49373".parse().unwrap();
        assert_eq!(handler.take_keyframe_request(), None, "unconfirmed route");
        handler.activate_video_source(host);
        assert_eq!(stats.udp_packets_recv.load(Relaxed), 0);
        assert_eq!(handler.take_keyframe_request(), Some((host, 7)));
        assert_eq!(handler.take_keyframe_request(), None, "250ms throttle");
        handler.last_keyframe_request = Some(Instant::now() - Duration::from_millis(251));
        assert_eq!(handler.take_keyframe_request(), Some((host, 7)));

        let mut idr = video_packet(7);
        idr.payload = vec![0, 0, 1, 0x26, 1];
        handler.handle(idr, 16).await;
        let (_, _, source) = received.try_recv().unwrap();
        let epoch = stats.video_recovery.snapshot().epoch;
        assert_eq!(source, epoch.source);
        handler.last_keyframe_request = None;
        assert_eq!(
            handler.take_keyframe_request(),
            Some((host, 7)),
            "IDR is not output"
        );
        assert!(stats.video_recovery.publish(epoch, || ()).is_some());
        handler.last_keyframe_request = None;
        assert_eq!(
            handler.take_keyframe_request(),
            None,
            "no extra packet is needed to stop recovery"
        );

        handler.suspend_video_source();
        assert_eq!(handler.take_keyframe_request(), None);
        handler.activate_video_source(host);
        assert_eq!(
            handler.take_keyframe_request(),
            Some((host, 7)),
            "resume before first packet"
        );
        session.store(8, Relaxed);
        assert_eq!(
            handler.take_keyframe_request(),
            None,
            "replacement is not authorized by an old route"
        );
        let replacement = "127.0.0.1:49374".parse().unwrap();
        handler.activate_video_source(replacement);
        assert_eq!(handler.take_keyframe_request(), Some((replacement, 8)));
        session.store(0, Relaxed);
        assert_eq!(handler.take_keyframe_request(), None);
    }

    #[tokio::test]
    async fn receiver_releases_a_gap_without_another_datagram() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let addr = mux.local_addr().unwrap();
        let (_, udp_receiver) = mux.split();
        let sender_mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, sender_rx) = sender_mux.split();
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
        assert!(
            matches!(tokio::time::timeout(Duration::from_millis(250), sender_rx.recv()).await.unwrap().unwrap(),
            MultiplexedPacket::Control(protocol::ControlMessage::RequestKeyframe { session_id: 7 }, source) if source == addr)
        );
        task.abort();
        let _ = task.await;
    }

    #[test]
    fn shared_stats_retain_measurement_diagnostics_and_expire_cached_estimates() {
        let stats = SharedHostStats::default();
        let estimate = synchronized_estimate(5_000_000);
        stats.set_clock_estimate(Some(estimate));
        let reported = stats.clock_estimate().unwrap();
        assert_eq!(reported.latest_sample.raw_total_rtt_us, 2_100);
        assert_eq!(reported.latest_sample.host_processing_us, 100);
        assert_eq!(reported.latest_sample.network_rtt_us, 2_000);
        assert!(reported.asymmetry_bound_us > 0.0);
        assert!(reported.fit_ci95_us.is_some());
        stats.update_video_link(Some(12.0), 2.0, 0.0, 0);
        assert_eq!(stats.estimated_e2e_latency_ms(), Some(12.0));
        let mut expired = estimate;
        expired.sample_age_us = MAX_CLOCK_SAMPLE_AGE_US + 1;
        stats.set_clock_estimate(Some(expired));
        assert_eq!(stats.clock_estimate().unwrap().state, ClockSyncState::Stale);
        assert_eq!(stats.estimated_e2e_latency_ms(), None);
        assert_eq!(stats.snapshot().e2e_latency_ms, 0.0);
        stats.reset();
        assert!(stats.clock_estimate().is_none());
    }

    #[test]
    fn only_fresh_synchronized_offsets_are_used_and_drift_is_evaluated_locally() {
        let mut estimate = synchronized_estimate(5_000_000);
        estimate.sample_age_us = 0;
        estimate.drift_ppm = 100.0;
        assert_eq!(
            fresh_clock_offset_at(Some(&estimate), estimate.client_time_us + 1_000_000),
            Some(5_000_100)
        );
        assert_eq!(
            fresh_clock_offset_at(Some(&estimate), estimate.client_time_us - 1),
            None
        );
        estimate.state = ClockSyncState::Acquiring;
        assert_eq!(
            fresh_clock_offset_at(Some(&estimate), estimate.client_time_us),
            None
        );
        estimate.state = ClockSyncState::Stale;
        assert_eq!(
            fresh_clock_offset_at(Some(&estimate), estimate.client_time_us),
            None
        );
    }

    #[tokio::test]
    async fn unsynchronized_and_stale_media_keep_raw_host_timing_without_fake_e2e() {
        for estimate in [
            None,
            Some({
                let mut stale = synchronized_estimate(5_000_000);
                stale.state = ClockSyncState::Stale;
                stale
            }),
        ] {
            let (event_tx, _) = mpsc::unbounded_channel();
            let mut handler = test_media_handler(Arc::new(AtomicU32::new(7)), event_tx);
            let (decode_tx, mut decode_rx) = mpsc::channel(4);
            handler.decode_tx = decode_tx;
            handler.update_clock_estimate(estimate);
            let mut host_timing = FrameTimingCheckpoints::new(1_000_000);
            host_timing.send_ts_us = 4_000;
            handler
                .handle_with_host_timing(video_packet(7), 100, host_timing)
                .await;
            let (_, timing, _) = decode_rx.try_recv().unwrap();
            assert_eq!(timing.capture_ts_us, 0);
            assert_eq!(timing.recv_ts_us, 0);
            assert_eq!(handler.host_stats.latest_host_timing(), Some(host_timing));
            assert!(handler.host_stats.estimated_e2e_latency_ms().is_none());
            assert_eq!(handler.host_stats.snapshot().e2e_latency_ms, 0.0);
        }
    }

    #[tokio::test]
    async fn future_host_capture_is_unavailable_instead_of_clipped_to_send_stage() {
        let (event_tx, _) = mpsc::unbounded_channel();
        let mut handler = test_media_handler(Arc::new(AtomicU32::new(7)), event_tx);
        let (decode_tx, mut decode_rx) = mpsc::channel(4);
        handler.decode_tx = decode_tx;
        handler.update_clock_estimate(Some(synchronized_estimate(0)));
        let mut host_timing =
            FrameTimingCheckpoints::new(crate::timing::quanta_now_us() + 60_000_000);
        host_timing.send_ts_us = 4_000;
        handler
            .handle_with_host_timing(video_packet(7), 100, host_timing)
            .await;
        let (_, timing, _) = decode_rx.try_recv().unwrap();
        assert_eq!(timing.capture_ts_us, 0);
        assert!(handler.host_stats.estimated_e2e_latency_ms().is_none());
    }

    #[test]
    fn zero_receive_offset_does_not_trigger_unmapped_jitter_fallback() {
        let now = 1_700_000_000_000_000;
        let host_timing = FrameTimingCheckpoints::new(now + 5_000_000);
        assert_eq!(mapped_receive_offset(&host_timing, 5_000_000, now), None);
        assert_eq!(
            mapped_receive_offset(&host_timing, 5_000_000, now + 1),
            Some(1)
        );
        assert_eq!(
            mapped_receive_offset(&host_timing, 5_000_000, now - 1),
            None
        );
    }

    #[test]
    fn paused_clock_probes_remain_sparse() {
        assert_eq!(clock_probe_period(false), Duration::from_secs(1));
        assert_eq!(clock_probe_period(true), Duration::from_secs(5));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn full_clock_send_queue_drops_measurements_without_waiting() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let addr = mux.local_addr().unwrap();
        let (sender, _) = mux.split();
        let clock = spawn_clock_control_sender(sender);
        // No await after spawn: the worker cannot drain this current-thread
        // fixture until the nonblocking queue-full assertion has completed.
        for probe_id in 0..8 {
            assert!(
                clock
                    .try_send((
                        OutboundClockControl::Probe(protocol::session_auth::ClockProbe {
                            session_id: [0; 16],
                            probe_id,
                            client_send_us: 1,
                        }),
                        addr
                    ))
                    .is_ok()
            );
        }
        assert!(matches!(
            clock.try_send((
                OutboundClockControl::Probe(protocol::session_auth::ClockProbe {
                    session_id: [0; 16],
                    probe_id: 8,
                    client_send_us: 1,
                }),
                addr
            )),
            Err(mpsc::error::TrySendError::Full(_))
        ));
        drop(clock);
    }

    #[tokio::test]
    async fn clock_send_worker_is_aborted_with_its_receiver_owner() {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let (sender, _) = mux.split();
        let clock = spawn_clock_control_sender(sender);
        let abort = clock.task.abort_handle();
        tokio::task::yield_now().await;
        assert!(!abort.is_finished());
        drop(clock);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned clock task must terminate with its receiver");
    }
}
