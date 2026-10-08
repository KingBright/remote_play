//! Production receive-queue -> native decoder -> bounded GPU-copy -> latest frame.
//! COM and all codec calls stay on one dedicated MTA worker. The GUI and network
//! runtimes never block on that worker. Closing the async owner signals shutdown.
use crate::{
    hevc_sequence::{self, SequenceInfo},
    portable_video::DecodedFrame,
    windows_hevc::{D3dDecodedFrame, WindowsHevcDecoder},
};
use anyhow::{Result, bail, ensure};
use gpui::native_video::{
    ChromaLocation, D3dVideoCopyPool, Matrix, Range, ReadyD3dVideoFrame, Rotation, VideoColor,
    VideoGeometry,
};
use remote_core::Statistics;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Notify, mpsc, oneshot, watch};
type Packet = (protocol::RtpPacket, protocol::FrameTimingCheckpoints);
static LIVE_WORKERS: AtomicU64 = AtomicU64::new(0);
/// Diagnostic observation only; never used to infer that frames were displayed.
pub fn active_workers() -> u64 {
    LIVE_WORKERS.load(Ordering::Acquire)
}

pub struct NativeFrame {
    pub ready: ReadyD3dVideoFrame,
    pub generation: u64,
}
impl std::fmt::Debug for NativeFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeFrame")
            .field("generation", &self.generation)
            .field("tag", &self.ready.tag)
            .field("geometry", &self.ready.frame.geometry())
            .finish()
    }
}
struct Ownership(watch::Sender<bool>);
impl Drop for Ownership {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}
struct WorkerCount;
impl Drop for WorkerCount {
    fn drop(&mut self) {
        LIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Shared {
    frame: Arc<Mutex<Option<DecodedFrame>>>,
    stats: Arc<Statistics>,
    epoch: Arc<AtomicU64>,
    status: Arc<Mutex<String>>,
    updates: Arc<Notify>,
}
struct Metadata {
    tag: u64,
    timestamp: u32,
    timing: protocol::FrameTimingCheckpoints,
    entered: Instant,
}
// One latest decoded surface waiting for a free copy slot. This is not a
// compressed-frame queue, and holding it does not block network/UI execution.
struct PendingCopy {
    frame:Arc<D3dDecodedFrame>,
    geometry:VideoGeometry,
    color:VideoColor,
    lease:Arc<dyn std::any::Any+Send+Sync>,
    tag:u64,
}
struct DecoderState {
    pending_copy:Option<PendingCopy>,
    // Drop the GPU copy pool before the MFT so unfinished source leases enter
    // retirement before the codec is released. No pending frame is CPU-mapped.
    pool: Option<D3dVideoCopyPool>,
    decoder: Option<WindowsHevcDecoder>,
    sequence: Option<SequenceInfo>,
    generation: u64,
    previous: Option<(u32, u16)>,
    waiting: bool,
    next_tag: u64,
    metadata: VecDeque<Metadata>,
}
impl DecoderState {
    fn new(generation: u64) -> Self {
        Self {
            pending_copy:None,
            pool: None,
            decoder: None,
            sequence: None,
            generation,
            previous: None,
            waiting: true,
            next_tag: 0,
            metadata: VecDeque::with_capacity(96),
        }
    }
    fn reset(&mut self, shared: &Shared, generation: u64) {
        self.pending_copy.take();
        self.pool.take();
        self.decoder.take();
        self.sequence = None;
        self.previous = None;
        self.waiting = true;
        self.metadata.clear();
        self.generation = generation;
        shared
            .stats
            .video_decoder_needs_keyframe
            .store(true, Ordering::Release);
        let mut slot = shared.frame.lock().unwrap();
        if generation == shared.epoch.load(Ordering::Acquire) {
            let old = slot.take();
            drop(slot);
            drop(old);
        }
    }
    fn fail(&mut self, shared: &Shared, error: impl std::fmt::Display) {
        self.reset(shared, shared.epoch.load(Ordering::Acquire));
        *shared.status.lock().unwrap() = format!("Native HEVC: {error}");
        shared
            .stats
            .video_decode_errors
            .fetch_add(1, Ordering::Relaxed);
        shared.updates.notify_one();
    }
    fn pending(&self) -> bool {
        self.pending_copy.is_some() || self.pool.as_ref().is_some_and(|p| p.pending_count() > 0)
    }
    fn publish_ready(&mut self, shared: &Shared) -> Result<()> {
        let Some(pool) = self.pool.as_mut() else {
            return Ok(());
        };
        for ready in pool.take_ready()? {
            let Some(index) = self.metadata.iter().position(|m| m.tag == ready.tag) else {
                bail!("native output lost its input timestamp")
            };
            let meta = self.metadata.remove(index).unwrap();
            let g = ready.frame.geometry();
            let mut timing = meta.timing;
            if timing.capture_ts_us > 0 {
                timing.decode_done_ts_us = remote_core::timing::advance_client_stage(
                    timing.decode_enter_ts_us.max(timing.recv_ts_us),
                    meta.entered.elapsed().as_micros().min(u128::from(u32::MAX)) as u32,
                );
            }
            let frame = DecodedFrame {
                width: g.visible[2],
                height: g.visible[3],
                rgba: Arc::new(Vec::new()),
                native: Some(Arc::new(NativeFrame {
                    ready,
                    generation: self.generation,
                })),
                timestamp: meta.timestamp,
                recv_time: timing.recv_ts_us / 1000,
                decode_cost_ms: meta.entered.elapsed().as_secs_f32() * 1000.,
                timing,
                decoded_at: Instant::now(),
            };
            let mut slot = shared.frame.lock().unwrap();
            if self.generation != shared.epoch.load(Ordering::Acquire) {
                drop(slot);
                continue;
            }
            let previous = slot.replace(frame);
            shared
                .stats
                .video_decoder_needs_keyframe
                .store(false, Ordering::Release);
            drop(slot);
            drop(previous);
            shared
                .stats
                .video_frames_decoded
                .fetch_add(1, Ordering::Relaxed);
            shared.status.lock().unwrap().clear();
            shared.updates.notify_one();
        }
        // Free completed leases before retrying the latest decoded output.
        // A transient full GPU pool must not discard the newest frame outright.
        self.try_submit_copy()?;
        Ok(())
    }
    fn try_submit_copy(&mut self)->Result<()> {
        let Some(pending)=self.pending_copy.as_ref() else {return Ok(());};
        let Some(pool)=self.pool.as_mut() else {return Ok(());};
        let accepted=unsafe{pool.submit(pending.frame.texture(),pending.frame.subresource,
            pending.geometry,pending.color,pending.lease.clone(),pending.tag)?};
        if accepted {self.pending_copy.take();}
        Ok(())
    }
    fn input(&mut self, packet: Packet, shared: &Shared, fps: u32) -> Result<()> {
        let (packet, mut timing) = packet;
        let generation = shared.epoch.load(Ordering::Acquire);
        let id = (packet.header.ssrc, packet.header.sequence_number);
        let gap = self
            .previous
            .is_some_and(|p| p.0 != id.0 || p.1.wrapping_add(1) != id.1);
        if generation != self.generation || gap {
            if gap {
                shared
                    .stats
                    .video_decoder_reference_gaps
                    .fetch_add(1, Ordering::Relaxed);
            }
            self.reset(shared, generation);
        }
        self.previous = Some(id);
        let key = remote_core::media_plane::is_hevc_keyframe(&packet.payload);
        if self.waiting && !key {
            shared
                .stats
                .video_decoder_reference_skipped
                .fetch_add(1, Ordering::Relaxed);
            shared
                .stats
                .video_decode_queue_dropped
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        let described =
            hevc_sequence::sequence_info(&packet.payload).map_err(anyhow::Error::msg)?;
        if let Some(s) = described {
            if self.sequence.is_some_and(|old| old != s) {
                self.reset(shared, generation);
                self.previous = Some(id);
            }
            if self.decoder.is_none() {
                ensure!(key, "sequence changes require a random-access frame");
                let decoder = WindowsHevcDecoder::new(s, [fps, 1])?;
                let pool = D3dVideoCopyPool::new(decoder.device().clone())?;
                self.pool = Some(pool);
                self.sequence = Some(s);
                self.decoder = Some(decoder);
            }
        }
        let Some(decoder) = self.decoder.as_mut() else {
            bail!("waiting for the source sequence header before hardware decode")
        };
        ensure!(
            self.metadata.len() < 96,
            "decoder retained too many input timestamps without output"
        );
        self.next_tag = self
            .next_tag
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("frame identity overflow"))?;
        let tag = self
            .next_tag
            .checked_mul(10_000_000 / u64::from(fps))
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or_else(|| anyhow::anyhow!("frame timestamp overflow"))?;
        let entered = Instant::now();
        if timing.capture_ts_us > 0 {
            timing.decode_enter_ts_us = timing.jitter_exit_ts_us.max(timing.recv_ts_us);
        }
        self.metadata.push_back(Metadata {
            tag,
            timestamp: packet.header.timestamp,
            timing,
            entered,
        });
        let frames = decoder.push(&packet.payload, tag as i64, key)?;
        // A valid random-access input starts a decode sequence, but input safety
        // still waits for actual current-generation GPU-ready output publication.
        self.waiting = false;
        for frame in frames {
            // MFTs may deliver a burst after initialization. Reap/submit between
            // outputs instead of filling all slots before checking completion.
            self.publish_ready(shared)?;
            let metadata = resolve_color(&frame)?;
            let geometry = VideoGeometry {
                coded: frame.coded,
                visible: frame.visible,
                pixel_aspect: frame.sample_aspect,
                rotation: Rotation::R0,
            };
            let output_tag = u64::try_from(frame.pts_100ns)?;
            let pending=PendingCopy{lease:frame.retention_lease()?,frame,geometry,color:metadata,tag:output_tag};
            if let Some(older)=self.pending_copy.replace(pending) {
                // Only coalesce an already decoded, not-yet-copied older frame.
                // References in the compressed bitstream are never discarded here.
                if let Some(i)=self.metadata.iter().position(|m|m.tag==older.tag){self.metadata.remove(i);}
                shared.stats.video_decode_queue_dropped.fetch_add(1,Ordering::Relaxed);
            }
            self.try_submit_copy()?;
        }
        self.publish_ready(shared)
    }
}
/// Resolve only supported signal matrices. Explicit source and decoder conflicts
/// are errors; an unspecified primary/transfer is not a false HDR declaration.
fn resolve_color(frame: &D3dDecodedFrame) -> Result<VideoColor> {
    let signal = frame.source_signal;
    if signal
        .and_then(|s| s.transfer)
        .is_some_and(|v| v == 16 || v == 18)
    {
        bail!("HDR requires a negotiated tone/gamut-mapping path")
    }
    let from_source = signal
        .and_then(|s| s.matrix)
        .filter(|v| *v != 2)
        .map(|v| match v {
            1 => Ok(Matrix::Bt709),
            5 | 6 => Ok(Matrix::Bt601),
            _ => Err(anyhow::anyhow!("unsupported signalled YUV matrix {v}")),
        })
        .transpose()?;
    let from_decoder = frame
        .matrix
        .filter(|v| *v != 0)
        .map(|v| match v {
            1 => Ok(Matrix::Bt709),
            2 => Ok(Matrix::Bt601),
            _ => Err(anyhow::anyhow!("unsupported decoder YUV matrix {v}")),
        })
        .transpose()?;
    ensure!(
        from_source.is_none() || from_decoder.is_none() || from_source == from_decoder,
        "decoder/source matrix disagreement"
    );
    let matrix = from_source
        .or(from_decoder)
        .ok_or_else(|| anyhow::anyhow!("source and decoder did not specify a YUV matrix"))?;
    let range = match (
        signal.map(|s| s.full_range),
        frame.nominal_range.filter(|r| *r != 0),
    ) {
        (Some(true), None | Some(1)) | (None, Some(1)) => Range::Full,
        (Some(false), None | Some(2)) | (None, Some(2)) => Range::Limited,
        (None, None) => bail!("source and decoder did not specify video range"),
        _ => bail!("decoder/source video range disagreement"),
    };
    let chroma = match frame.chroma_location.unwrap_or(0) {
        0 => ChromaLocation::Left,
        1 => ChromaLocation::Center,
        _ => bail!("unsupported progressive chroma siting"),
    };
    Ok(VideoColor {
        matrix,
        range,
        chroma,
    })
}

pub fn spawn_decoder(
    packets: mpsc::Receiver<Packet>,
    frame: Arc<Mutex<Option<DecodedFrame>>>,
    stats: Arc<Statistics>,
    epoch: Arc<AtomicU64>,
    status: Arc<Mutex<String>>,
    updates: Arc<Notify>,
    fps: u32,
) -> Result<tokio::task::JoinHandle<()>> {
    ensure!((1..=240).contains(&fps), "invalid native frame rate");
    let shared = Shared {
        frame,
        stats,
        epoch,
        status,
        updates,
    };
    let (cancel, rx) = watch::channel(false);
    let guard = Ownership(cancel);
    let (done_tx, done_rx) = oneshot::channel();
    LIVE_WORKERS.fetch_add(1, Ordering::AcqRel);
    let spawned = std::thread::Builder::new()
        .name("rp-native-hevc".into())
        .spawn(move || {
            let _count = WorkerCount;
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()?;
                    runtime.block_on(run(packets, rx, &shared, fps));
                    Ok(())
                }));
            if !matches!(result, Ok(Ok(()))) {
                *shared.status.lock().unwrap() =
                    "native decoder worker failed; reconnect this video session".into();
                shared
                    .stats
                    .video_decoder_needs_keyframe
                    .store(true, Ordering::Release);
                shared.updates.notify_one();
            }
            let _ = done_tx.send(());
        });
    if let Err(error) = spawned {
        LIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
        return Err(error.into());
    }
    // Aborting the public task drops the guard and wakes the worker's idle select.
    // A native OS call in progress cannot be forcibly interrupted by Tokio.
    Ok(tokio::spawn(async move {
        let _guard = guard;
        let _ = done_rx.await;
    }))
}
async fn run(
    mut packets: mpsc::Receiver<Packet>,
    mut cancel: watch::Receiver<bool>,
    shared: &Shared,
    fps: u32,
) {
    let mut decoder = DecoderState::new(shared.epoch.load(Ordering::Acquire));
    shared
        .stats
        .video_decoder_needs_keyframe
        .store(true, Ordering::Release);
    loop {
        if *cancel.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _=cancel.changed()=>break,
            packet=packets.recv()=>{
                let Some(packet)=packet else{break};
                if let Err(error)=decoder.input(packet,shared,fps){decoder.fail(shared,error);}
            },
            _=tokio::time::sleep(Duration::from_millis(1)),if decoder.pending()=>{
                if decoder.generation!=shared.epoch.load(Ordering::Acquire){decoder.reset(shared,shared.epoch.load(Ordering::Acquire));}
                else if let Err(error)=decoder.publish_ready(shared){decoder.fail(shared,error);}
            }
        }
    }
    decoder.pending_copy.take();
    decoder.pool.take();
    decoder.decoder.take();
}
