//! Capture exactly one explicitly identified GUI test window through the authorized host.
//! No desktop fallback, no input, no public screenshots, and a single bounded PNG output.
use protocol::session::{CaptureSource, SessionCommand, SubscriptionRequest};
use remote_core::{
    SharedHostStats, Statistics, mesh::AppPrivateMeshConfigStore,
    workspace_session::WorkspaceConnection,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
type E = Box<dyn std::error::Error + Send + Sync>;
#[tokio::main]
async fn main() -> Result<(), E> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let target = std::env::var("WEAVELO_GUI_CAPTURE_TARGET")?.parse()?;
    let pid = std::env::var("WEAVELO_GUI_CAPTURE_PID")?.parse::<i32>()?;
    let title = std::env::var("WEAVELO_GUI_CAPTURE_TITLE")?;
    let output = PathBuf::from(std::env::var("WEAVELO_GUI_CAPTURE_OUTPUT")?);
    if pid <= 0
        || title != "Weavelo restored GUI review"
        || !output.is_absolute()
        || output.exists()
    {
        return Err("Explicit unique test window and fresh local output required".into());
    }
    let config = AppPrivateMeshConfigStore::new(std::env::var("WEAVELO_GUI_CAPTURE_PROFILE")?)
        .load()?
        .ok_or("authorized identity missing")?;
    remote_core::session_crypto::use_paired_session_secret(config.network_secret.expose_secret());
    let dir = tempfile::tempdir()?;
    let (conn, mut events) = WorkspaceConnection::connect(target, dir.path().into()).await?;
    conn.control(SessionCommand::ListSources { request_id: 711 })
        .await?;
    let source = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.control.recv().await.ok_or("source channel closed")? {
                SessionCommand::Sources {
                    request_id: 711,
                    sources,
                } => {
                    let found: Vec<_> = sources
                        .iter()
                        .filter(|s| {
                            s.process_id == Some(pid)
                                && s.title == title
                                && matches!(s.source, CaptureSource::Window(_))
                        })
                        .collect();
                    if found.len() != 1 {
                        return Err("The requested GUI test window is missing or ambiguous".into());
                    }
                    return Ok::<_, E>(found[0].source);
                }
                SessionCommand::Error { reason, .. } => return Err(reason.into()),
                _ => {}
            }
        }
    })
    .await??;
    let stats = Statistics::new();
    let media = client::ClientMediaRuntime::start_video_only(stats.clone())?;
    conn.subscribe(
        SubscriptionRequest {
            id: 711,
            source,
            width: 1280,
            height: 800,
            fps: 10,
            bitrate_kbps: 8000,
            audio: false,
        },
        media.decode_tx.clone(),
        stats.clone(),
        Arc::new(SharedHostStats::default()),
    )
    .await?;
    let frame = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if stats
                .video_frames_decoded
                .load(std::sync::atomic::Ordering::Relaxed)
                >= 3
            {
                let shared = media.shared_frame();
                if let Some(native) = shared.lock().unwrap().take() {
                    return Ok::<_, E>(client::desktop_frame::rgba(&native)?);
                }
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await??;
    image::save_buffer(
        &output,
        &frame.pixels,
        frame.width,
        frame.height,
        image::ColorType::Rgba8,
    )?;
    conn.control(SessionCommand::Unsubscribe { id: 711 })
        .await?;
    conn.control(SessionCommand::Close {
        connection_id: conn.id,
    })
    .await?;
    println!(
        "OWN_GUI_WINDOW_CAPTURED {}x{} local_png=true input=false desktop_fallback=false",
        frame.width, frame.height
    );
    Ok(())
}
