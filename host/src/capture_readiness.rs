//! Resolve and validate the Windows capture dependency before starting audio or video.
//! Product builds use an app-private FFmpeg bundle so a random system install cannot
//! silently change capture behavior.
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

pub(crate) const FFMPEG_ENV: &str = "REMOTE_PLAY_FFMPEG_BIN";
pub(crate) const ALLOW_SYSTEM_FFMPEG_ENV: &str = "REMOTE_PLAY_ALLOW_SYSTEM_FFMPEG";
static VALIDATED_FFMPEG: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowsCaptureRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub adapter: String,
}

#[derive(Debug)]
struct DisplayCandidate {
    rect: WindowsCaptureRect,
    primary: bool,
    virtual_display: bool,
}

pub(crate) fn capture_display_rect() -> Result<WindowsCaptureRect, String> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Graphics::Gdi::{
        DEVMODEW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP,
        DISPLAY_DEVICE_PRIMARY_DEVICE, ENUM_CURRENT_SETTINGS, EnumDisplayDevicesW,
        EnumDisplaySettingsExW,
    };

    let mut candidates = Vec::new();
    for index in 0..32u32 {
        let mut device: DISPLAY_DEVICEW = unsafe { zeroed() };
        device.cb = size_of::<DISPLAY_DEVICEW>() as u32;
        if unsafe { EnumDisplayDevicesW(std::ptr::null(), index, &mut device, 0) } == 0 {
            break;
        }
        if device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP == 0 {
            continue;
        }

        let description = fixed_wide(&device.DeviceString);
        let device_id = fixed_wide(&device.DeviceID);
        let mut mode: DEVMODEW = unsafe { zeroed() };
        mode.dmSize = size_of::<DEVMODEW>() as u16;
        if unsafe {
            EnumDisplaySettingsExW(
                device.DeviceName.as_ptr(),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
                0,
            )
        } == 0
        {
            continue;
        }
        if mode.dmPelsWidth == 0 || mode.dmPelsHeight == 0 {
            continue;
        }
        let position = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
        candidates.push(DisplayCandidate {
            rect: WindowsCaptureRect {
                x: position.x,
                y: position.y,
                width: mode.dmPelsWidth,
                height: mode.dmPelsHeight,
                adapter: description.clone(),
            },
            primary: device.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE != 0,
            virtual_display: looks_like_virtual_display(&description, &device_id),
        });
    }

    candidates
        .into_iter()
        .filter(|candidate| !candidate.virtual_display)
        .min_by_key(|candidate| !candidate.primary)
        .map(|candidate| candidate.rect)
        .ok_or_else(|| {
            "Windows screen capture unavailable: no physical desktop display is attached; virtual or indirect displays are intentionally excluded.".into()
        })
}

fn fixed_wide(value: &[u16]) -> String {
    let len = value.iter().position(|unit| *unit == 0).unwrap_or(value.len());
    String::from_utf16_lossy(&value[..len])
}

fn looks_like_virtual_display(description: &str, device_id: &str) -> bool {
    let description = description.to_ascii_lowercase();
    let device_id = device_id.to_ascii_uppercase();
    description.contains("virtual display")
        || description.contains("indirect display")
        || description.contains("idd")
        || device_id.starts_with("ROOT\\DISPLAY")
}

pub(crate) fn ffmpeg_program() -> Result<PathBuf, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("Cannot locate the RemotePlay installation: {error}"))?;
    let path = resolve_ffmpeg(
        std::env::var_os(FFMPEG_ENV),
        executable
            .parent()
            .ok_or("RemotePlay installation has no parent directory")?,
        std::env::var_os("PATH"),
        std::env::var_os(ALLOW_SYSTEM_FFMPEG_ENV)
            .is_some_and(|value| matches!(value.to_string_lossy().as_ref(), "1" | "true" | "TRUE")),
    )?;
    validate_ffmpeg(&path)?;
    Ok(path)
}

