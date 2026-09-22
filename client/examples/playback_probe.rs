//! Exercise the production receiver and VideoToolbox decoder, without a GUI.
//! The default mutes local playback to avoid feedback in a same-host loopback.
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("The native playback probe currently requires macOS.");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use client::audio_player::AudioPlayerSettings;
    use client::{ClientMediaRuntime, ClientSessionReceiverConfig, SharedHostStats};
    use protocol::ControlMessage;
    use remote_core::VideoFrame;
    use remote_core::net::UdpMultiplexer;
    use remote_core::stats::Statistics;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
    use std::time::{Duration, Instant};

    let target = std::env::var("REMOTE_PLAY_PROBE_HOST")
        .unwrap_or_else(|_| "127.0.0.1:49373".into())
        .parse()?;
    let duration = Duration::from_secs(env_u32("REMOTE_PLAY_PROBE_SECONDS", 15) as u64);
    let same_host = std::env::var("REMOTE_PLAY_PROBE_SAME_HOST").as_deref() == Ok("1");
    let pause_at = std::env::var("REMOTE_PLAY_PROBE_PAUSE_AT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let pause_seconds = u64::from(env_u32("REMOTE_PLAY_PROBE_PAUSE_SECONDS", 20));
    let update_at = std::env::var("REMOTE_PLAY_PROBE_UPDATE_AT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    if pause_at.is_some_and(|at| duration.as_secs() <= at + pause_seconds + 1) {
        return Err(
            "probe duration must include pause plus at least two seconds of resumed video".into(),
        );
    }
    let stats = Statistics::new();
    let media = ClientMediaRuntime::start(stats.clone())?;
    media.audio_playback.set_settings(AudioPlayerSettings {
        remote_system_muted: true,
        remote_microphone_muted: true,
        ..Default::default()
    });
    let frame_slot = media.shared_frame();
    let mux = UdpMultiplexer::bind("127.0.0.1:0").await?;
    let bind_addr = mux.local_addr()?;
    let (sender, receiver) = mux.split();
    let session_id = 4242;
    let host_stats = Arc::new(SharedHostStats::default());
    host_stats.media_pause.begin_session(target, session_id);
    let receive_task = client::spawn_client_session_receiver(ClientSessionReceiverConfig {
        bind_addr,
        udp_receiver: receiver,
        stats: stats.clone(),
        active_session_id: Arc::new(AtomicU32::new(session_id)),
        host_stats: host_stats.clone(),
        audio_tx: client::audio_ingress_bridge(media.audio_tx.clone()),
        decode_tx: media.decode_tx.clone(),
        clipboard_control: None,
        file_transfer_control: None,
        session_event_tx: None,
    });
    let started = Instant::now();
    sender
        .send_control(
            &ControlMessage::StartStream {
                width: env_u32("REMOTE_PLAY_PROBE_WIDTH", 1920),
                height: env_u32("REMOTE_PLAY_PROBE_HEIGHT", 1080),
                fps: env_u32("REMOTE_PLAY_PROBE_FPS", 60),
                bitrate_kbps: env_u32("REMOTE_PLAY_PROBE_BITRATE", 8000),
                session_id,
            },
            target,
        )
        .await?;
    let deadline = tokio::time::Instant::now() + duration;
    let mut poll = tokio::time::interval(Duration::from_millis(2));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut heartbeat = tokio::time::interval(Duration::from_millis(500));
    let mut decode_ms = Vec::new();
    let mut capture_to_decode_ms = Vec::new();
    let mut first_frame_ms = None;
    let mut previous_frame_at: Option<Instant> = None;
    let mut steady_gaps_ms = Vec::new();
    let mut steady_frames = 0;
    let mut steady_capture_to_decode_ms = Vec::new();
    let mut dimensions = (0, 0);
    let mut pause_started = None::<Instant>;
    let mut resume_started = None::<Instant>;
    let mut pause_ack_ms = None;
    let mut resume_ack_ms = None;
    let mut resume_first_frame_ms = None;
    let mut decoded_while_paused = 0usize;
    let mut rates_updated = false;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = heartbeat.tick() => {
                sender.send_control(&ControlMessage::Heartbeat, target).await?;
            }
            _ = poll.tick() => {
                if let Some(at) = pause_at {
                    if pause_started.is_none() && started.elapsed().as_secs() >= at {
                        pause_started = Some(Instant::now());
                        host_stats.media_pause.set_paused(true);
                    }
                    if resume_started.is_none() && started.elapsed().as_secs() >= at + pause_seconds {
                        resume_started = Some(Instant::now());
                        host_stats.media_pause.set_paused(false);
                    }
                    if !host_stats.media_pause.is_pending() {
                        if let Some(resumed) = resume_started {
                            resume_ack_ms.get_or_insert_with(|| resumed.elapsed().as_secs_f64() * 1000.0);
                        } else if let Some(paused) = pause_started {
                            pause_ack_ms.get_or_insert_with(|| paused.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                }
                if !rates_updated && update_at.is_some_and(|at| started.elapsed().as_secs() >= at) {
                    sender.send_control(&ControlMessage::UpdateStreamSettings {
                        width: env_u32("REMOTE_PLAY_PROBE_WIDTH", 1920),
                        height: env_u32("REMOTE_PLAY_PROBE_HEIGHT", 1080),
                        fps: env_u32("REMOTE_PLAY_PROBE_NEXT_FPS", 37),
                        bitrate_kbps: env_u32("REMOTE_PLAY_PROBE_NEXT_BITRATE", 3500),
                        session_id,
                    }, target).await?;
                    rates_updated = true;
                }
                // Release the previous IOSurface outside the shared-frame mutex.
                let frame = frame_slot.lock().unwrap().take();
                if let Some(frame) = frame {
                    if let Some(resumed) = resume_started {
                        resume_first_frame_ms.get_or_insert_with(|| resumed.elapsed().as_secs_f64() * 1000.0);
                    } else if pause_started.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
                        decoded_while_paused += 1;
                    }
                    let now = Instant::now();
                    first_frame_ms.get_or_insert_with(|| started.elapsed().as_secs_f64() * 1000.0);
                    // Exclude the first two seconds only from explicitly named
                    // steady-state metrics; full-run counters retain startup.
                    if started.elapsed() >= Duration::from_secs(2) {
                        steady_frames += 1;
                        if let Some(previous) = previous_frame_at {
                            steady_gaps_ms.push(now.duration_since(previous).as_secs_f64() * 1000.0);
                        }
                    }
                    previous_frame_at = Some(now);
                    dimensions = (frame.width(), frame.height());
                    decode_ms.push(f64::from(frame.decode_cost_ms));
                    if same_host && frame.timing.capture_ts_us > 0 {
                        let observation_delay = frame.decoded_at.elapsed().as_micros() as u64;
                        let elapsed_ms = remote_core::timing::quanta_now_us()
                            .saturating_sub(observation_delay)
                            .saturating_sub(frame.timing.capture_ts_us) as f64 / 1000.0;
                        capture_to_decode_ms.push(elapsed_ms);
                        if started.elapsed() >= Duration::from_secs(2) {
                            steady_capture_to_decode_ms.push(elapsed_ms);
                        }
                    }
                }
            }
        }
    }
    sender
        .send_control(&ControlMessage::StopStream, target)
        .await?;
    receive_task.abort();
    let decoded = stats.video_frames_decoded.load(Relaxed);
    let host = host_stats.snapshot();
    println!(
        "latest_host_fps={:.3} latest_host_encode_ms={:.3} latest_host_bitrate_kbps={}",
        host.fps, host.latency, host.bitrate_kbps
    );
    println!("decode_errors={}", stats.video_decode_errors.load(Relaxed));
    println!(
        "decoded={decoded} observed={} size={}x{} first_frame_ms={:.3} decoded_fps={:.3} decode_p50_ms={:.3} decode_p95_ms={:.3} decode_p99_ms={:.3} decode_queue_dropped={} audio_ingress_dropped={}",
        decode_ms.len(),
        dimensions.0,
        dimensions.1,
        first_frame_ms.unwrap_or(-1.0),
        decoded as f64 / duration.as_secs_f64(),
        percentile(&decode_ms, 0.5),
        percentile(&decode_ms, 0.95),
        percentile(&decode_ms, 0.99),
        stats.video_decode_queue_dropped.load(Relaxed),
        stats.audio_ingress_dropped.load(Relaxed)
    );
    println!(
        "steady_observed_fps={:.3} steady_gap_p99_ms={:.3} steady_gap_max_ms={:.3} steady_gaps_over_100ms={}",
        steady_frames as f64 / (duration.as_secs_f64() - 2.0).max(1.0),
        percentile(&steady_gaps_ms, 0.99),
        percentile(&steady_gaps_ms, 1.0),
        steady_gaps_ms.iter().filter(|&&gap| gap > 100.0).count()
    );
    if same_host {
        println!(
            "same_host_capture_to_decode_p50_ms={:.3} p95_ms={:.3} p99_ms={:.3}",
            percentile(&capture_to_decode_ms, 0.5),
            percentile(&capture_to_decode_ms, 0.95),
            percentile(&capture_to_decode_ms, 0.99)
        );
        println!(
            "steady_same_host_capture_to_decode_p50_ms={:.3} p95_ms={:.3} p99_ms={:.3}",
            percentile(&steady_capture_to_decode_ms, 0.5),
            percentile(&steady_capture_to_decode_ms, 0.95),
            percentile(&steady_capture_to_decode_ms, 0.99)
        );
    }
    if decoded == 0 || dimensions.0 == 0 || dimensions.1 == 0 {
        return Err("no decoded video frame observed".into());
    }
    if pause_at.is_some() {
        println!(
            "pause_ack_ms={:.3} resume_ack_ms={:.3} resume_first_frame_ms={:.3} decoded_while_paused_after_grace={decoded_while_paused}",
            pause_ack_ms.unwrap_or(-1.0),
            resume_ack_ms.unwrap_or(-1.0),
            resume_first_frame_ms.unwrap_or(-1.0)
        );
        if pause_ack_ms.is_none()
            || resume_ack_ms.is_none()
            || resume_first_frame_ms.is_none()
            || decoded_while_paused != 0
        {
            return Err("pause/resume acknowledgement or playback failed".into());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(target_os = "macos")]
fn percentile(values: &[f64], q: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
        .get((sorted.len().saturating_sub(1) as f64 * q) as usize)
        .copied()
        .unwrap_or(0.0)
}
