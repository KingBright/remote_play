#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use protocol::session::{SessionCommand, SubscriptionRequest};
    use remote_core::file_transfer_runtime::{FileTransferCommand, FileTransferEvent};
    use remote_core::workspace_session::WorkspaceConnection;
    use remote_core::{SharedHostStats, Statistics, VideoFrame};
    use std::sync::{Arc, atomic::Ordering::Relaxed};
    use std::time::{Duration, Instant};
    let scratch = tempfile::tempdir()?;
    let socket = std::net::UdpSocket::bind("127.0.0.1:0")?;
    let target = socket.local_addr()?;
    drop(socket);
    let host_stats = Statistics::new();
    let host = tokio::spawn(host::run_host_service(host::HostServiceConfig {
        bind_addr: target,
        stats: host_stats.clone(),
        enable_clipboard_sync: false,
        enable_file_transfer: true,
        enable_talkback: false,
    }));
    tokio::time::sleep(Duration::from_millis(150)).await;
    let (connection, mut events) =
        WorkspaceConnection::connect(target, scratch.path().join("received")).await?;
    let source_file = scratch
        .path()
        .join(format!("v2-file-only-{}.bin", std::process::id()));
    let bytes: Vec<u8> = (0..1_048_576).map(|n| (n % 251) as u8).collect();
    tokio::fs::write(&source_file, &bytes).await?;
    connection
        .file_commands
        .send(FileTransferCommand::SendFile {
            path: source_file.clone(),
            mime_type: None,
        })
        .await?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let event = tokio::time::timeout_at(deadline.into(), events.files.recv())
            .await?
            .ok_or("file event closed")?;
        match event {
            FileTransferEvent::OutgoingCompleted { .. } => break,
            FileTransferEvent::Error { message, .. } => return Err(message.into()),
            _ => {}
        }
    }
    let received_file = std::env::var_os("REMOTE_PLAY_FILE_RECEIVE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("remote-play-host-received-files"))
        .join(source_file.file_name().unwrap());
    assert_eq!(tokio::fs::read(&received_file).await?, bytes);
    println!(
        "FILE_ONLY_CONFIRMED bytes={} capture_subscriptions=0",
        bytes.len()
    );
    tokio::fs::remove_file(received_file).await?;
    connection
        .control(SessionCommand::ListSources { request_id: 1 })
        .await?;
    let sources = loop {
        match tokio::time::timeout(Duration::from_secs(5), events.control.recv())
            .await?
            .ok_or("control closed")?
        {
            SessionCommand::Sources { sources, .. } => break sources,
            SessionCommand::Error { reason, .. } => return Err(reason.into()),
            _ => {}
        }
    };
    let sources: Vec<_> = sources
        .into_iter()
        .filter(|s| s.title.starts_with("RemotePlay V2 Probe"))
        .take(2)
        .collect();
    if sources.len() != 2 {
        return Err("two generated RemotePlay V2 Probe windows are required".into());
    }
    let mut views = Vec::new();
    for (i, source) in sources.iter().enumerate() {
        let stats = Statistics::new();
        let media = client::ClientMediaRuntime::start_video_only(stats.clone())?;
        connection
            .subscribe(
                SubscriptionRequest {
                    id: (i as u32 + 1) * 256,
                    source: source.source,
                    width: 1280,
                    height: 1080,
                    fps: 30,
                    bitrate_kbps: 2000,
                    audio: true,
                },
                media.decode_tx.clone(),
                stats.clone(),
                Arc::new(SharedHostStats::default()),
            )
            .await?;
        views.push((media, stats));
    }
    let mut owners = Vec::new();
    while owners.len() < 2 {
        match tokio::time::timeout(Duration::from_secs(8), events.control.recv())
            .await?
            .ok_or("control closed")?
        {
            SessionCommand::Subscribed {
                id, audio_owner, ..
            } => {
                println!("SUBSCRIBED id={id} audio_owner={audio_owner:?}");
                owners.push(audio_owner);
            }
            SessionCommand::Error { reason, .. } => return Err(reason.into()),
            _ => {}
        }
    }
    assert_eq!(
        owners[0], owners[1],
        "two windows of one app must share audio"
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    println!(
        "HOST_PIPELINE captured={} encoded={}",
        host_stats.video_frames_captured.load(Relaxed),
        host_stats.video_frames_encoded.load(Relaxed)
    );
    for (i, (media, stats)) in views.iter().enumerate() {
        let count = stats.video_frames_decoded.load(Relaxed);
        let frame = media.shared_frame();
        let frame = frame.lock().unwrap();
        let frame = frame.as_ref().ok_or("no decoded frame")?;
        let expected = sources[i].width as f64 / sources[i].height as f64;
        let actual = frame.width() as f64 / frame.height() as f64;
        println!(
            "VISIBLE id={} decoded={} dimensions={}x{} source={}x{} errors={}",
            (i + 1) * 256,
            count,
            frame.width(),
            frame.height(),
            sources[i].width,
            sources[i].height,
            stats.video_decode_errors.load(Relaxed)
        );
        assert!(count > 10, "both independent windows must decode");
        assert!(
            (actual / expected - 1.).abs() < 0.02,
            "source aspect must be preserved"
        );
    }
    let resized = SubscriptionRequest {
        id: 256,
        source: sources[0].source,
        width: 640,
        height: 480,
        fps: 37,
        bitrate_kbps: 3500,
        audio: true,
    };
    connection.update_settings(&resized).await?;
    // An old Subscribe datagram may be retried after a newer Configure. It must
    // acknowledge the original subscription without restoring obsolete settings.
    connection
        .control(SessionCommand::Subscribe(SubscriptionRequest {
            width: 1280,
            height: 1080,
            fps: 30,
            bitrate_kbps: 2000,
            ..resized.clone()
        }))
        .await?;
    let mut configured = false;
    let mut replayed = false;
    while !configured || !replayed {
        match tokio::time::timeout(Duration::from_secs(5), events.control.recv())
            .await?
            .ok_or("control closed")?
        {
            SessionCommand::Configured { id: 256, .. } => configured = true,
            SessionCommand::Subscribed { id: 256, .. } => replayed = true,
            SessionCommand::Error { reason, .. } => return Err(reason.into()),
            _ => {}
        }
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frames = views[0].0.shared_frame();
            if frames
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|f| f.width() <= 640 && f.height() <= 480)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    println!("CUSTOM_RATES_AND_RESIZE fps=37 bitrate_kbps=3500 stale_subscribe_ignored=true");
    connection
        .control(SessionCommand::SetActivity {
            id: 256,
            revision: 1,
            video: false,
            audio: false,
        })
        .await?;
    loop {
        if matches!(
            tokio::time::timeout(Duration::from_secs(5), events.control.recv()).await?,
            Some(SessionCommand::Activity {
                id: 256,
                video: false,
                ..
            })
        ) {
            break;
        }
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let before = [
        views[0].1.video_frames_decoded.load(Relaxed),
        views[1].1.video_frames_decoded.load(Relaxed),
    ];
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = [
        views[0].1.video_frames_decoded.load(Relaxed),
        views[1].1.video_frames_decoded.load(Relaxed),
    ];
    println!(
        "INDEPENDENT_PAUSE hidden_delta={} visible_delta={}",
        after[0] - before[0],
        after[1] - before[1]
    );
    assert_eq!(after[0], before[0]);
    assert!(after[1] > before[1] + 10);
    let resumed = Instant::now();
    connection
        .control(SessionCommand::SetActivity {
            id: 256,
            revision: 2,
            video: true,
            audio: true,
        })
        .await?;
    while views[0].1.video_frames_decoded.load(Relaxed) == after[0] {
        if resumed.elapsed() > Duration::from_secs(5) {
            return Err("resume did not decode a new frame".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    println!("RESUME_FIRST_DECODE_MS {}", resumed.elapsed().as_millis());
    connection
        .control(SessionCommand::Unsubscribe { id: 256 })
        .await?;
    let other = views[1].1.video_frames_decoded.load(Relaxed);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(views[1].1.video_frames_decoded.load(Relaxed) > other);
    println!("UNSUBSCRIBE_ISOLATED true");
    drop(connection);
    drop(views);
    tokio::time::sleep(Duration::from_millis(250)).await;
    host.abort();
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Native capture probe currently requires macOS");
}
