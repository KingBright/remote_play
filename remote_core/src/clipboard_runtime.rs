use crate::clipboard_sync::{ClipboardSyncConfig, ClipboardSyncEndpoint, ClipboardSyncError};
use crate::scheduled_sender::{ScheduledDataSendError, ScheduledDataSender};
use crate::traits::ClipboardProvider;
use protocol::{ContentKind, DataEnvelope};
use std::error::Error;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc};

#[derive(Debug, Clone, Copy)]
pub struct ClipboardSyncRunnerConfig {
    pub endpoint: ClipboardSyncConfig,
    pub poll_interval: Duration,
}

impl Default for ClipboardSyncRunnerConfig {
    fn default() -> Self {
        Self {
            endpoint: ClipboardSyncConfig::default(),
            poll_interval: Duration::from_millis(500),
        }
    }
}

#[derive(Debug)]
pub enum ClipboardSyncRunnerError {
    Endpoint(ClipboardSyncError),
    Send(ScheduledDataSendError),
}

impl fmt::Display for ClipboardSyncRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClipboardSyncRunnerError::Endpoint(err) => {
                write!(f, "clipboard sync endpoint failed: {err}")
            }
            ClipboardSyncRunnerError::Send(err) => write!(f, "clipboard sync send failed: {err}"),
        }
    }
}

impl Error for ClipboardSyncRunnerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ClipboardSyncRunnerError::Endpoint(err) => Some(err),
            ClipboardSyncRunnerError::Send(err) => Some(err),
        }
    }
}

impl From<ClipboardSyncError> for ClipboardSyncRunnerError {
    fn from(value: ClipboardSyncError) -> Self {
        Self::Endpoint(value)
    }
}

impl From<ScheduledDataSendError> for ClipboardSyncRunnerError {
    fn from(value: ScheduledDataSendError) -> Self {
        Self::Send(value)
    }
}

