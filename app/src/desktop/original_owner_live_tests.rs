//! Explicit real-network restoration regression. Only uniquely named caller-created
//! test windows are captured; no full desktop, input, audio, clipboard or files.
use super::*;
use remote_core::mesh::{AppPrivateMeshConfigStore, MeshConfig};
use std::{collections::BTreeMap, time::Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[ignore = "requires explicit authorized profile and two own named synthetic windows"]
async fn restored_owner_sequential_relay_devices_never_reuse_old_picture() {
    let output = std::path::PathBuf::from(
        std::env::var("RP_RESTORED_SWITCH_OUTPUT").expect("explicit report path"),
    );
    assert!(output.is_absolute() && !output.exists());
    let run = std::env::var("RP_RESTORED_SWITCH_RUN").expect("unique fixture namespace");
    assert!(run.starts_with("20261001-restored-"));
    let mut report = serde_json::json!({"passed":false,"renderer_controller":"OriginalOwner","transport":"Relay",
        "only_named_synthetic_windows":true,"input_sent":false,"desktop_subscribed":false,
        "pixels_read_on_cpu_only_by_test_validator":true,"gui_mouse_clicks_simulated":false,"rounds":[]});
    let result =
        tokio::time::timeout(Duration::from_secs(145), run_sequence(&run, &mut report)).await;
    match &result {
        Ok(Ok(())) => report["passed"] = true.into(),
        Ok(Err(error)) => report["error"] = error.to_string().into(),
        Err(_) => report["error"] = "bounded test timeout".into(),
    };
    std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!("RESTORED_OWNER_RELAY_SEQUENCE {report}");
    assert!(matches!(result, Ok(Ok(()))));
}
async fn run_sequence(run: &str, report: &mut serde_json::Value) -> Result<(), Error> {
    let original = AppPrivateMeshConfigStore::new(std::env::var("RP_RESTORED_SWITCH_PROFILE")?)
        .load()?
        .ok_or("authorized profile missing")?;
    let root = tempfile::tempdir()?;
    let identity =
        MeshConfig::from_invite_code(&original.invite_code(), "Restored controller acceptance")?;
    remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
    let dir = root.path().join("mesh");
    AppPrivateMeshConfigStore::new(&dir).save(&identity)?;
    let socket = std::net::UdpSocket::bind("127.0.0.1:0")?;
    let port = socket.local_addr()?.port();
    drop(socket);
    let mut cfg = crate::UnifiedRuntimeConfig::app_defaults();
    cfg.mesh_dir = dir;
    cfg.discovery_port = port;
    cfg.display_name = "Restored controller acceptance".into();
    cfg.host_bind_addr = "127.0.0.1:0".parse()?;
    cfg.enable_passive_host = false;
    cfg.enable_client_receiver = false;
    cfg.enable_viewer_media = false;
    cfg.enable_workspace_viewer = true;
    cfg.enable_p2p = false;
    cfg.enable_talkback = false;
    cfg.enable_clipboard_sync = false;
    cfg.enable_file_transfer = false;
    let runtime = crate::start_unified_runtime(cfg).await?;
    let owner = OriginalOwner::new(runtime.owner.clone());
    owner.set_clipboard_sync_enabled(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut peers = BTreeMap::new();
    loop {
        owner.poll();
        for device in owner.snapshot().devices {
            if !device.online {
                continue;
            }
            let cleaned: String = device
                .display_name
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect();
            let key = if cleaned.contains("MacStudio") {
                Some("STUDIO")
            } else if cleaned.contains("MacBook") {
                Some("MACBOOK")
            } else {
                None
            };
            if let Some(key) = key {
                peers.insert(key, device.device_id);
            }
        }
        if peers.len() == 2 {
            break;
        }
        if Instant::now() > deadline {
            return Err("The two authorized Mac hosts were not discovered via Relay".into());
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    let mut last_connection = None;
    for label in [
        "STUDIO", "MACBOOK", "STUDIO", "MACBOOK", "STUDIO", "MACBOOK",
    ] {
        owner.disconnect_active().await?;
        if owner.frame_binding().is_some() {
            return Err("Closed device retained an active frame binding".into());
        }
        let key = &peers[label];
        owner.connect_silent_files(key).await?;
        let conn = owner.active_connection().ok_or("No selected connection")?;
        if last_connection == Some(conn.id) {
            return Err("A new device reused the prior connection ID".into());
        }
        last_connection = Some(conn.id);
        let snapshot = owner.snapshot();
        if snapshot
            .role
            .session()
            .is_none_or(|s| s.peer.device_id != *key)
        {
            return Err("Wrong selected device after reconnection".into());
        }
        if owner.frame_binding().unwrap().1.lock().unwrap().is_some() {
            return Err("Old image appeared in the new connection before subscribing".into());
        }
        owner.request_capture_sources().await?;
        let title = format!("RemotePlay Switch {run} {label}");
        let deadline = Instant::now() + Duration::from_secs(10);
        let source = loop {
            owner.poll();
            let snapshot = owner.snapshot();
            let found: Vec<_> = snapshot
                .sources
                .iter()
                .filter(|s| s.title == title && matches!(s.source, CaptureSource::Window(_)))
                .collect();
            if found.len() == 1 {
                break found[0].source;
            }
            if found.len() > 1 {
                return Err("Ambiguous synthetic test window; no capture started".into());
            }
            if Instant::now() > deadline {
                return Err(format!("Exact fixture {title} absent; no desktop fallback").into());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        };
        let epoch = Instant::now();
        owner
            .switch_capture_source(source, 640, 480, 15, 1800)
            .await?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last_decoded = None;
        let mut neutral_frames = 0u32;
        let mut correct_frames = 0u32;
        let (r, g, b) = loop {
            owner.poll();
            let candidate = owner
                .frame_binding()
                .and_then(|(_, slot)| slot.lock().unwrap().clone());
            if let Some(frame) = candidate
                && Some(frame.decoded_at) != last_decoded
            {
                if frame.decoded_at < epoch {
                    return Err("New subscription exposed a pre-connection frame".into());
                }
                last_decoded = Some(frame.decoded_at);
                // Pixel readback belongs only to this validator. The real restored
                // presentation continues to retain the native decoder buffer.
                let rgba = client::desktop_frame::rgba(&frame)?;
                let i = ((rgba.height as usize / 2) * rgba.width as usize
                    + rgba.width as usize * 4 / 5)
                    * 4;
                let (r, g, b) = (
                    i16::from(rgba.pixels[i]),
                    i16::from(rgba.pixels[i + 1]),
                    i16::from(rgba.pixels[i + 2]),
                );
                let blue = b > r + 70 && b > g + 40;
                let red = r > b + 70 && r > g + 40;
                let correct = if label == "STUDIO" { blue } else { red };
                let wrong_target = if label == "STUDIO" { red } else { blue };
                if wrong_target {
                    return Err(
                        format!("Wrong-device pixels observed for {label}: {r},{g},{b}").into(),
                    );
                }
                if correct {
                    correct_frames += 1;
                    if correct_frames >= 3 {
                        break (r, g, b);
                    }
                } else {
                    neutral_frames += 1;
                    correct_frames = 0;
                }
            }
            if let Some(error) = owner.snapshot().error {
                return Err(error.into());
            }
            if Instant::now() > deadline {
                report["last_observation"] = owner.diagnostic_snapshot();
                let p = owner.pool.lock().unwrap();
                if let Some(e) = p.active() {
                    let session = &e.session;
                    report["last_media_state"] = serde_json::json!({
                        "connected":session.connected,"paused":session.paused,"background_paused":session.background_paused,
                        "pending_activity":session.media_activity_pending(),"correct_frames_seen":correct_frames,
                        "neutral_frames_seen":neutral_frames,"udp_packets_received":session.stats.udp_packets_recv.load(std::sync::atomic::Ordering::Relaxed),
                        "jitter_push":session.stats.video_jitter_buffer_push.load(std::sync::atomic::Ordering::Relaxed),
                        "jitter_pop":session.stats.video_jitter_buffer_pop.load(std::sync::atomic::Ordering::Relaxed),
                        "decode_queue_dropped":session.stats.video_decode_queue_dropped.load(std::sync::atomic::Ordering::Relaxed),
                        "decoder_status":session.media.as_ref().map(|m|m.decode_status.lock().unwrap().clone())
                    });
                }
                return Err(format!("{label} never produced three consecutive correct frames; correct={correct_frames}; neutral={neutral_frames}").into());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        };
        let diagnostic = owner.diagnostic_snapshot();
        let current = diagnostic["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["connection_id"] == conn.id)
            .ok_or("diagnostic session missing")?;
        if current["route"] != "Relay" {
            return Err("Acceptance accidentally used LAN instead of the required Relay".into());
        }
        if !current["input_locked"].as_bool().unwrap_or(false) {
            return Err("Test unexpectedly unlocked remote input".into());
        }
        report["rounds"].as_array_mut().unwrap().push(serde_json::json!({"expected":label,"device_id":key,"connection_id":conn.id,"target":conn.target.to_string(),"source":format!("{source:?}"),"rgb":[r,g,b],"consecutive_correct_frames":correct_frames,"initial_or_neutral_frames":neutral_frames,"frame_newer_than_subscription":true,"route":"Relay"}));
        drop(conn);
    }
    owner.close_all();
    if owner.frame_binding().is_some() || !owner.sessions().is_empty() {
        return Err("Test sessions remained after close_all".into());
    }
    report["all_test_sessions_closed"] = true.into();
    Ok(())
}
