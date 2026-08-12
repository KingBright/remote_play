use crate::data_plane::checksum_crc32;
use crate::file_transfer_runtime::{FileTransferCommand, FileTransferEvent, FileTransferGroupFile};
use crate::traits::{ClipboardFileReference, ClipboardFileReferenceProvider};
use std::error::Error;
use std::fmt;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

#[derive(Debug, Clone)]
pub struct ClipboardFileSyncConfig {
    pub poll_interval: Duration,
    pub max_files_per_poll: usize,
}

impl Default for ClipboardFileSyncConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            max_files_per_poll: 16,
        }
    }
}

#[derive(Debug)]
pub enum ClipboardFileSyncError {
    Provider(Box<dyn Error + Send + Sync>),
    CommandClosed,
}

impl fmt::Display for ClipboardFileSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClipboardFileSyncError::Provider(err) => {
                write!(f, "clipboard file reference provider failed: {err}")
            }
            ClipboardFileSyncError::CommandClosed => {
                write!(f, "file transfer command channel is closed")
            }
        }
    }
}

impl Error for ClipboardFileSyncError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ClipboardFileSyncError::Provider(err) => Some(err.as_ref()),
            ClipboardFileSyncError::CommandClosed => None,
        }
    }
}

pub async fn run_clipboard_file_sync<P>(
    mut provider: P,
    file_command_tx: mpsc::Sender<FileTransferCommand>,
    mut file_event_rx: mpsc::UnboundedReceiver<FileTransferEvent>,
    forward_event_tx: Option<mpsc::UnboundedSender<FileTransferEvent>>,
    mut cancel_rx: broadcast::Receiver<()>,
    config: ClipboardFileSyncConfig,
) -> Result<(), ClipboardFileSyncError>
where
    P: ClipboardFileReferenceProvider + Send + 'static,
{
    let mut poll_interval = tokio::time::interval(config.poll_interval);
    let mut last_sent_refs_crc32 = None;
    let mut last_applied_refs_crc32 = None;

    poll_interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel_rx.recv() => {
                return Ok(());
            }
            _ = poll_interval.tick() => {
                let references = provider
                    .read_clipboard_file_references()
                    .await
                    .map_err(ClipboardFileSyncError::Provider)?;
                let references = bounded_references(references, config.max_files_per_poll);
                if references.is_empty() {
                    continue;
                }

                let refs_crc32 = file_references_crc32(&references);
                if Some(refs_crc32) == last_sent_refs_crc32
                    || Some(refs_crc32) == last_applied_refs_crc32
                {
                    continue;
                }

                let files = references
                    .iter()
                    .map(|reference| FileTransferGroupFile {
                        path: reference.path.clone(),
                        mime_type: None,
                    })
                    .collect();
                file_command_tx
                    .send(FileTransferCommand::SendFileGroup { files })
                    .await
                    .map_err(|_| ClipboardFileSyncError::CommandClosed)?;
                last_sent_refs_crc32 = Some(refs_crc32);
            }
            maybe_event = file_event_rx.recv() => {
                let Some(event) = maybe_event else {
                    return Ok(());
                };

                if let Some(tx) = &forward_event_tx {
                    let _ = tx.send(event.clone());
                }

                match event {
                    FileTransferEvent::IncomingCompleted {
                        path, group: None, ..
                    } => {
                        let references = vec![ClipboardFileReference::new(path)];
                        provider
                            .write_clipboard_file_references(&references)
                            .await
                            .map_err(ClipboardFileSyncError::Provider)?;
                        last_applied_refs_crc32 = Some(file_references_crc32(&references));
                    }
                    FileTransferEvent::IncomingGroupCompleted { paths, .. } => {
                        let references = paths
                            .into_iter()
                            .map(ClipboardFileReference::new)
                            .collect::<Vec<_>>();
                        provider
                            .write_clipboard_file_references(&references)
                            .await
                            .map_err(ClipboardFileSyncError::Provider)?;
                        last_applied_refs_crc32 = Some(file_references_crc32(&references));
                    }
                    _ => {}
                }
            }
        }
    }
}

fn bounded_references(
    mut references: Vec<ClipboardFileReference>,
    max_files_per_poll: usize,
) -> Vec<ClipboardFileReference> {
    if max_files_per_poll == 0 {
        references.clear();
    } else {
        references.truncate(max_files_per_poll);
    }
    references
}

