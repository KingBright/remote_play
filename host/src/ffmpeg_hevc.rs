use std::error::Error;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

/// Long-running ffmpeg capture → HEVC Annex-B pipeline used by Linux and Windows hosts.
/// Stdout is drained on a dedicated bounded reader so cancellation never waits on a
/// blocking pipe read and an overloaded viewer cannot grow memory without bound.
pub struct FfmpegHevcSource {
    child: Mutex<Option<Child>>,
    access_units: AsyncMutex<mpsc::Receiver<Result<(Vec<u8>, bool), String>>>,
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// Drop the old OS capture process before creating its replacement. On failure
/// the slot stays empty rather than keeping a stale encoder with new settings.
pub(crate) fn replace_capture_source<T, E>(
    slot: &mut Option<T>,
    create: impl FnOnce() -> Result<T, E>,
) -> Result<(), E> {
    drop(slot.take());
    *slot = Some(create()?);
    Ok(())
}

impl FfmpegHevcSource {
    pub fn start(
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut command = capture_command(width, height, fps, bitrate_kbps)?;
        let backend = crate::capture_backend::ffmpeg_input_format(&command);
        // Keep startup/codec failures in the host's existing diagnostic log.
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().map_err(|err| {
            format!("ffmpeg HEVC pipeline failed to start ({err}). Install ffmpeg with libx265.")
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or("ffmpeg stdout was not captured")?;
        let (access_tx, access_rx) = mpsc::channel(4);
        let reader = match std::thread::Builder::new()
            .name("remoteplay-ffmpeg-reader".into())
            .spawn(move || drain_hevc_reader(stdout, access_tx))
        {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("failed to start FFmpeg reader thread: {error}").into());
            }
        };
        println!(
            "[FfmpegHevc] started with output bounds {width}x{height}@{fps} {bitrate_kbps} kbps ({})",
            backend
        );
        Ok(Self {
            child: Mutex::new(Some(child)),
            access_units: AsyncMutex::new(access_rx),
            reader: Mutex::new(Some(reader)),
            width,
            height,
            fps,
        })
    }

    pub async fn pull_access_unit(&self) -> Result<(Vec<u8>, bool), Box<dyn Error + Send + Sync>> {
        let mut receiver = self.access_units.lock().await;
        match receiver.recv().await {
            Some(Ok(packet)) => Ok(packet),
            Some(Err(error)) => Err(error.into()),
            None => Err("ffmpeg HEVC pipeline ended".into()),
        }
    }
}

fn drain_hevc_reader<R: Read>(
    mut stdout: R,
    access_tx: mpsc::Sender<Result<(Vec<u8>, bool), String>>,
) {
    use tokio::sync::mpsc::error::TrySendError;

    let mut leftover = Vec::new();
    let mut buf = [0u8; 32_768];
    loop {
        while let Some(au) = take_access_unit(&mut leftover) {
            let packet = Ok((au.clone(), is_hevc_keyframe(&au)));
            match access_tx.try_send(packet) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Closed(_)) => return,
            }
        }

        match stdout.read(&mut buf) {
            Ok(0) => {
                let _ = access_tx.try_send(Err("ffmpeg HEVC pipeline ended".into()));
                return;
            }
            Ok(read) => leftover.extend_from_slice(&buf[..read]),
            Err(error) => {
                let _ = access_tx.try_send(Err(format!("ffmpeg HEVC read failed: {error}")));
                return;
            }
        }
    }
}

impl Drop for FfmpegHevcSource {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock()
            && let Some(mut child) = child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Ok(mut reader) = self.reader.lock()
            && let Some(reader) = reader.take()
        {
            let _ = reader.join();
        }
    }
}

fn capture_command(
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
) -> Result<Command, Box<dyn Error + Send + Sync>> {
    #[cfg(target_os = "windows")]
    let program = crate::capture_readiness::ffmpeg_program()?;
    #[cfg(not(target_os = "windows"))]
    let program = std::path::PathBuf::from("ffmpeg");
    capture_command_using(&program, width, height, fps, bitrate_kbps)
}

