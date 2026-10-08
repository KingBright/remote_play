//! The Linux/Windows HEVC decoder uses one owned FFmpeg child per active stream.
//! Reads/writes are asynchronous, decoded storage is latest-only, and cancellation
//! kills the child. Merely opening the application never starts capture or FFmpeg.
use remote_core::Statistics;
use std::{
    error::Error,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
    task::JoinHandle,
};

pub const MAX_PIXELS: usize = 4096 * 2160;
#[derive(Clone, Debug)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
    #[cfg(all(target_os = "windows", feature = "native-windows-video"))]
    pub native: Option<Arc<crate::windows_playback::NativeFrame>>,
    #[cfg(all(target_os="linux",feature="native-linux-video"))]
    pub native_linux:Option<Arc<crate::linux_playback::NativeFrame>>,
    pub timestamp: u32,
    pub recv_time: u32,
    pub decode_cost_ms: f32,
    pub timing: protocol::FrameTimingCheckpoints,
    pub decoded_at: Instant,
}
impl remote_core::VideoFrame for DecodedFrame {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn handle_kind(&self) -> remote_core::VideoFrameHandleKind {
        #[cfg(all(target_os = "windows", feature = "native-windows-video"))]
        if self.native.is_some() {
            return remote_core::VideoFrameHandleKind::WindowsD3D11Texture;
        }
        #[cfg(all(target_os="linux",feature="native-linux-video"))]
        if self.native_linux.is_some(){return remote_core::VideoFrameHandleKind::LinuxDmaBuf;}
        remote_core::VideoFrameHandleKind::CpuMemory
    }
}

pub fn decoder_program() -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    if let Some(p) = std::env::var_os("REMOTE_PLAY_FFMPEG_BIN") {
        let p = PathBuf::from(p);
        if p.is_absolute() && p.is_file() {
            return Ok(p);
        }
        return Err("REMOTE_PLAY_FFMPEG_BIN must name an existing absolute file".into());
    }
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or("application directory is unavailable")?;
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    for path in [dir.join("bin").join(name), dir.join(name)] {
        if path.is_file() {
            return Ok(path);
        }
    }
    #[cfg(target_os = "linux")]
    if PathBuf::from("/usr/bin/ffmpeg").is_file() {
        return Ok(PathBuf::from("/usr/bin/ffmpeg"));
    }
    Err("HEVC playback runtime is missing. Install the complete RemotePlay bundle (Linux: FFmpeg), not only its executable.".into())
}

async fn header_line<R: AsyncBufRead + Unpin>(input: &mut R) -> Result<Option<String>, String> {
    let mut bytes = Vec::with_capacity(80);
    for _ in 0..512 {
        match input.read_u8().await {
            Ok(b'\n') => {
                return String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|_| "invalid PAM header".into());
            }
            Ok(byte) => bytes.push(byte),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && bytes.is_empty() => {
                return Ok(None);
            }
            Err(e) => return Err(format!("decoder pipe ended: {e}")),
        }
    }
    Err("decoder header line exceeds 512 bytes".into())
}

pub async fn read_rgba_frame<R: AsyncBufRead + Unpin>(
    input: &mut R,
) -> Result<Option<(u32, u32, Vec<u8>)>, String> {
    let Some(magic) = header_line(input).await? else {
        return Ok(None);
    };
    if magic.trim() != "P7" {
        return Err("decoder output is not PAM".into());
    }
    let (mut width, mut height, mut depth, mut maxval) = (None, None, None, None);
    let mut ended = false;
    for _ in 0..16 {
        let line = header_line(input).await?.ok_or("truncated frame header")?;
        if line.trim() == "ENDHDR" {
            ended = true;
            break;
        }
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or("");
        if key.starts_with('#') {
            continue;
        }
        let dest = match key {
            "WIDTH" => &mut width,
            "HEIGHT" => &mut height,
            "DEPTH" => &mut depth,
            "MAXVAL" => &mut maxval,
            _ => continue,
        };
        if dest.is_some() {
            return Err("duplicate decoder frame attribute".into());
        }
        *dest = Some(
            parts
                .next()
                .ok_or("missing frame attribute")?
                .parse::<u32>()
                .map_err(|_| "invalid frame attribute")?,
        );
    }
    let (w, h) = (
        width.ok_or("missing frame width")?,
        height.ok_or("missing frame height")?,
    );
    if !ended || depth != Some(4) || maxval != Some(255) || w == 0 || h == 0 || w > 8192 || h > 8192
    {
        return Err("unsupported decoder frame geometry/format".into());
    }
    let pixels = (w as usize)
        .checked_mul(h as usize)
        .filter(|n| *n <= MAX_PIXELS)
        .ok_or("decoder frame exceeds memory limit")?;
    let mut bytes = vec![0; pixels * 4];
    input
        .read_exact(&mut bytes)
        .await
        .map_err(|e| format!("truncated decoder frame: {e}"))?;
    Ok(Some((w, h, bytes)))
}

