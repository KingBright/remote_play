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

    // Tokio intervals tick immediately; skip that so startup does not instantly resend stale state.
    poll_interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel_rx.recv() => {
                return Ok(());
            }
            _ = poll_interval.tick() => {
                if let Some(transfer) = endpoint.poll_outgoing(now_ms()).await? {
                    for envelope in transfer.envelopes {
                        data_sender.send(envelope).await?;
                    }
                }
            }
            maybe_envelope = inbound_rx.recv() => {
                let Some(envelope) = maybe_envelope else {
                    return Ok(());
                };

                if envelope.header.kind == ContentKind::ClipboardBundle {
                    let _ = endpoint.receive_envelope(envelope).await?;
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