fn file_references_crc32(references: &[ClipboardFileReference]) -> u32 {
    let mut bytes = Vec::new();
    for reference in references {
        bytes.extend_from_slice(reference.path.to_string_lossy().as_bytes());
        bytes.push(0);
    }
    checksum_crc32(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard_provider::MemoryClipboardFileReferenceProvider;
    use std::path::PathBuf;
    use tokio::time::timeout;

    #[tokio::test]
    async fn clipboard_file_sync_turns_local_file_references_into_send_commands() {
        let provider = MemoryClipboardFileReferenceProvider::with_references(vec![
            ClipboardFileReference::new("/tmp/a.txt"),
            ClipboardFileReference::new("/tmp/b.txt"),
        ]);
        let (command_tx, mut command_rx) = mpsc::channel(8);
        let (_event_tx, event_rx) = mpsc::unbounded_channel();
        let (cancel_tx, cancel_rx) = broadcast::channel(1);

        let task = tokio::spawn(run_clipboard_file_sync(
            provider,
            command_tx,
            event_rx,
            None,
            cancel_rx,
            ClipboardFileSyncConfig {
                poll_interval: Duration::from_millis(1),
                max_files_per_poll: 1,
            },
        ));

        let command = timeout(Duration::from_secs(1), command_rx.recv())
            .await
            .expect("file command should arrive")
            .expect("file command channel should remain open");
        assert_eq!(
            command,
            FileTransferCommand::SendFileGroup {
                files: vec![FileTransferGroupFile {
                    path: PathBuf::from("/tmp/a.txt"),
                    mime_type: None
                }]
            }
        );
        assert!(
            timeout(Duration::from_millis(20), command_rx.recv())
                .await
                .is_err(),
            "same clipboard file reference should not be resent"
        );

        let _ = cancel_tx.send(());
        task.await
            .expect("sync task should join")
            .expect("sync task should stop cleanly");
    }

    #[tokio::test]
    async fn clipboard_file_sync_writes_received_file_references_and_forwards_events() {
        let provider = MemoryClipboardFileReferenceProvider::new();
        let (command_tx, _command_rx) = mpsc::channel(8);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (forward_tx, mut forward_rx) = mpsc::unbounded_channel();
        let (cancel_tx, cancel_rx) = broadcast::channel(1);

        let task = tokio::spawn(run_clipboard_file_sync(
            provider,
            command_tx,
            event_rx,
            Some(forward_tx),
            cancel_rx,
            ClipboardFileSyncConfig {
                poll_interval: Duration::from_secs(60),
                ..ClipboardFileSyncConfig::default()
            },
        ));

        let event = FileTransferEvent::IncomingCompleted {
            transfer_id: 1,
            file_object_id: 2,
            group: None,
            path: PathBuf::from("/tmp/received.txt"),
            size_bytes: 5,
        };
        event_tx
            .send(event.clone())
            .expect("incoming event should send");

        assert_eq!(
            timeout(Duration::from_secs(1), forward_rx.recv())
                .await
                .expect("forwarded event should arrive"),
            Some(event)
        );

        let _ = cancel_tx.send(());
        task.await
            .expect("sync task should join")
            .expect("sync task should stop cleanly");
    }

    #[tokio::test]
    async fn clipboard_file_sync_publishes_group_paths_atomically() {
        let provider = MemoryClipboardFileReferenceProvider::new();
        let (command_tx, mut command_rx) = mpsc::channel(8);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (cancel_tx, cancel_rx) = broadcast::channel(1);

        let task = tokio::spawn(run_clipboard_file_sync(
            provider,
            command_tx,
            event_rx,
            None,
            cancel_rx,
            ClipboardFileSyncConfig {
                poll_interval: Duration::from_millis(1),
                ..ClipboardFileSyncConfig::default()
            },
        ));

        event_tx
            .send(FileTransferEvent::IncomingGroupCompleted {
                group_id: 9,
                paths: vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")],
                total_size_bytes: 10,
            })
            .expect("group event should send");

        assert!(
            timeout(Duration::from_millis(50), command_rx.recv())
                .await
                .is_err(),
            "publishing a received group should not echo it back as a new send"
        );

        let _ = cancel_tx.send(());
        task.await
            .expect("sync task should join")
            .expect("sync task should stop cleanly");
    }
}