struct DecoderPipe {
    child: Child,
    input: ChildStdin,
    reader: JoinHandle<()>,
    errors: JoinHandle<()>,
}
impl Drop for DecoderPipe {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.reader.abort();
        self.errors.abort();
    }
}
impl DecoderPipe {
    fn new(
        shared: Arc<Mutex<Option<DecodedFrame>>>,
        stats: Arc<Statistics>,
        epoch: Arc<AtomicU64>,
        generation: u64,
        status: Arc<Mutex<String>>,
        timestamp: Arc<AtomicU32>,
        frame_signal: Option<Arc<tokio::sync::Notify>>,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(decoder_program().map_err(|e| e.to_string())?);
        cmd.args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-flags",
            "low_delay",
            "-threads",
            "2",
            "-f",
            "hevc",
            "-r",
            "60",
            "-i",
            "pipe:0",
            "-an",
            "-sn",
            "-dn",
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "rgba",
            "-c:v",
            "pam",
            "-threads",
            "1",
            "-f",
            "image2pipe",
            "-flush_packets",
            "1",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000 | 0x0000_4000);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Cannot start HEVC decoder: {e}"))?;
        let input = child.stdin.take().ok_or("decoder stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("decoder stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("decoder stderr unavailable")?;
        let report = status.clone();
        let reader = tokio::spawn(async move {
            let mut input = BufReader::new(stdout);
            loop {
                let start = Instant::now();
                match read_rgba_frame(&mut input).await {
                    Ok(Some((width, height, rgba))) => {
                        if epoch.load(Ordering::Acquire) != generation {
                            break;
                        }
                        let frame = DecodedFrame {
                            width,
                            height,
                            rgba: Arc::new(rgba),
                            #[cfg(all(target_os="linux",feature="native-linux-video"))]
                            native_linux:None,
                            #[cfg(all(target_os = "windows", feature = "native-windows-video"))]
                            native: None,
                            timestamp: timestamp.load(Ordering::Relaxed),
                            recv_time: 0,
                            decode_cost_ms: start.elapsed().as_secs_f32() * 1000.,
                            timing: Default::default(),
                            decoded_at: Instant::now(),
                        };
                        let mut slot = shared.lock().unwrap();
                        if epoch.load(Ordering::Acquire) != generation {
                            break;
                        }
                        stats
                            .video_decoder_needs_keyframe
                            .store(false, Ordering::Release);
                        let previous = slot.replace(frame);
                        drop(slot);
                        drop(previous);
                        if let Some(signal) = &frame_signal {
                            signal.notify_one();
                        }
                        stats.video_frames_decoded.fetch_add(1, Ordering::Relaxed);
                        report.lock().unwrap().clear();
                    }
                    Ok(None) => break,
                    Err(e) => {
                        *report.lock().unwrap() = e;
                        stats.video_decode_errors.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                }
            }
        });
        let errors = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            // Drain without accumulating arbitrary child output in memory.
            loop {
                match header_line(&mut reader).await {
                    Ok(Some(line)) => {
                        if !line.is_empty() {
                            *status.lock().unwrap() = line.chars().take(300).collect();
                        }
                    }
                    _ => break,
                }
            }
        });
        Ok(Self {
            child,
            input,
            reader,
            errors,
        })
    }
}

pub fn spawn_decoder(
    packets: mpsc::Receiver<(protocol::RtpPacket, protocol::FrameTimingCheckpoints)>,
    shared: Arc<Mutex<Option<DecodedFrame>>>,
    stats: Arc<Statistics>,
    epoch: Arc<AtomicU64>,
    status: Arc<Mutex<String>>,
) -> JoinHandle<()> {
    spawn_decoder_with_signal(packets, shared, stats, epoch, status, None)
}

