//! RTP queue -> dedicated VAAPI decoder -> latest immutable DMA-BUF frame.
//! Uses the same source-generation and keyframe recovery rules as other native
//! players. A blocked native codec/export does not block GUI/network executors.
use crate::{
    linux_hevc::{LinuxHevcDecoder, render_node},
    portable_video::DecodedFrame,
};
use anyhow::{Result, ensure};
use gpui::native_video::{DmaVideoFrame, VideoSurfaceStats};
use remote_core::Statistics;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use tokio::sync::{Notify, mpsc, oneshot, watch};
type Packet = (protocol::RtpPacket, protocol::FrameTimingCheckpoints);
static LIVE_WORKERS: AtomicU64 = AtomicU64::new(0);
pub fn active_workers() -> u64 {
    LIVE_WORKERS.load(Ordering::Acquire)
}
#[derive(Clone, Debug)]
pub struct NativeFrame {
    pub frame: DmaVideoFrame,
    pub generation: u64,
    pub tag: u64,
}
struct Shared {
    frame: Arc<Mutex<Option<DecodedFrame>>>,
    stats: Arc<Statistics>,
    epoch: Arc<AtomicU64>,
    status: Arc<Mutex<String>>,
    updates: Arc<Notify>,
    surfaces: Arc<VideoSurfaceStats>,
}
struct Metadata {
    tag: u64,
    timestamp: u32,
    timing: protocol::FrameTimingCheckpoints,
    entered: Instant,
}
struct DecoderState {
    decoder: Option<LinuxHevcDecoder>,
    generation: u64,
    previous: Option<(u32, u16)>,
    waiting: bool,
    tag: u64,
    metadata: VecDeque<Metadata>,
}
impl DecoderState {
    fn new(generation: u64) -> Self {
        Self {
            decoder: None,
            generation,
            previous: None,
            waiting: true,
            tag: 0,
            metadata: VecDeque::with_capacity(128),
        }
    }
    fn reset(&mut self, s: &Shared, generation: u64) {
        self.decoder.take();
        self.metadata.clear();
        self.previous = None;
        self.waiting = true;
        self.generation = generation;
        s.stats
            .video_decoder_needs_keyframe
            .store(true, Ordering::Release);
    }
    fn fail(&mut self, s: &Shared, error: anyhow::Error) {
        self.reset(s, s.epoch.load(Ordering::Acquire));
        *s.status.lock().unwrap() = error.to_string();
        s.stats.video_decode_errors.fetch_add(1, Ordering::Relaxed);
        s.updates.notify_one();
    }
    fn output(&mut self, s: &Shared) -> Result<()> {
        loop {
            let Some(decoded) = self.decoder.as_mut().unwrap().receive()? else {
                return Ok(());
            };
            if self.generation != s.epoch.load(Ordering::Acquire) {
                drop(decoded);
                return Ok(());
            }
            let Some(index) = self.metadata.iter().position(|m| m.tag == decoded.tag) else {
                continue;
            };
            let metadata = self.metadata.remove(index).unwrap();
            let mut timing = metadata.timing;
            let cost = metadata.entered.elapsed();
            if timing.capture_ts_us>0 {
                timing.decode_done_ts_us=remote_core::timing::advance_client_stage(
                    timing.decode_enter_ts_us.max(timing.recv_ts_us),
                    cost.as_micros().min(u128::from(u32::MAX)) as u32);
            }
            let geometry = decoded.frame.geometry();
            let frame = DecodedFrame {
                width: geometry.visible[2],
                height: geometry.visible[3],
                rgba: Arc::new(Vec::new()),
                native_linux: Some(Arc::new(NativeFrame {
                    frame: decoded.frame,
                    generation: self.generation,
                    tag: decoded.tag,
                })),
                timestamp: metadata.timestamp,
                recv_time: 0,
                decode_cost_ms: cost.as_secs_f32() * 1000.,
                timing,
                decoded_at: Instant::now(),
            };
            let old = {
                let mut slot = s.frame.lock().unwrap();
                if self.generation != s.epoch.load(Ordering::Acquire) {
                    drop(slot);
                    drop(frame);
                    continue;
                }
                s.stats
                    .video_decoder_needs_keyframe
                    .store(false, Ordering::Release);
                slot.replace(frame)
            };
            drop(old);
            self.waiting = false;
            s.stats.video_frames_decoded.fetch_add(1, Ordering::Relaxed);
            s.status.lock().unwrap().clear();
            s.updates.notify_one();
        }
    }
    fn input(&mut self, (packet, mut timing): Packet, s: &Shared) -> Result<()> {
        let epoch = s.epoch.load(Ordering::Acquire);
        if epoch != self.generation {
            self.reset(s, epoch);
        }
        let now = (packet.header.ssrc, packet.header.sequence_number);
        if self
            .previous
            .is_some_and(|(ssrc, seq)| ssrc != now.0 || seq.wrapping_add(1) != now.1)
        {
            s.stats
                .video_decoder_reference_gaps
                .fetch_add(1, Ordering::Relaxed);
            self.reset(s, epoch);
        }
        self.previous = Some(now);
        let key = remote_core::media_plane::is_hevc_keyframe(&packet.payload);
        if self.waiting && !key {
            s.stats
                .video_decoder_reference_skipped
                .fetch_add(1, Ordering::Relaxed);
            s.stats
                .video_decode_queue_dropped
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        ensure!(
            !packet.payload.is_empty() && packet.payload.len() <= 8 * 1024 * 1024,
            "invalid native compressed video frame length"
        );
        if self.decoder.is_none() {
            self.decoder = Some(LinuxHevcDecoder::new(&render_node()?, s.surfaces.clone())?);
        }
        ensure!(
            self.metadata.len() < 128,
            "native decoder delayed too many outputs"
        );
        self.tag = self
            .tag
            .checked_add(1)
            .filter(|n| *n < i64::MAX as u64)
            .ok_or_else(|| anyhow::anyhow!("native frame tag exhausted"))?;
        if timing.capture_ts_us > 0 {
            timing.decode_enter_ts_us = timing.jitter_exit_ts_us.max(timing.recv_ts_us);
        }
        self.metadata.push_back(Metadata {
            tag: self.tag,
            timestamp: packet.header.timestamp,
            timing,
            entered: Instant::now(),
        });
        if !self
            .decoder
            .as_mut()
            .unwrap()
            .submit(&packet.payload, self.tag)?
        {
            self.output(s)?;
            ensure!(
                self.decoder
                    .as_mut()
                    .unwrap()
                    .submit(&packet.payload, self.tag)?,
                "native decoder input did not resume after output drain"
            );
        }
        self.output(s)?;
        // Valid compressed keyframe accepted, but global readiness stays false until
        // an actual output is published. Subsequent dependencies must still be decoded.
        if key {
            self.waiting = false;
        }
        Ok(())
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
    render_node()?;
    let shared = Shared {
        frame,
        stats,
        epoch,
        status,
        updates,
        surfaces: Arc::new(VideoSurfaceStats::default()),
    };
    let (cancel, rx) = watch::channel(false);
    let ownership = Ownership(cancel);
    let (done_tx, done_rx) = oneshot::channel();
    LIVE_WORKERS.fetch_add(1, Ordering::AcqRel);
    let spawned = std::thread::Builder::new()
        .name("rp-vaapi-native".into())
        .spawn(move || {
            let _count = WorkerCount;
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()?;
                    runtime.block_on(run(packets, rx, &shared));
                    Ok(())
                }));
            if !matches!(result, Ok(Ok(()))) {
                *shared.status.lock().unwrap() =
                    "native Linux decoder worker failed; reconnect the stream".into();
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
    Ok(tokio::spawn(async move {
        let _ownership = ownership;
        let _ = done_rx.await;
    }))
}
async fn run(mut packets: mpsc::Receiver<Packet>, mut cancel: watch::Receiver<bool>, s: &Shared) {
    let mut decoder = DecoderState::new(s.epoch.load(Ordering::Acquire));
    s.stats
        .video_decoder_needs_keyframe
        .store(true, Ordering::Release);
    loop {
        if *cancel.borrow() {
            break;
        }
        tokio::select! {biased;_ = cancel.changed()=>break,packet=packets.recv()=>{let Some(packet)=packet else{break;};if let Err(error)=decoder.input(packet,s){decoder.fail(s,error);}}}
    }
    drop(decoder);
}
