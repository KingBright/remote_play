//! Directed delivery owns a control-only WorkspaceConnection. The target uses
//! the normal pull connection after proving the one-time sending intention.
use protocol::session::{DeliveryClaimProof, DeliveryState, SessionCommand};
use remote_core::workspace_session::{WorkspaceConnection, WorkspaceEvents};
use std::time::Duration;
type Error = Box<dyn std::error::Error + Send + Sync>;

/// Retry the exact command/revision; newer revisions never substitute for a
/// lost first offer. Only this connection's matching receipt completes it.
async fn exchange(
    conn: &WorkspaceConnection,
    events: &mut WorkspaceEvents,
    command: SessionCommand,
    request: u64,
    revision: u64,
) -> Result<DeliveryState, Error> {
    for _ in 0..4 {
        conn.control(command.clone()).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(750);
        loop {
            match tokio::time::timeout_at(deadline, events.control.recv()).await {
                Ok(Some(SessionCommand::DeliveryReceipt {
                    request_id,
                    revision: got,
                    state,
                    reason,
                })) if request_id == request
                    && (got == revision
                        || matches!(state, DeliveryState::Cancelled | DeliveryState::Rejected)) =>
                {
                    if state == DeliveryState::Rejected {
                        return Err(format!("Target rejected delivery: {reason}").into());
                    }
                    return Ok(state);
                }
                Ok(Some(SessionCommand::Closed { .. })) | Ok(None) => {
                    return Err("Control connection closed".into());
                }
                Ok(Some(_)) => continue,
                Err(_) => break,
            }
        }
    }
    Err("Target did not acknowledge directed delivery; it may need an update".into())
}
pub(crate) async fn claim(
    conn: &WorkspaceConnection,
    events: &mut WorkspaceEvents,
    proof: &DeliveryClaimProof,
) -> Result<(), Error> {
    let state = exchange(
        conn,
        events,
        SessionCommand::DeliveryClaim {
            request_id: proof.request_id,
            target_device_id: proof.target_device_id.clone(),
            ticket: proof.ticket,
        },
        proof.request_id,
        0,
    )
    .await?;
    if state != DeliveryState::Claimed {
        return Err("Source did not accept this sending intention".into());
    }
    Ok(())
}