pub async fn run_clipboard_sync<P>(
    provider: P,
    data_sender: ScheduledDataSender,
    mut inbound_rx: mpsc::Receiver<DataEnvelope>,
    mut cancel_rx: broadcast::Receiver<()>,
    config: ClipboardSyncRunnerConfig,
) -> Result<(), ClipboardSyncRunnerError>
where
    P: ClipboardProvider + Send + 'static,
{
    let mut endpoint = ClipboardSyncEndpoint::new(provider, config.endpoint);
    let mut poll_interval = tokio::time::interval(config.poll_interval);
    let delivery = crate::file_delivery::FileDeliveryTracker::default();
    let mut outgoing = tokio::task::JoinSet::new();

    // Tokio intervals tick immediately; skip that so startup does not instantly resend stale state.
    poll_interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel_rx.recv() => {
                return Ok(());
            }
            result = outgoing.join_next(), if !outgoing.is_empty() => {
                if !matches!(result, Some(Ok(Ok(())))) { endpoint.retry_outgoing(); }
            }
            _ = poll_interval.tick(), if outgoing.is_empty() => {
                if let Some(transfer) = endpoint.poll_outgoing(now_ms()).await? {
                    let sender = data_sender.clone();
                    let tracker = delivery.clone();
                    outgoing.spawn(async move {
                        let object = transfer.envelopes.first().and_then(|e| e.header.chunk.as_ref()).ok_or("missing clipboard identity")?.object_id;
                        let mut window = tracker.window([object, u64::MAX]);
                        for envelope in transfer.envelopes { window.send(envelope, &sender, || false).await?; }
                        window.flush(&sender, || false).await?;
                        Ok::<(), String>(())
                    });
                }
            }
            maybe_envelope = inbound_rx.recv() => {
                let Some(mut envelope) = maybe_envelope else {
                    return Ok(());
                };

                if envelope.header.kind == ContentKind::ClipboardControl {
                    envelope.header.kind = ContentKind::FileControl;
                    if let Ok(control) = crate::file_transfer::control_from_envelope(&envelope) { delivery.route(control); }
                } else if envelope.header.kind == ContentKind::ClipboardBundle {
                    let identity = envelope.header.chunk.as_ref().map(|c| (c.object_id, c.chunk_index));
                    let result = endpoint.receive_envelope(envelope).await;
                    if let Some((object_id, chunk_index)) = identity {
                        let control = match result {
                            Ok(_) => protocol::FileTransferControl::ChunkAccepted { object_id, chunk_index },
                            Err(error) => protocol::FileTransferControl::Rejected { object_id, reason:error.to_string() },
                        };
                        if let Ok(mut ack) = crate::file_transfer::control_to_envelope(&control, 0, config.endpoint.stream_id, 0, now_ms()) {
                            ack.header.kind = ContentKind::ClipboardControl;
                            data_sender.send(ack).await?;
                        }
                    }
                }
            }
        }
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
    use crate::clipboard_provider::MemoryClipboardProvider;
    use crate::net::{MultiplexedPacket, UdpMultiplexer};
    use crate::scheduled_sender::ScheduledDataSenderConfig;
    use protocol::{ClipboardBundle, ClipboardItem, ClipboardText};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering::Relaxed},
    };

    struct RecordingClipboard(Arc<Mutex<Vec<ClipboardBundle>>>);
    #[async_trait::async_trait]
    impl crate::ClipboardProvider for RecordingClipboard {
        fn capabilities(&self) -> crate::ClipboardBackendCapabilities {
            crate::ClipboardBackendCapabilities {
                text: true,
                image: true,
                ..Default::default()
            }
        }
        async fn read_clipboard(
            &mut self,
            _: crate::clipboard_plane::ClipboardSyncPolicy,
        ) -> Result<Option<ClipboardBundle>, Box<dyn Error + Send + Sync>> {
            Ok(None)
        }
        async fn write_clipboard(
            &mut self,
            bundle: &ClipboardBundle,
            _: crate::clipboard_plane::ClipboardSyncPolicy,
        ) -> Result<(), Box<dyn Error + Send + Sync>> {
            self.0.lock().unwrap().push(bundle.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn clipboard_retransmits_lost_data_and_final_ack_without_reapplying() {
        let source = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let sink = UdpMultiplexer::bind("127.0.0.1:0").await.unwrap();
        let source_addr = source.local_addr().unwrap();
        let sink_addr = sink.local_addr().unwrap();
        let (source_tx, source_rx) = source.split();
        let (sink_tx, sink_rx) = sink.split();
        let (source_sender, source_worker) =
            ScheduledDataSender::spawn(source_tx, sink_addr, Default::default());
        let (sink_sender, sink_worker) =
            ScheduledDataSender::spawn(sink_tx, source_addr, Default::default());
        let (source_in, source_in_rx) = mpsc::channel(128);
        let (sink_in, sink_in_rx) = mpsc::channel(128);
        let final_index = Arc::new(AtomicU32::new(u32::MAX));
        let dropped_data = Arc::new(AtomicBool::new(false));
        let dropped_ack = Arc::new(AtomicBool::new(false));
        let index = final_index.clone();
        let dropped = dropped_data.clone();
        let to_sink = tokio::spawn(async move {
            while let Ok(MultiplexedPacket::Data(envelope, _)) = sink_rx.recv().await {
                if envelope.header.kind == ContentKind::ClipboardBundle {
                    let chunk = envelope.header.chunk.as_ref().unwrap();
                    index.store(chunk.total_chunks - 1, Relaxed);
                    if chunk.chunk_index == 0 && !dropped.swap(true, Relaxed) {
                        continue;
                    }
                }
                if sink_in.send(envelope).await.is_err() {
                    break;
                }
            }
        });
        let index = final_index;
        let dropped = dropped_ack.clone();
        let to_source = tokio::spawn(async move {
            while let Ok(MultiplexedPacket::Data(envelope, _)) = source_rx.recv().await {
                if envelope.header.kind == ContentKind::ClipboardControl {
                    let mut control = envelope.clone();
                    control.header.kind = ContentKind::FileControl;
                    if let Ok(protocol::FileTransferControl::ChunkAccepted { chunk_index, .. }) =
                        crate::file_transfer::control_from_envelope(&control)
                        && chunk_index == index.load(Relaxed)
                        && !dropped.swap(true, Relaxed)
                    {
                        continue;
                    }
                }
                if source_in.send(envelope).await.is_err() {
                    break;
                }
            }
        });
        let writes = Arc::new(Mutex::new(Vec::new()));
        let bundle = ClipboardBundle::text(42, "剪贴板丢包恢复".repeat(5000));
        let (cancel, rx) = broadcast::channel(2);
        let config = ClipboardSyncRunnerConfig {
            poll_interval: Duration::from_millis(10),
            ..Default::default()
        };
        let sending = tokio::spawn(run_clipboard_sync(
            MemoryClipboardProvider::with_bundle(bundle.clone()),
            source_sender,
            source_in_rx,
            rx,
            config,
        ));
        let receiving = tokio::spawn(run_clipboard_sync(
            RecordingClipboard(writes.clone()),
            sink_sender,
            sink_in_rx,
            cancel.subscribe(),
            config,
        ));
        timeout(Duration::from_secs(6), async {
            while writes.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(dropped_data.load(Relaxed) && dropped_ack.load(Relaxed));
        assert_eq!(*writes.lock().unwrap(), vec![bundle]);
        let _ = cancel.send(());
        sending.await.unwrap().unwrap();
        receiving.await.unwrap().unwrap();
        to_sink.abort();
        to_source.abort();
        source_worker.abort();
        sink_worker.abort();
    }
    use tokio::time::timeout;

    fn text_bundle() -> ClipboardBundle {
        ClipboardBundle::new(
            1,
            vec![ClipboardItem::Text(ClipboardText {
                text: "runner clipboard".to_string(),
            })],
        )
    }

    #[tokio::test]
    async fn runner_sends_polled_clipboard_bundle_through_scheduled_sender() {
        let target = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("target bind should succeed");
        let target_addr = target.local_addr().expect("target addr should exist");
        let target_rx = target.split().1;

        let source = UdpMultiplexer::bind("127.0.0.1:0")
            .await
            .expect("source bind should succeed");
        let source_tx = source.split().0;
        let (scheduled_sender, _worker) = ScheduledDataSender::spawn(
            source_tx,
            target_addr,
            ScheduledDataSenderConfig {
                ..ScheduledDataSenderConfig::default()
            },
        );

        let (_inbound_tx, inbound_rx) = mpsc::channel(8);
        let (cancel_tx, cancel_rx) = broadcast::channel(1);
        let provider = MemoryClipboardProvider::with_bundle(text_bundle());

        let runner = tokio::spawn(run_clipboard_sync(
            provider,
            scheduled_sender,
            inbound_rx,
            cancel_rx,
            ClipboardSyncRunnerConfig {
                poll_interval: Duration::from_millis(1),
                ..ClipboardSyncRunnerConfig::default()
            },
        ));

        let received = timeout(Duration::from_secs(1), target_rx.recv())
            .await
            .expect("clipboard packet should arrive")
            .expect("clipboard packet should decode");

        let MultiplexedPacket::Data(envelope, _) = received else {
            panic!("expected data-plane clipboard packet");
        };
        assert_eq!(envelope.header.kind, ContentKind::ClipboardBundle);

        let _ = cancel_tx.send(());
        runner
            .await
            .expect("runner task should join")
            .expect("runner should stop cleanly");
    }
}
