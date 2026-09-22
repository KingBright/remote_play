//! Bounded selective-repeat delivery for file objects. ACKs describe receiver work,
//! never just successful insertion into the local network queue.
use crate::scheduled_sender::ScheduledDataSender;
use protocol::{DataEnvelope, FileTransferControl};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

const WINDOW: usize = 32;
const RETRY: Duration = Duration::from_millis(500);
const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone, Default)]
pub(crate) struct FileDeliveryTracker {
    routes: Arc<Mutex<HashMap<u64, mpsc::Sender<FileTransferControl>>>>,
}

impl FileDeliveryTracker {
    pub fn route(&self, control: FileTransferControl) {
        let object_id = match &control {
            FileTransferControl::ChunkAccepted { object_id, .. }
            | FileTransferControl::Rejected { object_id, .. } => *object_id,
            _ => return,
        };
        if let Some(tx) = self.routes.lock().unwrap().get(&object_id) {
            // Duplicate ACK floods cannot allocate memory or block incoming files.
            let _ = tx.try_send(control);
        }
    }

    pub fn window(&self, objects: [u64; 2]) -> DeliveryWindow {
        let (tx, rx) = mpsc::channel(WINDOW * 4);
        let mut routes = self.routes.lock().unwrap();
        for object in objects {
            routes.insert(object, tx.clone());
        }
        DeliveryWindow {
            tracker: self.clone(),
            objects,
            rx,
            pending: HashMap::new(),
        }
    }
}

struct Pending {
    envelope: DataEnvelope,
    first_sent: Instant,
    last_sent: Instant,
}

pub(crate) struct DeliveryWindow {
    tracker: FileDeliveryTracker,
    objects: [u64; 2],
    rx: mpsc::Receiver<FileTransferControl>,
    pending: HashMap<(u64, u32), Pending>,
}

impl Drop for DeliveryWindow {
    fn drop(&mut self) {
        let mut routes = self.tracker.routes.lock().unwrap();
        for object in self.objects {
            routes.remove(&object);
        }
    }
}

impl DeliveryWindow {
    /// False means a user cancellation; the caller sends the cancellation control.
    pub async fn send(
        &mut self,
        envelope: DataEnvelope,
        sender: &ScheduledDataSender,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        if !self.drain_to(WINDOW - 1, sender, &cancelled).await? {
            return Ok(false);
        }
        let chunk = envelope
            .header
            .chunk
            .as_ref()
            .ok_or("missing file chunk identity")?;
        let key = (chunk.object_id, chunk.chunk_index);
        let now = Instant::now();
        self.pending.insert(
            key,
            Pending {
                envelope: envelope.clone(),
                first_sent: now,
                last_sent: now,
            },
        );
        Self::enqueue(sender, envelope).await?;
        Ok(true)
    }

    pub async fn flush(
        &mut self,
        sender: &ScheduledDataSender,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        self.drain_to(0, sender, cancelled).await
    }

    async fn enqueue(sender: &ScheduledDataSender, envelope: DataEnvelope) -> Result<(), String> {
        tokio::time::timeout(DEADLINE, sender.send(envelope))
            .await
            .map_err(|_| "file sender queue timed out".to_owned())?
            .map_err(|error| error.to_string())
    }

    async fn drain_to(
        &mut self,
        limit: usize,
        sender: &ScheduledDataSender,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        while self.pending.len() > limit {
            if cancelled() {
                return Ok(false);
            }
            tokio::select! {
                reply = self.rx.recv() => match reply {
                    Some(FileTransferControl::ChunkAccepted { object_id, chunk_index }) => {
                        self.pending.remove(&(object_id, chunk_index));
                    }
                    Some(FileTransferControl::Rejected { reason, .. }) => return Err(format!("receiver rejected file: {reason}")),
                    None => return Err("file acknowledgement channel closed".into()),
                    _ => {}
                },
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
            let now = Instant::now();
            for pending in self.pending.values_mut() {
                if now.duration_since(pending.first_sent) >= DEADLINE {
                    return Err(
                        "receiver confirmation timed out; delivery was not confirmed".into(),
                    );
                }
                if now.duration_since(pending.last_sent) >= RETRY {
                    Self::enqueue(sender, pending.envelope.clone()).await?;
                    pending.last_sent = Instant::now();
                }
            }
        }
        Ok(!cancelled())
    }
}