pub fn spawn_decoder_with_signal(
    mut packets: mpsc::Receiver<(protocol::RtpPacket, protocol::FrameTimingCheckpoints)>,
    shared: Arc<Mutex<Option<DecodedFrame>>>,
    stats: Arc<Statistics>,
    epoch: Arc<AtomicU64>,
    status: Arc<Mutex<String>>,
    frame_signal: Option<Arc<tokio::sync::Notify>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut pipe: Option<DecoderPipe> = None;
        let mut generation = epoch.load(Ordering::Acquire);
        let mut previous: Option<(u32, u16)> = None;
        let mut waiting = true;
        stats
            .video_decoder_needs_keyframe
            .store(true, Ordering::Release);
        let timestamp = Arc::new(AtomicU32::new(0));
        while let Some((packet, _timing)) = packets.recv().await {
            let id = (packet.header.ssrc, packet.header.sequence_number);
            let changed = epoch.load(Ordering::Acquire) != generation
                || previous.is_some_and(|p| p.0 != id.0 || p.1.wrapping_add(1) != id.1);
            if changed {
                stats
                    .video_decoder_reference_gaps
                    .fetch_add(1, Ordering::Relaxed);
                stats
                    .video_decoder_needs_keyframe
                    .store(true, Ordering::Release);
                pipe.take();
                waiting = true;
                generation = epoch.load(Ordering::Acquire);
                shared.lock().unwrap().take();
            }
            previous = Some(id);
            let keyframe = remote_core::media_plane::is_hevc_keyframe(&packet.payload);
            if waiting && !keyframe {
                stats
                    .video_decoder_reference_skipped
                    .fetch_add(1, Ordering::Relaxed);
                stats
                    .video_decode_queue_dropped
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if pipe.is_none() {
                match DecoderPipe::new(
                    shared.clone(),
                    stats.clone(),
                    epoch.clone(),
                    generation,
                    status.clone(),
                    timestamp.clone(),
                    frame_signal.clone(),
                ) {
                    Ok(p) => pipe = Some(p),
                    Err(e) => {
                        *status.lock().unwrap() = e;
                        continue;
                    }
                }
            }
            timestamp.store(packet.header.timestamp, Ordering::Relaxed);
            let p = pipe.as_mut().unwrap();
            match tokio::time::timeout(Duration::from_secs(2), p.input.write_all(&packet.payload))
                .await
            {
                Ok(Ok(())) => waiting = false,
                _ => {
                    *status.lock().unwrap() =
                        "HEVC decoder input stalled or closed; waiting for a keyframe".into();
                    pipe.take();
                    waiting = true;
                    stats
                        .video_decoder_needs_keyframe
                        .store(true, Ordering::Release);
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn live_hevc_pipeline_decodes_without_waiting_for_input_eof() {
        if decoder_program().is_err() {
            return;
        }
        let shared = Arc::new(Mutex::new(None));
        let stats = Statistics::new();
        let epoch = Arc::new(AtomicU64::new(1));
        let error = Arc::new(Mutex::new(String::new()));
        let (tx, rx) = mpsc::channel(4);
        let task = spawn_decoder(rx, shared.clone(), stats.clone(), epoch, error.clone());
        let payload = include_bytes!("../tests/fixtures/desktop-test.h265").to_vec();
        assert!(remote_core::media_plane::is_hevc_keyframe(&payload));
        tx.send((
            protocol::RtpPacket {
                header: protocol::RtpHeader {
                    version: 2,
                    payload_type: 96,
                    sequence_number: 1,
                    timestamp: 100,
                    ssrc: 17,
                },
                payload,
            },
            Default::default(),
        ))
        .await
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.lock().unwrap().is_none() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        {
            let frame = shared.lock().unwrap();
            let f = frame
                .as_ref()
                .unwrap_or_else(|| panic!("{}", error.lock().unwrap()));
            assert_eq!((f.width, f.height), (128, 72));
            assert_eq!(f.rgba.len(), 128 * 72 * 4);
        }
        assert!(stats.video_frames_decoded.load(Ordering::Relaxed) > 0);
        task.abort();
        let _ = task.await;
        drop(tx);
    }

    #[tokio::test]
    async fn reads_bounded_dynamic_rgba_frames() {
        let mut bytes =
            b"P7\nWIDTH 2\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n".to_vec();
        bytes.extend_from_slice(&[1, 2, 3, 255, 4, 5, 6, 255]);
        let result = read_rgba_frame(&mut bytes.as_slice())
            .await
            .unwrap()
            .unwrap();
        assert_eq!((result.0, result.1), (2, 1));
        assert_eq!(result.2.len(), 8);
    }
    #[tokio::test]
    async fn refuses_oversize_duplicate_and_truncated_frames() {
        for header in [
            "P7\nWIDTH 9000\nHEIGHT 9000\nDEPTH 4\nMAXVAL 255\nENDHDR\n",
            "P7\nWIDTH 1\nWIDTH 1\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nENDHDR\n",
            "P7\nWIDTH 2\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nENDHDR\n",
        ] {
            assert!(read_rgba_frame(&mut header.as_bytes()).await.is_err());
        }
    }
    #[tokio::test]
    async fn bounds_header_memory_and_accepts_clean_eof() {
        assert!(header_line(&mut vec![b'X'; 513].as_slice()).await.is_err());
        assert!(read_rgba_frame(&mut &b""[..]).await.unwrap().is_none());
    }
}
