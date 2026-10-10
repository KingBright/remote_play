//! Owned YUV input to the existing FFmpeg/libx265 Annex-B media chain.
//! No screen capture process, inferred color matrix, or compressed-frame loss.

use crate::linux_frame::{FrameFormat, FrameStamp, OwnedFrame, PixelFormat, SourceColor};
use std::error::Error;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, mpsc as sync_mpsc};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const MAX_AU_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RawSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}
impl RawSettings {
    pub(crate) fn validate_for(self, source_width: u32, source_height: u32) -> Result<()> {
        protocol::validate_video_settings(self.width, self.height, self.fps, self.bitrate_kbps)?;
        if source_width == 0 || source_height == 0 || self.fps > 120 {
            return Err("unsupported native encoder dimensions/frame rate".into());
        }
        let scale = (f64::from(self.width) / f64::from(source_width))
            .min(f64::from(self.height) / f64::from(source_height))
            .min(1.0);
        if f64::from(source_width) * scale < 16.0 || f64::from(source_height) * scale < 16.0 {
            return Err("native libx265 output must retain at least 16 pixels on each axis".into());
        }
        Ok(())
    }
}

pub struct RawChunk {
    pub nalu: Vec<u8>,
    pub is_keyframe: bool,
    pub stamp: FrameStamp,
    pub encode_done_ts_us: u64,
}

#[derive(Default)]
struct InputState {
    latest: Option<OwnedFrame>,
    closed: bool,
    finishing: bool,
}
#[derive(Default)]
struct Input {
    state: Mutex<InputState>,
    ready: Condvar,
}
impl Input {
    fn submit(&self, frame: OwnedFrame) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed || state.finishing {
            return Err("native encoder input is closed".into());
        }
        // Only the not-yet-submitted raw frame is replaced. Writer-owned frames
        // and every compressed AU retain their order under backpressure.
        state.latest = Some(frame);
        self.ready.notify_one();
        Ok(())
    }
    fn receive(&self) -> Option<OwnedFrame> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if state.closed {
                return None;
            }
            if let Some(frame) = state.latest.take() {
                // Drop only pending raw pixels, never compressed/reference AUs.
                // First handoff and a later publish cannot renew capture age.
                if frame.stamp.format_revision != 0
                    && !frame.initial_snapshot
                    && frame.is_expired(std::time::Duration::from_millis(250))
                {
                    continue;
                }
                return Some(frame);
            }
            if state.finishing {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        state.latest = None;
        self.ready.notify_all();
    }
}

