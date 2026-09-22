//! Paired, real-device V2 receive/file probe. No simulated media is accepted.
#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use protocol::session::{SessionCommand, SubscriptionRequest};
    use remote_core::file_transfer_runtime::{FileTransferCommand, FileTransferEvent};
    use remote_core::workspace_session::WorkspaceConnection;
    use remote_core::{SharedHostStats, Statistics, VideoFrame};
    use std::{
        sync::{Arc, atomic::Ordering::Relaxed},
        time::Duration,
    };
    let group = std::env::var_os("REMOTE_PLAY_TEST_GROUP_DIR")
        .ok_or("set REMOTE_PLAY_TEST_GROUP_DIR to a paired test group")?;
    let identity =
        remote_core::mesh::AppPrivateMeshConfigStore::new(std::path::PathBuf::from(group))
            .load_or_generate("Device acceptance client")?;
    remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
    let target = std::env::var("REMOTE_PLAY_TEST_TARGET")?.parse()?;
    let directory = tempfile::tempdir()?;
    let (connection, mut events) =
        WorkspaceConnection::connect(target, directory.path().into()).await?;
    println!(
        "AUTHENTICATED max_subscriptions={} files={}",
        connection.max_subscriptions, connection.files_available
    );
    if let Some(path) = std::env::var_os("REMOTE_PLAY_TEST_FILE") {
        connection
            .file_commands
            .send(FileTransferCommand::SendFile {
                path: path.into(),
                mime_type: None,
            })
            .await?;
        tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                match events.files.recv().await.ok_or("file events closed")? {
                    FileTransferEvent::OutgoingCompleted { .. } => {
                        return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
                    }
                    FileTransferEvent::Error { message, .. } => return Err(message.into()),
                    _ => {}
                }
            }
        })
        .await??;
        println!("FILE_RECEIVER_CONFIRMED capture_subscriptions=0");
    }
    if std::env::var("REMOTE_PLAY_TEST_FILE_ONLY").as_deref() == Ok("1") {
        close(&connection, &mut events.control).await?;
        return Ok(());
    }
    connection
        .control(SessionCommand::ListSources { request_id: 1 })
        .await?;
    let sources = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            match events.control.recv().await.ok_or("control events closed")? {
                SessionCommand::Sources { sources, .. } => {
                    return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(sources);
                }
                SessionCommand::Error { reason, .. } => return Err(reason.into()),
                _ => {}
            }
        }
    })
    .await??;
    let source = sources
        .first()
        .ok_or("peer has no authorized capture source")?;
    let stats = Statistics::new();
    let media = client::ClientMediaRuntime::start_video_only(stats.clone())?;
    connection
        .subscribe(
            SubscriptionRequest {
                id: 256,
                source: source.source,
                width: 1280,
                height: 720,
                fps: 30,
                bitrate_kbps: 4000,
                audio: false,
            },
            media.decode_tx.clone(),
            stats.clone(),
            Arc::new(SharedHostStats::default()),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if stats.video_frames_decoded.load(Relaxed) > 10 {
                return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
            }
            while let Ok(event) = events.control.try_recv() {
                if let SessionCommand::Error { reason, .. }
                | SessionCommand::Closed { reason, .. } = event
                {
                    return Err(reason.into());
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    {
        let frame = media.shared_frame();
        let frame = frame.lock().unwrap();
        let frame = frame.as_ref().ok_or("no decoded frame")?;
        println!(
            "DEVICE_VIDEO_DECODED frames={} dimensions={}x{}",
            stats.video_frames_decoded.load(Relaxed),
            frame.width(),
            frame.height()
        );
    }
    close(&connection, &mut events.control).await?;
    Ok(())
}

#[cfg(target_os = "macos")]
async fn close(
    connection: &remote_core::workspace_session::WorkspaceConnection,
    control: &mut tokio::sync::mpsc::UnboundedReceiver<protocol::session::SessionCommand>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use protocol::session::SessionCommand;
    connection
        .control(SessionCommand::Close {
            connection_id: connection.id,
        })
        .await?;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(event) = control.recv().await {
            if matches!(event, SessionCommand::Closed { connection_id, .. } if connection_id == connection.id) { return; }
        }
    }).await?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This native decoding probe requires macOS.");
}
