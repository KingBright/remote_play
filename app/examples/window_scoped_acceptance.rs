//! Explicit bounded real-input test: only two synthetic windows owned by the supplied fixture PID.
//! Refuses missing/ambiguous source identities. Never subscribes the desktop.
use protocol::{
    InputEvent,
    session::{CaptureSource, SessionCommand, SubscriptionRequest},
};
use remote_core::{
    SharedHostStats, Statistics, mesh::AppPrivateMeshConfigStore,
    workspace_session::WorkspaceConnection,
};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering::Relaxed},
    time::{Duration, Instant},
};
type E = Box<dyn std::error::Error + Send + Sync>;
fn state(root: &std::path::Path) -> Result<serde_json::Value, E> {
    Ok(serde_json::from_slice(&std::fs::read(
        root.join("state.json"),
    )?)?)
}
async fn click(conn: &WorkspaceConnection, id: u32, x: u16, y: u16) -> Result<(), E> {
    for event in [
        InputEvent::MouseMoveAbsolute { x, y },
        InputEvent::MouseDown(0),
        InputEvent::MouseUp(0),
    ] {
        conn.control(SessionCommand::SourceInput {
            id,
            source_revision: 0,
            event,
        })
        .await?;
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    Ok(())
}
async fn key(conn: &WorkspaceConnection, id: u32, code: u16) -> Result<(), E> {
    for pressed in [true, false] {
        conn.control(SessionCommand::SourceInput {
            id,
            source_revision: 0,
            event: InputEvent::Key {
                key_code: code,
                pressed,
                modifiers: 0,
            },
        })
        .await?;
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    Ok(())
}
async fn wait(
    root: &std::path::Path,
    check: impl Fn(&serde_json::Value) -> bool,
) -> Result<serde_json::Value, E> {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let s = state(root)?;
        if check(&s) {
            return Ok(s);
        };
        if Instant::now() > deadline {
            return Err(format!("fixture confirmation timed out: {s}").into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
#[tokio::main]
async fn main() -> Result<(), E> {
    let root = PathBuf::from(std::env::var("RP_WINDOW_FIXTURE_DIR")?);
    let output = root.join("acceptance.json");
    let mut report = serde_json::json!({"only_synthetic_windows":true,"desktop_subscribed":false,"passed":false});
    let result = tokio::time::timeout(Duration::from_secs(70), run(&root, &mut report)).await;
    match &result {
        Ok(Ok(())) => report["passed"] = true.into(),
        Ok(Err(e)) => report["error"] = e.to_string().into(),
        Err(_) => report["error"] = "bounded test timeout".into(),
    };
    std::fs::write(output, serde_json::to_vec_pretty(&report)?)?;
    println!("WINDOW_SCOPED_ACCEPTANCE {report}");
    result??;
    Ok(())
}
async fn run(root: &std::path::Path, report: &mut serde_json::Value) -> Result<(), E> {
    let initial = state(root)?;
    let pid = initial["pid"].as_i64().ok_or("missing fixture PID")? as i32;
    let identity = AppPrivateMeshConfigStore::new(std::env::var("RP_PROBE_PROFILE")?)
        .load()?
        .ok_or("missing authorized identity")?;
    remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
    let target = std::env::var("RP_PROBE_TARGET")?.parse()?;
    let temp = tempfile::tempdir()?;
    let (conn, mut events) = WorkspaceConnection::connect(target, temp.path().into()).await?;
    conn.control(SessionCommand::ListSources { request_id: 900 })
        .await?;
    let sources = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.control.recv().await.ok_or("closed")? {
                SessionCommand::Sources {
                    request_id: 900,
                    sources,
                } => return Ok::<_, E>(sources),
                SessionCommand::Error { reason, .. } => return Err(reason.into()),
                _ => {}
            }
        }
    })
    .await??;
    let mut streams = Vec::new();
    for (label, id) in [("A", 256), ("B", 512)] {
        let wid = initial[label]["id"]
            .as_u64()
            .ok_or("fixture window id missing")? as u32;
        let source = sources
            .iter()
            .find(|s| s.source == CaptureSource::Window(wid) && s.process_id == Some(pid))
            .ok_or("exact fixture window not enumerated")?;
        if !source.supports_input {
            return Err(format!("{label} was still advertised read-only").into());
        }
        let stats = Statistics::new();
        let media = client::ClientMediaRuntime::start_video_only(stats.clone())?;
        conn.subscribe(
            SubscriptionRequest {
                id,
                source: source.source,
                width: 800,
                height: 600,
                fps: 10,
                bitrate_kbps: 2500,
                audio: false,
            },
            media.decode_tx.clone(),
            stats.clone(),
            Arc::new(SharedHostStats::default()),
        )
        .await?;
        streams.push((id, media, stats));
    }
    let deadline = Instant::now() + Duration::from_secs(12);
    while streams
        .iter()
        .any(|(_, _, stats)| stats.video_frames_decoded.load(Relaxed) < 4)
    {
        while let Ok(e) = events.control.try_recv() {
            if let SessionCommand::Error { reason, .. } = e {
                return Err(reason.into());
            }
        }
        if Instant::now() > deadline {
            return Err("window video missing".into());
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    report["both_window_video_received"] = true.into();
    let mut input_diagnostics = events.control;
    tokio::spawn(async move {
        while let Some(event) = input_diagnostics.recv().await {
            if let SessionCommand::Error { request_id, reason } = event {
                eprintln!("HOST_WINDOW_INPUT_ERROR {request_id}: {reason}");
            }
        }
    });

    click(&conn, 256, 16384, 24576).await?;
    let a = wait(root, |s| s["A"]["clicks"].as_u64().unwrap_or(0) == 1).await?;
    if a["B"]["clicks"] != 0 {
        return Err("input reached the other same-process window".into());
    }
    let point = a["A"]["last_point"]
        .as_array()
        .ok_or("click position unavailable")?;
    if (point[0].as_f64().unwrap() - 0.25).abs() > 0.02
        || (point[1].as_f64().unwrap() - 0.375).abs() > 0.02
    {
        return Err(format!("normalized coordinate mismatch: {point:?}").into());
    }
    key(&conn, 256, 0).await?;
    wait(root, |s| s["A"]["last_key"] == "a" && s["B"]["keys"] == 0).await?;
    report["window_A_click_key_and_coordinates"] = true.into();
    click(&conn, 512, 32768, 32768).await?;
    wait(root, |s| s["B"]["clicks"] == 1).await?;
    key(&conn, 512, 11).await?;
    wait(root, |s| s["B"]["last_key"] == "b" && s["A"]["keys"] == 1).await?;
    report["same_process_window_isolation"] = true.into();
    // With B focused, keyboard events still addressed to A must reach neither window.
    key(&conn, 256, 8).await?;
    for pressed in [true, false] {
        conn.control(SessionCommand::SourceInput {
            id: 512,
            source_revision: 99,
            event: InputEvent::Key {
                key_code: 8,
                pressed,
                modifiers: 0,
            },
        })
        .await?;
        conn.control(SessionCommand::Input {
            id: 512,
            event: InputEvent::Key {
                key_code: 8,
                pressed,
                modifiers: 0,
            },
        })
        .await?;
    }
    tokio::time::sleep(Duration::from_millis(350)).await;
    let isolated = state(root)?;
    if isolated["A"]["keys"] != 1 || isolated["B"]["keys"] != 1 {
        return Err("stale, unfocused or unscoped input reached a fixture window".into());
    }
    report["unfocused_stale_and_legacy_input_rejected"] = true.into();
    conn.control(SessionCommand::ReleaseInput { id: 512 })
        .await?;
    std::fs::write(
        root.join("command.json"),
        br#"{"id":"move1","action":"move_resize_A"}"#,
    )?;
    wait(root, |s| s["command"] == "move1").await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    click(&conn, 256, 49151, 32768).await?;
    let moved = wait(root, |s| s["A"]["clicks"] == 2).await?;
    let point = moved["A"]["last_point"]
        .as_array()
        .ok_or("moved point missing")?;
    if (point[0].as_f64().unwrap() - 0.75).abs() > 0.02
        || (point[1].as_f64().unwrap() - 0.5).abs() > 0.02
    {
        return Err("moved/resized mapping failed".into());
    }
    report["move_resize_mapping"] = true.into();
    report["geometry_test_restored_target_to_foreground"] = true.into();

    conn.control(SessionCommand::SetActivity {
        id: 256,
        revision: 1,
        video: false,
        audio: false,
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    click(&conn, 256, 32768, 32768).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    if state(root)?["A"]["clicks"] != 2 {
        return Err("paused window accepted input".into());
    }
    report["paused_window_rejects_input"] = true.into();
    std::fs::write(
        root.join("command.json"),
        br#"{"id":"close1","action":"close_A"}"#,
    )?;
    wait(root, |s| s["command"] == "close1").await?;
    conn.control(SessionCommand::SetActivity {
        id: 256,
        revision: 2,
        video: true,
        audio: false,
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    click(&conn, 256, 32768, 32768).await?;
    key(&conn, 256, 8).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let final_state = state(root)?;
    if final_state["B"]["keys"] != 1 || final_state["B"]["clicks"] != 1 {
        return Err("hidden window input was redirected to another window".into());
    }
    report["hidden_window_does_not_fallback"] = true.into();
    report["fixture_final"] = final_state;
    report["decoded_frames"] = serde_json::json!([
        streams[0].2.video_frames_decoded.load(Relaxed),
        streams[1].2.video_frames_decoded.load(Relaxed)
    ]);
    for (id, _, _) in &streams {
        conn.control(SessionCommand::ReleaseInput { id: *id })
            .await?;
    }
    conn.control(SessionCommand::Close {
        connection_id: conn.id,
    })
    .await?;
    Ok(())
}
