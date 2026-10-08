//! Bounded local encoded-fixture test of the production ClientMediaRuntime.
//! Uses its real RTP input queue and reset/drop path; never captures or injects input.
#[cfg(any(all(target_os="windows",feature="native-windows-video"),all(target_os="linux",feature="native-linux-video")))]
#[path = "support/hevc_fixture.rs"]
mod fixture;
#[cfg(any(all(target_os="windows",feature="native-windows-video"),all(target_os="linux",feature="native-linux-video")))]
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use protocol::{RtpHeader, RtpPacket};
    use std::{
        path::PathBuf,
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };
    let input = std::fs::read(std::env::var("RP_NATIVE_HEVC_FIXTURE")?)?;
    let output = PathBuf::from(std::env::var("RP_NATIVE_HEVC_RECEIPT")?);
    if output.exists() || input.len() > 32 * 1024 * 1024 {
        return Err("invalid output or fixture bound".into());
    }
    let units = fixture::access_units(&input);
    if units.len() != 60 {
        return Err("fixture must contain 60 frames".into());
    }
    #[cfg(target_os="windows")]
    use client::windows_playback::active_workers;
    #[cfg(target_os="linux")]
    use client::linux_playback::active_workers;
    let baseline = active_workers();
    let stats = remote_core::Statistics::new();
    let media = client::ClientMediaRuntime::start_native(stats.clone(), false, 30)?;
    let slot = media.shared_frame();
    let notify = media.frame_updates();
    let begun = Instant::now();
    let mut sent = 0usize;
    let mut observed = std::collections::BTreeSet::new();
    let mut details = Vec::new();
    let packet = |unit: usize, seq: u16, ssrc: u32, timestamp: u32| RtpPacket {
        header: RtpHeader {
            version: 2,
            payload_type: 96,
            sequence_number: seq,
            timestamp,
            ssrc,
        },
        payload: units[unit].to_vec(),
    };
    while (!observed.contains(&1059) || stats.video_frames_decoded.load(Ordering::Relaxed) < 60)
        && begun.elapsed() < Duration::from_secs(9)
    {
        if sent < units.len()
            && begun.elapsed() >= Duration::from_nanos(sent as u64 * 1_000_000_000 / 30)
        {
            media
                .decode_tx
                .send((
                    packet(sent, sent as u16, 900, 1000 + sent as u32),
                    Default::default(),
                ))
                .await?;
            sent += 1;
        }
        let next = slot.lock().unwrap().take();
        if let Some(frame) = next {
            #[cfg(target_os="windows")]
            let native=frame.native.as_ref().ok_or("CPU frame on explicit native path")?;
            #[cfg(target_os="linux")]
            let native=frame.native_linux.as_ref().ok_or("CPU frame on explicit native path")?;
            #[cfg(target_os="windows")]
            let resource=&native.ready.frame;
            #[cfg(target_os="linux")]
            let resource=&native.frame;
            if !frame.rgba.is_empty() || native.generation != 1 {
                return Err("native storage or generation mismatch".into());
            }
            if resource.was_submitted() {
                return Err("unpainted decoded frame incorrectly acknowledges a draw".into());
            }
            observed.insert(frame.timestamp);
            details.push(serde_json::json!({"rtp_timestamp":frame.timestamp,"coded":resource.geometry().coded,"visible":resource.geometry().visible,"elapsed_us":frame.decode_cost_ms*1000.}));
        }
        tokio::select! {_=notify.notified()=>{},_=tokio::time::sleep(Duration::from_millis(1))=>{}}
    }
    let status = media.decode_status.lock().unwrap().clone();
    let produced = stats.video_frames_decoded.load(Ordering::Relaxed);
    let dropped = stats.video_decode_queue_dropped.load(Ordering::Relaxed);
    // The production buffer is latest-only. The consumer is intentionally not
    // guaranteed every intermediate publication: count producer output separately
    // rather than pretending an observed frame equals a decoded frame.
    if produced != 60 || !observed.contains(&1059) || dropped != 0 || !status.is_empty() {
        return Err(format!("native output: produced={produced}/60 observed={} queue_dropped={dropped} latest={:?}; status={status}; errors={}",observed.len(),observed.last(),stats.video_decode_errors.load(Ordering::Relaxed)).into());
    }
    // Do not declare input safe from a previously displayed resource after reset.
    media.reset_video();
    if slot.lock().unwrap().is_some() || !stats.video_decoder_needs_keyframe.load(Ordering::Acquire)
    {
        return Err("reset kept an obsolete frame or recovery flag".into());
    }
    media
        .decode_tx
        .send((packet(1, 100, 901, 8001), Default::default()))
        .await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    if slot.lock().unwrap().is_some() || !stats.video_decoder_needs_keyframe.load(Ordering::Acquire)
    {
        return Err("non-key input bypassed source recovery".into());
    }
    for index in 0..6 {
        media
            .decode_tx
            .send((
                packet(index, 101 + index as u16, 901, 9000 + index as u32),
                Default::default(),
            ))
            .await?;
        tokio::time::sleep(Duration::from_millis(35)).await;
    }
    let until = Instant::now() + Duration::from_secs(3);
    let mut recovered = None;
    while Instant::now() < until {
        if let Some(frame) = slot.lock().unwrap().take() {
            #[cfg(target_os="windows")]
            let native=frame.native.as_ref().ok_or("recovery reverted to CPU")?;
            #[cfg(target_os="linux")]
            let native=frame.native_linux.as_ref().ok_or("recovery reverted to CPU")?;
            if native.generation != 2 || frame.timestamp < 9000 {
                return Err("old source output crossed reset".into());
            }
            recovered = Some((frame.timestamp, native.generation));
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    if recovered.is_none() {
        return Err(format!(
            "keyframe recovery missing: {}",
            media.decode_status.lock().unwrap()
        )
        .into());
    }
    drop(media);
    drop(slot);
    let until = Instant::now() + Duration::from_secs(3);
    while active_workers() != baseline && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    if active_workers() != baseline {
        return Err("native worker retained after media owner was dropped".into());
    }
    let report = serde_json::json!({"passed":true,"production_media_runtime":true,"frames_from_rtp_queue":produced,"latest_frames_observed":observed.len(),"latest_slot_replacements":produced-observed.len(),"cpu_decoded_bytes":0,"native_storage":true,"no_unpainted_input_ack":true,"reset_rejects_non_key_input":true,"recovered_timestamp_and_generation":recovered,"worker_count_returned_to_baseline":true,"no_network_capture_input":true,"frames":details,"display_scanout_tested":false});
    std::fs::write(output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "PRODUCTION_NATIVE_MEDIA_PASS frames={} reset_generation=2 no_cpu_pixels=true worker_released=true",
        produced
    );
    Ok(())
}
#[cfg(not(any(all(target_os="windows",feature="native-windows-video"),all(target_os="linux",feature="native-linux-video"))))]
fn main() {
    eprintln!("Explicit native pipeline validation requires an enabled Linux or Windows native backend.");
}
