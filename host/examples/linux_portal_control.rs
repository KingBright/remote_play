//! Manual control-plane entry. No action occurs without an explicit --select-*.
//! Optional --sample-frames=N observes product-owned raw pixels, never exports them.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This manual portal control check requires Linux; no portal was called.");
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use host::{PickerSources, request_local_portal_capture};
    let mut sources = None;
    let mut parent = String::new();
    let mut sample_frames: Option<usize> = None;
    let mut trace_stages = false;
    let mut selection_timeout = 45u64;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--select-window" if sources.is_none() => sources = Some(PickerSources::Window),
            "--select-monitor" if sources.is_none() => sources = Some(PickerSources::Monitor),
            "--trace-stages" => trace_stages = true,
            value if value.starts_with("--selection-timeout-seconds=") => {
                selection_timeout = value[28..].parse::<u64>()?;
                if !(5..=120).contains(&selection_timeout) { return Err("selection timeout must be 5..=120 seconds".into()); }
            }
            value if value.starts_with("--parent=") => parent = value[9..].to_owned(),
            value if value.starts_with("--sample-frames=") && sample_frames.is_none() => {
                let count = value[16..].parse::<usize>()?;
                if !(1..=60).contains(&count) { return Err("sample frames must be 1..=60".into()); }
                sample_frames = Some(count);
            }
            _ => return Err("use exactly one --select-window/--select-monitor and optional --parent=wayland:HANDLE or x11:XID / --sample-frames=1..60".into()),
        }
    }
    let Some(sources) = sources else {
        eprintln!(
            "No selection requested. Pass --select-window or --select-monitor to open the normal local system picker; no portal was called."
        );
        return Ok(());
    };
    let (cancel, cancel_rx) = tokio::sync::watch::channel(false);
    let cancel_task = tokio::spawn(async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("PORTAL_CANCEL cause=interrupt");
            },
            _ = async {
                if sample_frames.is_some() { tokio::time::sleep(std::time::Duration::from_secs(selection_timeout)).await; }
                else { std::future::pending::<()>().await; }
            } => {
                eprintln!("PORTAL_CANCEL cause=controlled_deadline seconds={selection_timeout}");
            },
        }
        cancel.send_replace(true);
        std::future::pending::<()>().await;
    });
    let request = request_local_portal_capture(&parent, sources, cancel_rx);
    let result = if trace_stages {
        host::with_portal_stage_receipts(request).await
    } else {
        request.await
    };
    match result {
        Ok(mut lease) => {
            let selected = lease.selected_source();
            println!(
                "Selected {:?}; logical size {:?}; capture pixels and presentation remain unverified.",
                selected.kind, selected.logical_size
            );
            if let Some(count) = sample_frames {
                let sampled = sample_owned_frames(lease, count).await;
                cancel_task.abort();
                return sampled;
            }
            let (_, fd) = lease.take_pipewire_remote()?;
            drop(fd); // This check never connects a global or restricted PipeWire remote.
            let closed = lease.close().await;
            println!("Restricted FD returned once; Close RPC receipt={closed}.");
        }
        Err(error) => {
            eprintln!(
                "RAW_CAPTURE_RESULT requested_frames={sample_frames:?} raw_frames=0 selection_error={error}"
            );
            cancel_task.abort();
            return Err(error.into());
        }
    }
    cancel_task.abort();
    Ok(())
}

#[cfg(target_os = "linux")]
async fn sample_owned_frames(
    lease: host::PortalCaptureLease,
    count: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use host::linux_frame::{MAX_FRAME_BYTES, PixelFormat};
    use std::{collections::HashSet, time::Duration};
    let capture = match host::prepare_pipewire_capture(lease).await {
        Ok(capture) => capture,
        Err(error) => {
            eprintln!("RAW_CAPTURE_RESULT raw_frames=0 prepare_error={error}");
            return Err(error.into());
        }
    };
    let mut fingerprints = HashSet::new();
    let mut frames = 0usize;
    let mut nonblack_frames = 0usize;
    let mut changes = 0usize;
    let mut previous = None;
    let mut failure = None;
    for _ in 0..count {
        let frame = match capture
            .observe_owned_frame(Duration::from_secs(2), MAX_FRAME_BYTES)
            .await
        {
            Ok(frame) => frame,
            Err(error) => {
                failure = Some(error);
                break;
            }
        };
        let pixels = u64::from(frame.width) * u64::from(frame.height);
        let (nonblack, stride, metric) = match frame.source_format.pixel_format {
            PixelFormat::Bgra | PixelFormat::Bgrx | PixelFormat::Rgba | PixelFormat::Rgbx => (
                frame.planes[0]
                    .chunks_exact(4)
                    .filter(|p| p[0].max(p[1]).max(p[2]) > 8)
                    .count() as u64,
                frame.width * 4,
                "rgb_any_channel_gt_8",
            ),
            PixelFormat::Nv12 | PixelFormat::I420 => (
                frame.planes[0].iter().filter(|&&y| y > 20).count() as u64,
                frame.width,
                "luma_gt_20",
            ),
        };
        let fingerprint = frame
            .planes
            .iter()
            .flatten()
            .fold(0xcbf29ce484222325u64, |hash, &byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            });
        if previous.is_some_and(|old| old != fingerprint) {
            changes += 1;
        }
        previous = Some(fingerprint);
        fingerprints.insert(fingerprint);
        frames += 1;
        nonblack_frames += usize::from(nonblack > 0);
        println!(
            "RAW_FRAME index={frames} width={} height={} format={:?} owned_stride={stride} original_spa_stride=not_retained metric={metric} nonblack_pixels={nonblack} pixels={pixels} bytes={} fingerprint={fingerprint:016x} sequence={:?} pts_ns={:?}",
            frame.width,
            frame.height,
            frame.source_format.pixel_format,
            frame.planes.iter().map(Vec::len).sum::<usize>(),
            frame.stamp.sequence,
            frame.stamp.pipewire_pts_ns
        );
        // Let the visible fixture change; no queue of old frames is retained.
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let closed = capture.close_owned().await;
    println!(
        "RAW_CAPTURE_RESULT requested_frames={count} raw_frames={frames} nonblack_frames={nonblack_frames} changed_frames={changes} unique_fingerprints={} portal_close_ok={} encoder=not_exercised network=not_started pixels_exported=false",
        fingerprints.len(),
        closed.is_ok()
    );
    if let Some(error) = failure {
        return Err(error.into());
    }
    closed?;
    if frames != count {
        return Err("incomplete raw frame observation".into());
    }
    Ok(())
}