#[cfg(any(unix, windows))]
mod sender {
    use super::*;
    use host::directed_delivery::LocalIntent;
    use std::{
        net::SocketAddr,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::Instant,
    };
    static NEXT_REGISTRATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    pub(crate) struct SendHandle {
        pub target: String,
        stop: tokio::sync::watch::Sender<bool>,
        heartbeat: Arc<Mutex<Instant>>,
        status: Arc<Mutex<String>>,
        task: tokio::task::JoinHandle<()>,
    }
    impl SendHandle {
        pub fn start(
            profile: PathBuf,
            generation: String,
            source: String,
            target: String,
            routes: Vec<(String, SocketAddr)>,
        ) -> Self {
            let (stop, mut cancelled) = tokio::sync::watch::channel(false);
            let heartbeat = Arc::new(Mutex::new(Instant::now()));
            let beat = heartbeat.clone();
            let status = Arc::new(Mutex::new("Contacting target".into()));
            let progress = status.clone();
            let peer = target.clone();
            let task = tokio::spawn(async move {
                let result = run(
                    profile,
                    generation,
                    source,
                    peer,
                    routes,
                    &mut cancelled,
                    beat,
                    progress.clone(),
                )
                .await;
                *progress.lock().unwrap() = match result {
                    Ok(()) => "Sending stopped".into(),
                    Err(error) => format!("Delivery stopped: {error}"),
                };
            });
            Self {
                target,
                stop,
                heartbeat,
                status,
                task,
            }
        }
        pub fn poll(&self) -> String {
            *self.heartbeat.lock().unwrap() = Instant::now();
            self.status.lock().unwrap().clone()
        }
        pub fn is_finished(&self) -> bool {
            self.task.is_finished()
        }
    }
    impl Drop for SendHandle {
        fn drop(&mut self) {
            self.stop.send_replace(true);
        }
    }
    async fn local(
        client: &crate::background_service::Client,
        intent: LocalIntent,
    ) -> Result<(), Error> {
        let client = client.clone();
        tokio::task::spawn_blocking(move || client.delivery(intent)).await?
    }
    async fn run(
        profile: PathBuf,
        generation: String,
        source: String,
        target: String,
        routes: Vec<(String, SocketAddr)>,
        cancelled: &mut tokio::sync::watch::Receiver<bool>,
        heartbeat: Arc<Mutex<Instant>>,
        status: Arc<Mutex<String>>,
    ) -> Result<(), Error> {
        let sequence = NEXT_REGISTRATION
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |n| n.checked_add(1),
            )
            .map_err(|_| "Sending sequence exhausted")?;
        let client = tokio::task::spawn_blocking(move || {
            crate::background_service::Client::connect(&profile)
        })
        .await??;
        let mut connected = None;
        for (_, endpoint) in routes.into_iter().take(3) {
            let result = tokio::select! {biased;_=cancelled.changed()=>return Ok(()),result=tokio::time::timeout(Duration::from_secs(5),WorkspaceConnection::connect_media(endpoint))=>result};
            if let Ok(Ok(connection)) = result {
                connected = Some(connection);
                break;
            }
        }
        let (conn, mut events) = connected.ok_or("No authenticated route to target")?;
        let mut ticket = [0; 32];
        ticket[..16].copy_from_slice(&remote_core::session_crypto::random_bytes_16());
        ticket[16..].copy_from_slice(&remote_core::session_crypto::random_bytes_16());
        let request = u64::from_le_bytes(ticket[..8].try_into().unwrap()).max(1);
        local(
            &client,
            LocalIntent::Register {
                ticket,
                target: target.clone(),
                generation: generation.clone(),
                connection: conn.id,
                sequence,
            },
        )
        .await?;
        let scope = remote_core::shared_files::current_share_scope();
        let cancellation = cancelled.clone();
        let sending = async {
            let mut revision = 1;
            loop {
                if *cancellation.borrow() {
                    return Ok::<(), Error>(());
                }
                if heartbeat.lock().unwrap().elapsed() > Duration::from_secs(10)
                    || remote_core::shared_files::current_share_scope() != scope
                {
                    return Err("Sending window or device group changed".into());
                }
                if revision > 1 {
                    local(
                        &client,
                        LocalIntent::Renew {
                            ticket,
                            generation: generation.clone(),
                            revision,
                        },
                    )
                    .await?;
                }
                let command = SessionCommand::DeliveryOffer {
                    request_id: request,
                    source_device_id: source.clone(),
                    target_device_id: target.clone(),
                    ticket,
                    revision,
                };
                let state = exchange(&conn, &mut events, command, request, revision).await?;
                *status.lock().unwrap() = match state {
                    DeliveryState::Pending => "Opening receiver…".into(),
                    DeliveryState::Accepted => "Connecting…".into(),
                    DeliveryState::Receiving => "Receiving".into(),
                    DeliveryState::Cancelled | DeliveryState::Rejected => return Ok(()),
                    _ => return Err("Unexpected delivery receipt".into()),
                };
                tokio::time::sleep(Duration::from_secs(5)).await;
                revision = revision
                    .checked_add(1)
                    .ok_or("Sending revision exhausted")?;
            }
        };
        // Drop signals are independent of GUI polling. Revocation happens
        // locally first; a lost target Cancel cannot leave our capture running.
        let result = tokio::select! {biased;_=cancelled.changed()=>Ok(()),result=sending=>result};
        let revoked = local(&client, LocalIntent::Revoke { ticket, generation }).await;
        let _ = conn
            .control(SessionCommand::DeliveryCancel {
                request_id: request,
            })
            .await;
        conn.close().await;
        revoked?;
        result
    }
}
#[cfg(any(unix, windows))]
pub(crate) use sender::SendHandle;
