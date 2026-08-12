use crate::data_plane::{LaneScheduler, LaneSchedulerConfig, SchedulerPushResult};
use crate::net::UdpSender;
use protocol::DataEnvelope;
use std::error::Error;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub type ScheduledDataSenderJoin = JoinHandle<Result<(), Box<dyn Error + Send + Sync>>>;

#[derive(Debug, Clone, Copy)]
pub struct ScheduledDataSenderConfig {
    pub scheduler: LaneSchedulerConfig,
    pub queue_capacity: usize,
    pub send_budget_per_tick: usize,
    pub tick_interval: Duration,
}

impl Default for ScheduledDataSenderConfig {
    fn default() -> Self {
        Self {
            scheduler: LaneSchedulerConfig::default(),
            queue_capacity: 512,
            send_budget_per_tick: 32,
            tick_interval: Duration::from_millis(1),
        }
    }
}

#[derive(Clone)]
pub struct ScheduledDataSender {
    tx: mpsc::Sender<DataEnvelope>,
    counters: Arc<ScheduledDataSenderCounters>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScheduledDataSenderStats {
    pub entrance_queued: usize,
    pub entrance_enqueued: u64,
    pub entrance_full: u64,
    pub entrance_closed: u64,
    pub scheduler_dropped_stale_realtime: u64,
    pub scheduler_dropped_realtime_capacity: u64,
    pub scheduler_rejected_reliable_capacity: u64,
    pub sent_realtime: u64,
    pub sent_reliable: u64,
    pub send_errors: u64,
}

#[derive(Default)]
struct ScheduledDataSenderCounters {
    entrance_enqueued: AtomicU64,
    entrance_full: AtomicU64,
    entrance_closed: AtomicU64,
    scheduler_dropped_stale_realtime: AtomicU64,
    scheduler_dropped_realtime_capacity: AtomicU64,
    scheduler_rejected_reliable_capacity: AtomicU64,
    sent_realtime: AtomicU64,
    sent_reliable: AtomicU64,
    send_errors: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledDataSendError {
    Full,
    Closed,
}

impl fmt::Display for ScheduledDataSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScheduledDataSendError::Full => write!(f, "scheduled data sender queue is full"),
            ScheduledDataSendError::Closed => write!(f, "scheduled data sender is closed"),
        }
    }
}

impl Error for ScheduledDataSendError {}

impl ScheduledDataSender {
    pub fn spawn(
        udp_sender: UdpSender,
        target: SocketAddr,
        config: ScheduledDataSenderConfig,
    ) -> (Self, ScheduledDataSenderJoin) {
        let (tx, rx) = mpsc::channel(config.queue_capacity);
        let counters = Arc::new(ScheduledDataSenderCounters::default());
        let worker = tokio::spawn(run_sender_worker(
            udp_sender,
            target,
            config,
            rx,
            counters.clone(),
        ));
        (Self { tx, counters }, worker)
    }

    pub async fn send(&self, envelope: DataEnvelope) -> Result<(), ScheduledDataSendError> {
        match self.tx.send(envelope).await {
            Ok(()) => {
                self.counters.entrance_enqueued.fetch_add(1, Relaxed);
                Ok(())
            }
            Err(_) => {
                self.counters.entrance_closed.fetch_add(1, Relaxed);
                Err(ScheduledDataSendError::Closed)
            }
        }
    }

    pub fn try_send(&self, envelope: DataEnvelope) -> Result<(), ScheduledDataSendError> {
        match self.tx.try_send(envelope) {
            Ok(()) => {
                self.counters.entrance_enqueued.fetch_add(1, Relaxed);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.counters.entrance_full.fetch_add(1, Relaxed);
                Err(ScheduledDataSendError::Full)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.counters.entrance_closed.fetch_add(1, Relaxed);
                Err(ScheduledDataSendError::Closed)
            }
        }
    }