fn resolve_ffmpeg(
    explicit: Option<OsString>,
    installation: &Path,
    search_path: Option<OsString>,
    allow_system: bool,
) -> Result<PathBuf, String> {
    if let Some(explicit) = explicit {
        let path = PathBuf::from(explicit);
        if !path.is_absolute() || !path.is_file() {
            return Err(format!(
                "Windows screen capture unavailable: {FFMPEG_ENV} must identify an existing absolute ffmpeg.exe path. No capture or microphone was started."
            ));
        }
        return Ok(path);
    }
    for path in [
        installation.join("bin/ffmpeg.exe"),
        installation.join("ffmpeg.exe"),
    ] {
        if path.is_file() {
            return Ok(path);
        }
    }
    if allow_system && let Some(search_path) = search_path {
        for directory in
            std::env::split_paths(&search_path).filter(|directory| directory.is_absolute())
        {
            let path = directory.join("ffmpeg.exe");
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Err(format!(
        "Windows screen capture unavailable: the private ffmpeg.exe bundle is missing. Install RemotePlay with its FFmpeg bundle, or set {FFMPEG_ENV} to an absolute validated path. System PATH fallback is disabled unless {ALLOW_SYSTEM_FFMPEG_ENV}=1. No capture or microphone was started."
    ))
}

fn validate_ffmpeg(path: &Path) -> Result<(), String> {
    let validated = VALIDATED_FFMPEG.get_or_init(|| Mutex::new(HashSet::new()));
    if validated
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(path)
    {
        return Ok(());
    }

    let devices = ffmpeg_listing(path, "-devices")?;
    if !devices.to_ascii_lowercase().contains("gdigrab") {
        return Err(
            "Windows screen capture unavailable: RemotePlay's FFmpeg bundle does not provide gdigrab. No capture or microphone was started."
                .into(),
        );
    }
    let encoders = ffmpeg_listing(path, "-encoders")?;
    if !encoders.to_ascii_lowercase().contains("libx265") {
        return Err(
            "Windows screen capture unavailable: RemotePlay's FFmpeg bundle does not provide libx265 HEVC encoding. No capture or microphone was started."
                .into(),
        );
    }
    validated
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(path.to_path_buf());
    Ok(())
}

fn ffmpeg_listing(path: &Path, flag: &str) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = Command::new(path)
        .args(["-hide_banner", flag])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("Cannot run RemotePlay's FFmpeg bundle: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "RemotePlay's FFmpeg bundle failed capability probe {flag} with status {}",
            output.status
        ));
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "remoteplay-readiness-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"test fixture, not an executable").unwrap();
            path
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_dependency_has_actionable_error() {
        let dir = Directory::new();
        let error = resolve_ffmpeg(None, &dir.0, None, false).unwrap_err();
        assert!(error.contains("private ffmpeg.exe bundle is missing"));
        assert!(error.contains("No capture or microphone was started"));
    }

    #[test]
    fn explicit_absolute_path_has_priority() {
        let dir = Directory::new();
        let explicit = dir.file("custom/ffmpeg.exe");
        dir.file("bin/ffmpeg.exe");
        assert_eq!(
            resolve_ffmpeg(Some(explicit.clone().into()), &dir.0, None, false).unwrap(),
            explicit
        );
    }

    #[test]
    fn broken_explicit_configuration_does_not_fall_back() {
        let dir = Directory::new();
        dir.file("bin/ffmpeg.exe");
        assert!(
            resolve_ffmpeg(Some(dir.0.join("missing.exe").into()), &dir.0, None, false).is_err()
        );
        assert!(resolve_ffmpeg(Some("ffmpeg.exe".into()), &dir.0, None, false).is_err());
        assert!(resolve_ffmpeg(Some("".into()), &dir.0, None, false).is_err());
    }

    #[test]
    fn private_bundle_precedes_path() {
        let dir = Directory::new();
        let bundled = dir.file("bin/ffmpeg.exe");
        dir.file("tools/ffmpeg.exe");
        let path = std::env::join_paths([dir.0.join("tools")]).unwrap();
        assert_eq!(
            resolve_ffmpeg(None, &dir.0, Some(path), false).unwrap(),
            bundled
        );
    }

    #[test]
    fn supports_executable_next_to_application() {
        let dir = Directory::new();
        let bundled = dir.file("ffmpeg.exe");
        assert_eq!(resolve_ffmpeg(None, &dir.0, None, false).unwrap(), bundled);
    }

    #[test]
    fn system_path_requires_explicit_opt_in() {
        let dir = Directory::new();
        let tool = dir.file("tools/ffmpeg.exe");
        let path = std::env::join_paths([dir.0.join("tools")]).unwrap();
        assert!(resolve_ffmpeg(None, &dir.0, Some(path.clone()), false).is_err());
        assert_eq!(
            resolve_ffmpeg(None, &dir.0, Some(path), true).unwrap(),
            tool
        );
    }

    #[test]
    fn a_directory_named_ffmpeg_is_not_an_executable() {
        let dir = Directory::new();
        std::fs::create_dir(dir.0.join("ffmpeg.exe")).unwrap();
        assert!(resolve_ffmpeg(None, &dir.0, None, false).is_err());
    }

    #[test]
    fn relative_search_path_entries_are_not_used() {
        let dir = Directory::new();
        let path = std::env::join_paths([PathBuf::new(), PathBuf::from(".")]).unwrap();
        assert!(resolve_ffmpeg(None, &dir.0, Some(path), true).is_err());
    }

    #[test]
    fn selected_physical_display_has_positive_geometry() {
        let display = capture_display_rect().unwrap();
        assert!(display.width > 0 && display.height > 0);
        assert!(!display.adapter.trim().is_empty());
    }

    #[test]
    fn virtual_display_detection_does_not_reject_physical_gpu() {
        assert!(looks_like_virtual_display(
            "Example Virtual Display Adapter",
            "ROOT\\DISPLAY\\0001"
        ));
        assert!(looks_like_virtual_display(
            "Indirect Display Driver",
            "ROOT\\SAMPLE\\0001"
        ));
        assert!(!looks_like_virtual_display(
            "AMD Radeon(TM) 8060S Graphics",
            "PCI\\VEN_1002&DEV_1586"
        ));
    }
}