pub struct RawHevcSource {
    input: Arc<Input>,
    child: Child,
    output: AsyncMutex<mpsc::Receiver<std::result::Result<RawChunk, String>>>,
    writer: Option<std::thread::JoinHandle<()>>,
    reader: Option<std::thread::JoinHandle<()>>,
    format: FrameFormat,
    width: u32,
    height: u32,
    generation: u64,
    format_revision: u64,
}
impl RawHevcSource {
    pub fn start(first: &OwnedFrame, settings: RawSettings) -> Result<Self> {
        first.validate_owned()?;
        let mut command = raw_command(first, settings)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command
            .spawn()
            .map_err(|_| "native HEVC encoder could not start; FFmpeg with libx265 is required")?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or("native encoder stdin is unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("native encoder stdout is unavailable")?;
        let input = Arc::new(Input::default());
        let (stamps_tx, stamps_rx) = sync_mpsc::sync_channel::<FrameStamp>(4);
        let (output_tx, mut output_rx) = mpsc::channel(4);
        let writer_input = input.clone();
        let writer_errors = output_tx.clone();
        let writer = match std::thread::Builder::new()
            .name("rp-native-hevc-writer".into())
            .spawn(move || {
                while let Some(frame) = writer_input.receive() {
                    // Register timing before any bytes can produce an AU.
                    if stamps_tx.send(frame.stamp).is_err() {
                        break;
                    }
                    for plane in &frame.planes {
                        if stdin.write_all(plane).is_err() {
                            let _ = writer_errors
                                .blocking_send(Err("native HEVC raw write failed".into()));
                            writer_input.close();
                            return;
                        }
                    }
                }
            }) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        let reader_input = input.clone();
        let reader = match std::thread::Builder::new()
            .name("rp-native-hevc-reader".into())
            .spawn(move || {
                drain_reader(stdout, stamps_rx, output_tx);
                reader_input.close();
            }) {
            Ok(reader) => reader,
            Err(error) => {
                input.close();
                output_rx.close();
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                return Err(error.into());
            }
        };
        Ok(Self {
            input,
            child,
            output: AsyncMutex::new(output_rx),
            writer: Some(writer),
            reader: Some(reader),
            format: first.source_format,
            width: first.width,
            height: first.height,
            generation: first.stamp.generation,
            format_revision: first.stamp.format_revision,
        })
    }
    pub fn matches(&self, frame: &OwnedFrame) -> bool {
        self.generation == frame.stamp.generation
            && self.format_revision == frame.stamp.format_revision
            && self.format == frame.source_format
            && (self.width, self.height) == (frame.width, frame.height)
    }
    pub fn submit(&self, frame: OwnedFrame) -> Result<()> {
        frame.validate_owned()?;
        if !self.matches(&frame) {
            return Err("native encoder format/generation mismatch".into());
        }
        self.input.submit(frame)
    }
    pub async fn pull(&self) -> Result<RawChunk> {
        match self.output.lock().await.recv().await {
            Some(Ok(chunk)) => Ok(chunk),
            Some(Err(error)) => Err(error.into()),
            None => Err("native HEVC encoder ended".into()),
        }
    }
    #[cfg(test)]
    pub fn finish_input(&self) {
        self.input.close_after_latest();
    }
}

impl Input {
    #[cfg(test)]
    fn close_after_latest(&self) {
        // Used only for bounded fixture streams. Production cancellation uses
        // close(), which clears queued raw input before process teardown.
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.finishing = true;
        self.ready.notify_all();
    }
}

impl Drop for RawHevcSource {
    fn drop(&mut self) {
        self.input.close();
        // Release a reader blocked on a full compressed queue before kill/join.
        self.output.get_mut().close();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

// SPA and AV enums are different. Exact SDR mappings from their public headers;
// unknown stays AV_UNSPECIFIED. Unmapped/HDR values fail rather than guess.
fn color_values(color: SourceColor) -> Result<(u32, u32, u32, u32)> {
    let range = match color.range {
        0 => {
            return Err("unknown SPA color range cannot be preserved by FFmpeg/libx265 VUI".into());
        }
        1 => 2,
        2 => 1,
        _ => return Err("unsupported SPA color range".into()),
    };
    let matrix = match color.matrix {
        0 => 2,
        2 => 4,
        3 => 1,
        4 => 6,
        5 => 7,
        6 => 9,
        _ => return Err("unsupported SPA YUV matrix".into()),
    };
    let transfer = match color.transfer {
        0 => 2,
        1 => 8,
        4 => 4,
        5 => 1,
        6 => 7,
        7 => 13,
        8 => 5,
        9 => 9,
        10 => 10,
        11 => 15,
        13 => 14,
        16 => 6,
        _ => return Err("unsupported or HDR SPA transfer function".into()),
    };
    let primaries = match color.primaries {
        0 => 2,
        1 => 1,
        2 => 4,
        3 => 5,
        4 => 6,
        5 => 7,
        6 => 8,
        7 => 9,
        9 => 10,
        10 => 11,
        11 => 12,
        12 => 22,
        _ => return Err("unsupported SPA color primaries".into()),
    };
    Ok((range, matrix, transfer, primaries))
}

pub(crate) fn validate_native_input(frame: &OwnedFrame) -> Result<()> {
    frame.validate_owned()?;
    if !matches!(
        frame.source_format.pixel_format,
        PixelFormat::Nv12 | PixelFormat::I420
    ) {
        return Err(
            "native HEVC RGB conversion is not yet validated; refusing an inferred YUV matrix"
                .into(),
        );
    }
    color_values(frame.source_format.color)?;
    Ok(())
}

fn raw_command(frame: &OwnedFrame, settings: RawSettings) -> Result<Command> {
    validate_native_input(frame)?;
    settings.validate_for(frame.width, frame.height)?;
    let pixel =
        match frame.source_format.pixel_format {
            PixelFormat::Nv12 => "nv12",
            PixelFormat::I420 => "yuv420p",
            _ => return Err(
                "native HEVC RGB conversion is not yet validated; refusing an inferred YUV matrix"
                    .into(),
            ),
        };
    let (range, matrix, transfer, primaries) = color_values(frame.source_format.color)?;
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-probesize", "32", "-analyzeduration", "0",
        "-f", "rawvideo", "-pixel_format", pixel, "-video_size", &format!("{}x{}", frame.width, frame.height),
        "-framerate", &settings.fps.to_string(), "-color_range", &range.to_string(),
        "-colorspace", &matrix.to_string(), "-color_trc", &transfer.to_string(), "-color_primaries", &primaries.to_string(),
        "-i", "pipe:0", "-an", "-filter_threads", "1", "-threads", "2",
        "-vf", &format!("scale=w={}:h={}:force_original_aspect_ratio=decrease:force_divisible_by=2", settings.width.min(frame.width), settings.height.min(frame.height)),
        "-fps_mode", "passthrough", "-pix_fmt", "yuv420p", "-c:v", "libx265", "-preset", "ultrafast", "-tune", "zerolatency",
        "-x265-params", &format!("keyint={}:min-keyint={}:repeat-headers=1:bframes=0:rc-lookahead=0:aud=1:pools=2:frame-threads=1:log-level=error:colorprim={primaries}:transfer={transfer}:colormatrix={matrix}", settings.fps, settings.fps),
        "-color_range", &range.to_string(), "-colorspace", &matrix.to_string(), "-color_trc", &transfer.to_string(), "-color_primaries", &primaries.to_string(),
        "-b:v", &format!("{}k", settings.bitrate_kbps), "-maxrate", &format!("{}k", settings.bitrate_kbps), "-bufsize", &format!("{}k", settings.bitrate_kbps),
        "-f", "hevc", "-flush_packets", "1", "pipe:1"]);
    Ok(command)
}

fn starts(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let length = if bytes[index..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[index..].starts_with(&[0, 0, 1]) {
            3
        } else {
            index += 1;
            continue;
        };
        result.push((index, length));
        index += length;
    }
    result
}
fn nal_type(bytes: &[u8], start: usize, prefix: usize) -> Option<u8> {
    bytes.get(start + prefix).map(|byte| (byte >> 1) & 0x3f)
}
fn take_au(bytes: &mut Vec<u8>, eof: bool) -> std::result::Result<Option<(Vec<u8>, bool)>, String> {
    if bytes.len() > MAX_AU_BYTES {
        return Err("native HEVC access unit exceeds bounded parser budget".into());
    }
    let mut vcl = false;
    let mut key = false;
    for (start, prefix) in starts(bytes) {
        let Some(kind) = nal_type(bytes, start, prefix) else {
            break;
        };
        let first_slice = kind <= 31
            && bytes
                .get(start + prefix + 2)
                .is_some_and(|byte| byte & 0x80 != 0);
        if vcl && (matches!(kind, 32..=35 | 39) || first_slice) {
            return Ok(Some((bytes.drain(..start).collect(), key)));
        }
        if kind <= 31 {
            vcl = true;
            key |= matches!(kind, 16..=21);
        }
    }
    if eof && vcl {
        return Ok(Some((std::mem::take(bytes), key)));
    }
    Ok(None)
}
fn drain_reader(
    mut stdout: impl Read,
    stamps: sync_mpsc::Receiver<FrameStamp>,
    output: mpsc::Sender<std::result::Result<RawChunk, String>>,
) {
    let mut bytes = Vec::new();
    let mut read = [0u8; 32_768];
    let mut eof = false;
    loop {
        match take_au(&mut bytes, eof) {
            Ok(Some((nalu, is_keyframe))) => {
                let Ok(stamp) = stamps.recv() else {
                    let _ =
                        output.blocking_send(Err("native HEVC timing association ended".into()));
                    return;
                };
                let chunk = RawChunk {
                    nalu,
                    is_keyframe,
                    stamp,
                    encode_done_ts_us: remote_core::timing::quanta_now_us(),
                };
                if output.blocking_send(Ok(chunk)).is_err() {
                    return;
                }
                continue;
            }
            Err(error) => {
                let _ = output.blocking_send(Err(error));
                return;
            }
            Ok(None) if eof => return,
            Ok(None) => {}
        }
        match stdout.read(&mut read) {
            Ok(0) => eof = true,
            Ok(length) => bytes.extend_from_slice(&read[..length]),
            Err(_) => {
                let _ = output.blocking_send(Err("native HEVC output read failed".into()));
                return;
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
