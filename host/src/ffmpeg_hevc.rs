use std::error::Error;
use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Mutex;

/// Long-running ffmpeg capture → HEVC Annex-B pipeline used by Linux and Windows hosts.
pub struct FfmpegHevcSource {
    child: Mutex<Option<Child>>,
    stdout: Mutex<Option<ChildStdout>>,
    leftover: Mutex<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl FfmpegHevcSource {
    pub fn start(
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut command = capture_command(width, height, fps, bitrate_kbps)?;
        command.stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = command.spawn().map_err(|err| {
            format!("ffmpeg HEVC pipeline failed to start ({err}). Install ffmpeg with libx265.")
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or("ffmpeg stdout was not captured")?;
        println!(
            "[FfmpegHevc] started {width}x{height}@{fps} {bitrate_kbps} kbps ({})",
            capture_backend_name()
        );
        Ok(Self {
            child: Mutex::new(Some(child)),
            stdout: Mutex::new(Some(stdout)),
            leftover: Mutex::new(Vec::new()),
            width,
            height,
            fps,
        })
    }

    pub fn pull_access_unit(&self) -> Result<(Vec<u8>, bool), Box<dyn Error + Send + Sync>> {
        let mut leftover = self.leftover.lock().unwrap_or_else(|err| err.into_inner());
        loop {
            if let Some(au) = take_access_unit(&mut leftover) {
                let keyframe = is_hevc_keyframe(&au);
                return Ok((au, keyframe));
            }
            let mut stdout = self.stdout.lock().unwrap_or_else(|err| err.into_inner());
            let stdout = stdout.as_mut().ok_or("ffmpeg stdout closed")?;
            let mut buf = [0u8; 32_768];
            let read = stdout.read(&mut buf)?;
            if read == 0 {
                return Err("ffmpeg HEVC pipeline ended".into());
            }
            leftover.extend_from_slice(&buf[..read]);
        }
    }
}

impl Drop for FfmpegHevcSource {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            if let Some(mut child) = child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn capture_backend_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "gdigrab"
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        "pipewire/wayland"
    } else {
        "x11grab"
    }
}

fn capture_command(
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
) -> Result<Command, Box<dyn Error + Send + Sync>> {
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner").arg("-loglevel").arg("error");
    #[cfg(target_os = "windows")]
    {
        cmd.args([
            "-f",
            "gdigrab",
            "-framerate",
            &fps.to_string(),
            "-i",
            "desktop",
        ]);
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(display) = std::env::var("DISPLAY") {
            cmd.args([
                "-f",
                "x11grab",
                "-video_size",
                &format!("{width}x{height}"),
                "-framerate",
                &fps.to_string(),
                "-i",
                &display,
            ]);
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
    let bitrate = format!("{bitrate_kbps}k");
    let keyint = fps.max(1).to_string();
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
        &format!("keyint={keyint}:min-keyint={keyint}:repeat-headers=1:bframes=0"),
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
    (0..slice.len()).find_map(|offset| {
        (start_code_len(&slice[offset..]) > 0).then_some(from + offset)
    })
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