fn capture_command_using(
    program: &std::path::Path,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
) -> Result<Command, Box<dyn Error + Send + Sync>> {
    let mut cmd = Command::new(program);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
    #[cfg(target_os = "windows")]
    let capture_rect = crate::capture_readiness::capture_display_rect()?;
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        let capture_size = format!("{}x{}", capture_rect.width, capture_rect.height);
        let offset_x = capture_rect.x.to_string();
        let offset_y = capture_rect.y.to_string();
        cmd.args([
            "-f",
            "gdigrab",
            "-framerate",
            &fps.to_string(),
            "-video_size",
            &capture_size,
            "-offset_x",
            &offset_x,
            "-offset_y",
            &offset_y,
            "-i",
            "desktop",
        ]);
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(display) = std::env::var("DISPLAY") {
            let region = crate::linux_capture_geometry::capture_region(&display)?;
            let plan = crate::linux_capture_geometry::X11CapturePlan::new(region, width, height)?;
            plan.append_args(&mut cmd, &display, fps);
            println!(
                "[FfmpegHevc] X11 compatibility plan: root source {}x{} at +{},{}; encoded {}x{} within requested {}x{}",
                region.width,
                region.height,
                region.x,
                region.y,
                plan.output_width,
                plan.output_height,
                width,
                height,
            );
        } else {
            cmd.args([
                "-f",
                "kmsgrab",
                "-framerate",
                &fps.to_string(),
                "-i",
                "/dev/dri/card0",
            ]);
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = (width, height, fps);
        return Err("ffmpeg HEVC capture is only implemented for Linux and Windows".into());
    }
    #[cfg(target_os = "windows")]
    {
        // Capture only the primary physical desktop rectangle. The encoded output
        // may be smaller, but RemotePlay never changes display modes or touches a
        // virtual display owned by another remote desktop application.
        let target_width = width.min(capture_rect.width).max(2);
        let target_height = height.min(capture_rect.height).max(2);
        cmd.args([
            "-vf",
            &format!(
                "scale=w={target_width}:h={target_height}:force_original_aspect_ratio=decrease:force_divisible_by=2"
            ),
            "-filter_threads",
            "1",
            "-threads",
            "2",
        ]);
    }
    let bitrate = format!("{bitrate_kbps}k");
    let keyint = fps.max(1).to_string();
    let mut encoder_options =
        format!("keyint={keyint}:min-keyint={keyint}:repeat-headers=1:bframes=0");
    #[cfg(target_os = "windows")]
    encoder_options.push_str(":pools=2:frame-threads=1");
    cmd.args([
        "-pix_fmt",
        "yuv420p",
        "-c:v",
        "libx265",
        "-preset",
        "ultrafast",
        "-tune",
        "zerolatency",
        "-x265-params",
        &encoder_options,
        "-b:v",
        &bitrate,
        "-maxrate",
        &bitrate,
        "-bufsize",
        &bitrate,
        "-f",
        "hevc",
        "pipe:1",
    ]);
    Ok(cmd)
}

fn start_code_len(data: &[u8]) -> usize {
    if data.len() >= 4 && data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 1 {
        4
    } else if data.len() >= 3 && data[0] == 0 && data[1] == 0 && data[2] == 1 {
        3
    } else {
        0
    }
}

fn find_start(data: &[u8], from: usize) -> Option<usize> {
    let slice = data.get(from..)?;
    (0..slice.len())
        .find_map(|offset| (start_code_len(&slice[offset..]) > 0).then_some(from + offset))
}

fn hevc_nal_type(nal: &[u8]) -> Option<u8> {
    let prefix = start_code_len(nal);
    nal.get(prefix).map(|byte| (byte >> 1) & 0x3f)
}

fn is_hevc_vcl(nal_type: u8) -> bool {
    nal_type <= 31
}

fn is_hevc_keyframe(au: &[u8]) -> bool {
    let mut offset = 0;
    while offset < au.len() {
        let Some(next) = find_start(au, offset + 1) else {
            return matches!(hevc_nal_type(&au[offset..]), Some(16..=21));
        };
        if matches!(hevc_nal_type(&au[offset..next]), Some(16..=21)) {
            return true;
        }
        offset = next;
    }
    false
}