    pub fn stats(&self) -> ScheduledDataSenderStats {
        ScheduledDataSenderStats {
            entrance_queued: self.tx.max_capacity() - self.tx.capacity(),
            entrance_enqueued: self.counters.entrance_enqueued.load(Relaxed),
            entrance_full: self.counters.entrance_full.load(Relaxed),
            entrance_closed: self.counters.entrance_closed.load(Relaxed),
            scheduler_dropped_stale_realtime: self
                .counters
                .scheduler_dropped_stale_realtime
                .load(Relaxed),
            scheduler_dropped_realtime_capacity: self
                .counters
                .scheduler_dropped_realtime_capacity
                .load(Relaxed),
            scheduler_rejected_reliable_capacity: self
                .counters
                .scheduler_rejected_reliable_capacity
                .load(Relaxed),
            sent_realtime: self.counters.sent_realtime.load(Relaxed),
            sent_reliable: self.counters.sent_reliable.load(Relaxed),
            send_errors: self.counters.send_errors.load(Relaxed),
        }
    }
}

async fn run_sender_worker(
    udp_sender: UdpSender,
    target: SocketAddr,
    config: ScheduledDataSenderConfig,
    mut rx: mpsc::Receiver<DataEnvelope>,
    counters: Arc<ScheduledDataSenderCounters>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut scheduler = LaneScheduler::with_config(config.scheduler);
    let mut interval = tokio::time::interval(config.tick_interval);

    // Consume tokio interval's immediate first tick so initial submissions can batch.
    interval.tick().await;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                drain_ready_envelopes(&mut rx, &mut scheduler, &counters);
                drain_scheduled_envelopes(&mut scheduler, &udp_sender, target, config.send_budget_per_tick, &counters).await?;
            }
            maybe_envelope = rx.recv() => {
                match maybe_envelope {
                    Some(envelope) => {
                        record_push_result(&counters, scheduler.push(envelope, now_ms()));
                    }
                    None => {
                        while !scheduler.is_empty() {
                            drain_scheduled_envelopes(&mut scheduler, &udp_sender, target, config.send_budget_per_tick, &counters).await?;
                            tokio::task::yield_now().await;
                        }
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn drain_ready_envelopes(
    rx: &mut mpsc::Receiver<DataEnvelope>,
    scheduler: &mut LaneScheduler,
    counters: &ScheduledDataSenderCounters,
) {
    while let Ok(envelope) = rx.try_recv() {
        record_push_result(counters, scheduler.push(envelope, now_ms()));
    }
}

fn record_push_result(counters: &ScheduledDataSenderCounters, result: SchedulerPushResult) {
    match result {
        SchedulerPushResult::Queued => {}
        SchedulerPushResult::DroppedStaleRealtime => {
            counters
                .scheduler_dropped_stale_realtime
                .fetch_add(1, Relaxed);
        }
        SchedulerPushResult::DroppedRealtimeForCapacity => {
            counters
                .scheduler_dropped_realtime_capacity
                .fetch_add(1, Relaxed);
        }
        SchedulerPushResult::RejectedReliableForCapacity => {
            counters
                .scheduler_rejected_reliable_capacity
                .fetch_add(1, Relaxed);
        }
    }
}

async fn drain_scheduled_envelopes(
    scheduler: &mut LaneScheduler,
    udp_sender: &UdpSender,
    target: SocketAddr,
    budget: usize,
    counters: &ScheduledDataSenderCounters,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    for _ in 0..budget {
        let Some(envelope) = scheduler.pop_next(now_ms()) else {
            break;
        };
        let is_realtime = envelope.header.lane.is_realtime();
        if let Err(err) = udp_sender.send_data(&envelope, target).await {
            counters.send_errors.fetch_add(1, Relaxed);
            return Err(err);
        }
        if is_realtime {
            counters.sent_realtime.fetch_add(1, Relaxed);
        } else {
            counters.sent_reliable.fetch_add(1, Relaxed);
        }
    }
    Ok(())
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
    use crate::data_plane::{
        ObjectChunkSpec, RealtimePacketSpec, make_realtime_envelope, object_chunk_envelope,
    };
    use crate::media_plane::{realtime_data_to_rtp, rtp_to_realtime_data};
    use crate::net::{MultiplexedPacket, UdpMultiplexer};
    use protocol::{PayloadType, RtpHeader, RtpPacket};
    use tokio::time::{sleep, timeout};

    fn realtime(sequence_number: u64) -> DataEnvelope {
        let now = now_ms();
        make_realtime_envelope(
            RealtimePacketSpec::video(1, sequence_number, now, now + 1_000),
            vec![sequence_number as u8],
        )
    }

    fn bulk(sequence_number: u64) -> DataEnvelope {
        object_chunk_envelope(
            ObjectChunkSpec {
                object_id: sequence_number,
                stream_id: 7,
                sequence_number,
                timestamp_ms: now_ms(),
                chunk_index: 0,
                total_chunks: 1,
                offset: 0,
                total_size: 1,
                checksum_crc32: None,
            },
            vec![sequence_number as u8],
        )
    }

    fn media_packet(payload_type: PayloadType, sequence_number: u16, ssrc: u32) -> RtpPacket {
        let timestamp = if payload_type == PayloadType::VideoH265 {
            now_ms().saturating_add(1_000) as u32
        } else {
            sequence_number as u32 * 16
        };

        RtpPacket {
            header: RtpHeader {
                version: 2,
                payload_type: payload_type as u8,
                sequence_number,
                timestamp,
                ssrc,
            },
            payload: vec![sequence_number as u8; 16],
        }
    }

    async fn recv_data(receiver: &crate::net::UdpReceiver) -> DataEnvelope {
        match timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("receive should not time out")
            .expect("receive should succeed")
        {
            MultiplexedPacket::Data(envelope, _) => envelope,
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    async fn wait_for_stats(
        scheduled: &ScheduledDataSender,
        predicate: impl Fn(ScheduledDataSenderStats) -> bool,
    ) -> ScheduledDataSenderStats {
        timeout(Duration::from_secs(2), async {
            loop {
                let stats = scheduled.stats();
                if predicate(stats) {
                    return stats;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("stats predicate should become true")
    }

    #[test]
    fn scheduled_sender_reports_entrance_backpressure() {
        let (tx, _rx) = mpsc::channel(1);
        let scheduled = ScheduledDataSender {
            tx,
            counters: Arc::new(ScheduledDataSenderCounters::default()),
        };

        scheduled
            .try_send(realtime(1))
            .expect("first packet should fit");
        assert_eq!(
            scheduled
                .try_send(realtime(2))
                .expect_err("second packet should hit full entrance"),
            ScheduledDataSendError::Full
        );

        let stats = scheduled.stats();
        assert_eq!(stats.entrance_queued, 1);
        assert_eq!(stats.entrance_enqueued, 1);
        assert_eq!(stats.entrance_full, 1);
        assert_eq!(stats.entrance_closed, 0);
    }

    #[tokio::test]
    async fn scheduled_sender_sends_realtime_before_queued_bulk() {
        let sender_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("sender should bind");
        let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("receiver should bind");
        let target = receiver_mux
            .local_addr()
            .expect("receiver should have local addr");
        let (udp_sender, _) = sender_mux.split();
        let (_, receiver) = receiver_mux.split();
        let (scheduled, worker) = ScheduledDataSender::spawn(
            udp_sender,
            target,
            ScheduledDataSenderConfig {
                scheduler: LaneSchedulerConfig {
                    max_realtime_queued: 8,
                    max_reliable_queued: 8,
                },
                queue_capacity: 8,
                send_budget_per_tick: 1,
                tick_interval: Duration::from_millis(20),
            },
        );

        scheduled.send(bulk(1)).await.expect("bulk should queue");
        scheduled
            .send(realtime(2))
            .await
            .expect("realtime should queue");

        let first = recv_data(&receiver).await;
        let second = recv_data(&receiver).await;

        assert!(first.header.lane.is_realtime());
        assert!(!second.header.lane.is_realtime());
        let stats = scheduled.stats();
        assert_eq!(stats.sent_realtime, 1);
        assert_eq!(stats.sent_reliable, 1);
        assert_eq!(stats.send_errors, 0);

        drop(scheduled);
        worker
            .await
            .expect("worker should join")
            .expect("worker should stop");
    }

    #[tokio::test]
    async fn scheduled_sender_loopback_prioritizes_media_over_bulk() {
        let sender_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("sender should bind");
        let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("receiver should bind");
        let target = receiver_mux
            .local_addr()
            .expect("receiver should have local addr");
        let (udp_sender, _) = sender_mux.split();
        let (_, receiver) = receiver_mux.split();
        let (scheduled, worker) = ScheduledDataSender::spawn(
            udp_sender,
            target,
            ScheduledDataSenderConfig {
                scheduler: LaneSchedulerConfig {
                    max_realtime_queued: 8,
                    max_reliable_queued: 8,
                },
                queue_capacity: 8,
                send_budget_per_tick: 1,
                tick_interval: Duration::from_millis(20),
            },
        );
        let video = media_packet(PayloadType::VideoH265, 2, 10);
        let audio = media_packet(PayloadType::AudioOpus, 3, 11);

        scheduled.send(bulk(1)).await.expect("bulk should queue");
        scheduled
            .send(rtp_to_realtime_data(&audio).expect("audio should adapt"))
            .await
            .expect("audio should queue");
        scheduled
            .send(rtp_to_realtime_data(&video).expect("video should adapt"))
            .await
            .expect("video should queue");

        let first = recv_data(&receiver).await;
        let second = recv_data(&receiver).await;
        let third = recv_data(&receiver).await;

        let first_media = realtime_data_to_rtp(first).expect("first should be media");
        let second_media = realtime_data_to_rtp(second).expect("second should be media");
        assert_eq!(first_media, video);
        assert_eq!(second_media, audio);
        assert!(!third.header.lane.is_realtime());
        let stats = scheduled.stats();
        assert_eq!(stats.sent_realtime, 2);
        assert_eq!(stats.sent_reliable, 1);
        assert_eq!(stats.send_errors, 0);

        drop(scheduled);
        worker
            .await
            .expect("worker should join")
            .expect("worker should stop");
    }

    #[tokio::test]
    async fn scheduled_sender_keeps_realtime_first_under_reliable_pressure() {
        let sender_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("sender should bind");
        let receiver_mux = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("receiver should bind");
        let target = receiver_mux
            .local_addr()
            .expect("receiver should have local addr");
        let (udp_sender, _) = sender_mux.split();
        let (_, receiver) = receiver_mux.split();
        let (scheduled, worker) = ScheduledDataSender::spawn(
            udp_sender,
            target,
            ScheduledDataSenderConfig {
                scheduler: LaneSchedulerConfig {
                    max_realtime_queued: 8,
                    max_reliable_queued: 4,
                },
                queue_capacity: 32,
                send_budget_per_tick: 1,
                tick_interval: Duration::from_millis(100),
            },
        );
        let video = media_packet(PayloadType::VideoH265, 50, 10);
        let audio = media_packet(PayloadType::AudioOpus, 51, 11);

        for sequence_number in 1..=12 {
            scheduled
                .send(bulk(sequence_number))
                .await
                .expect("bulk should enter scheduled sender");
        }
        scheduled
            .send(rtp_to_realtime_data(&audio).expect("audio should adapt"))
            .await
            .expect("audio should enter scheduled sender");
        scheduled
            .send(rtp_to_realtime_data(&video).expect("video should adapt"))
            .await
            .expect("video should enter scheduled sender");

        let first = recv_data(&receiver).await;
        let second = recv_data(&receiver).await;
        let first_media = realtime_data_to_rtp(first).expect("first should be media");
        let second_media = realtime_data_to_rtp(second).expect("second should be media");

        assert_eq!(first_media, video);
        assert_eq!(second_media, audio);

        let stats = wait_for_stats(&scheduled, |stats| {
            stats.sent_realtime == 2 && stats.scheduler_rejected_reliable_capacity >= 8
        })
        .await;
        assert_eq!(stats.scheduler_dropped_stale_realtime, 0);
        assert_eq!(stats.scheduler_dropped_realtime_capacity, 0);
        assert_eq!(stats.send_errors, 0);

        drop(scheduled);
        worker
            .await
            .expect("worker should join")
            .expect("worker should stop");
    }
}