fn take_access_unit(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let first = find_start(buffer, 0)?;
    if first > 0 {
        buffer.drain(..first);
    }
    let mut offset = 0;
    let mut saw_vcl = false;
    loop {
        let Some(start) = find_start(buffer, offset) else {
            return None;
        };
        let Some(next) = find_start(buffer, start + 3) else {
            return None;
        };
        let nal_type = hevc_nal_type(&buffer[start..next]).unwrap_or(0);
        if is_hevc_vcl(nal_type) {
            if saw_vcl {
                let au = buffer.drain(..start).collect();
                return Some(au);
            }
            saw_vcl = true;
        }
        offset = next;
        if offset > 2 * 1024 * 1024 {
            let au = buffer.drain(..next).collect();
            return Some(au);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires DISPLAY and an independently observed RP_X11_EXPECTED_ROOT; metadata only"]
    fn linux_x11_command_reads_root_metadata_without_capturing() {
        let display = std::env::var("DISPLAY").expect("explicit X11 display required");
        let expected = std::env::var("RP_X11_EXPECTED_ROOT")
            .expect("independently observed root geometry required");
        let (width, height) = expected.split_once('x').expect("expected WIDTHxHEIGHT");
        let expected = (
            width.parse::<u32>().unwrap(),
            height.parse::<u32>().unwrap(),
        );
        let source = crate::linux_capture_geometry::capture_region(&display).unwrap();
        assert_eq!((source.x, source.y), (0, 0));
        assert_eq!((source.width, source.height), expected);
        let plan = crate::linux_capture_geometry::X11CapturePlan::new(source, 1920, 1080).unwrap();

        // Exercise the real Linux command constructor, never Command::spawn.
        let command = capture_command(1920, 1080, 60, 6000).unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        let input_size = format!("{}x{}", source.width, source.height);
        let input = format!("{display}+0,0");
        let filter = format!("scale=w={}:h={}", plan.output_width, plan.output_height);
        for (flag, value) in [
            ("-video_size", input_size.as_str()),
            ("-i", input.as_str()),
            ("-vf", filter.as_str()),
            ("-c:v", "libx265"),
            ("-pix_fmt", "yuv420p"),
        ] {
            assert!(args.windows(2).any(|pair| pair == [flag, value]));
        }
        assert_eq!(
            crate::capture_backend::ffmpeg_input_format(&command),
            "x11grab"
        );
        println!("metadata-only Linux command verified: {command:?}");
    }

    #[test]
    fn replacement_drops_previous_capture_before_factory_runs() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Capture(Arc<AtomicUsize>);
        impl Drop for Capture {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let live = Arc::new(AtomicUsize::new(1));
        let mut slot = Some(Capture(live.clone()));
        replace_capture_source(&mut slot, || {
            assert_eq!(live.load(Ordering::SeqCst), 0);
            live.fetch_add(1, Ordering::SeqCst);
            Ok::<_, ()>(Capture(live.clone()))
        })
        .unwrap();
        assert_eq!(live.load(Ordering::SeqCst), 1);
        drop(slot);
        assert_eq!(live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_replacement_leaves_no_stale_capture() {
        let mut slot = Some(42u32);
        let error = replace_capture_source(&mut slot, || Err::<u32, _>("startup failed"));
        assert_eq!(error, Err("startup failed"));
        assert!(slot.is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_capture_command_bounds_output_and_worker_threads_without_launching() {
        let command = capture_command_using(
            std::path::Path::new("fixture-ffmpeg.exe"),
            1280,
            720,
            30,
            4000,
        )
        .unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|value| value == "-nostdin"));
        let display = crate::capture_readiness::capture_display_rect().unwrap();
        let display_size = format!("{}x{}", display.width, display.height);
        let offset_x = display.x.to_string();
        let offset_y = display.y.to_string();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-video_size", display_size.as_str()])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-offset_x", offset_x.as_str()])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-offset_y", offset_y.as_str()])
        );
        assert!(args.iter().any(|value| value
            == "scale=w=1280:h=720:force_original_aspect_ratio=decrease:force_divisible_by=2"));
        assert!(args.windows(2).any(|pair| pair == ["-threads", "2"]));
        assert!(
            args.iter()
                .any(|value| value.contains("pools=2:frame-threads=1"))
        );
        assert_eq!(command.get_program(), "fixture-ffmpeg.exe");
    }

    #[tokio::test]
    async fn bounded_reader_drops_overload_without_blocking() {
        let mut input = Vec::new();
        for byte in 0u8..20 {
            input.extend_from_slice(&[0, 0, 0, 1, 0x02, byte]);
        }
        input.extend_from_slice(&[0, 0, 0, 1, 0x02, 0xff]);
        let (tx, mut rx) = mpsc::channel(2);
        let reader = std::thread::spawn(move || {
            drain_hevc_reader(std::io::Cursor::new(input), tx);
        });
        reader.join().unwrap();

        let mut delivered = 0;
        while let Some(packet) = rx.recv().await {
            assert!(packet.is_ok());
            delivered += 1;
        }
        assert_eq!(delivered, 2);
    }

    #[test]
    fn splits_two_vcl_nals_into_access_units() {
        let mut buffer = vec![
            0, 0, 0, 1, 0x02, 0xAA, // trail VCL
            0, 0, 0, 1, 0x02, 0xBB, // next picture VCL
            0, 0, 0, 1, 0x02, // incomplete
        ];
        let first = take_access_unit(&mut buffer).expect("first AU");
        assert_eq!(first, vec![0, 0, 0, 1, 0x02, 0xAA]);
        assert!(take_access_unit(&mut buffer).is_none());
    }

    #[test]
    fn detects_idr_keyframe() {
        let idr = vec![0, 0, 0, 1, 40, 0x00]; // type 20
        assert!(is_hevc_keyframe(&idr));
        let trail = vec![0, 0, 0, 1, 0x02, 0x00];
        assert!(!is_hevc_keyframe(&trail));
    }
}
