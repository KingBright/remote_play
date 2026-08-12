#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

pub(crate) fn run_mesh_admin_setup() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let script_path = macos_mesh_daemon_installer_path()?;
        let script = macos_mesh_daemon_installer_applescript(&script_path);
        let output = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .map_err(|err| format!("could not open the macOS administrator prompt: {err}"))?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        Err(if detail.is_empty() {
            format!("macOS administrator setup exited with {}", output.status)
        } else {
            detail.to_string()
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("automatic mesh administrator setup is not available on this platform yet".to_string())
    }
}

pub(crate) fn mesh_setup_was_cancelled(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("user canceled") || lower.contains("user cancelled")
}

#[cfg(target_os = "macos")]
fn macos_mesh_daemon_installer_path() -> Result<PathBuf, String> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(path) = bundled_macos_mesh_daemon_installer_path(&exe)
        && path.is_file()
    {
        return Ok(path);
    }

    let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| root.join("scripts").join("install_macos_mesh_daemon.sh"))
        .filter(|path| path.is_file());
    dev_path.ok_or_else(|| "RemotePlay mesh setup script was not found.".to_string())
}

#[cfg(target_os = "macos")]
fn bundled_macos_mesh_daemon_installer_path(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name().is_none_or(|name| name != "MacOS") {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir
        .file_name()
        .is_none_or(|name| name != "Contents")
    {
        return None;
    }
    Some(
        contents_dir
            .join("Resources")
            .join("scripts")
            .join("install_macos_mesh_daemon.sh"),
    )
}

#[cfg(target_os = "macos")]
fn macos_mesh_daemon_installer_applescript(script_path: &Path) -> String {
    let command = shell_quote(&script_path.display().to_string());
    format!(
        "do shell script {} with administrator privileges",
        applescript_string_literal(&command)
    )
}

#[cfg(target_os = "macos")]
fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    let mut quoted = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(target_os = "macos")]
fn applescript_string_literal(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn bundled_installer_path_is_derived_from_app_executable() {
        let path = bundled_macos_mesh_daemon_installer_path(Path::new(
            "/Applications/RemotePlay.app/Contents/MacOS/remote_play",
        ))
        .expect("app executable should map to installer script");

        assert_eq!(
            path,
            PathBuf::from(
                "/Applications/RemotePlay.app/Contents/Resources/scripts/install_macos_mesh_daemon.sh"
            )
        );
    }

    #[test]
    fn mesh_installer_applescript_quotes_shell_path() {
        let script = macos_mesh_daemon_installer_applescript(Path::new(
            "/Applications/Remote Play.app/Contents/Resources/scripts/install's.sh",
        ));

        assert!(script.starts_with("do shell script \""));
        assert!(script.ends_with("\" with administrator privileges"));
        assert!(script.contains("/Applications/Remote Play.app"));
        assert!(script.contains("'\\\\''s.sh"));
    }

    #[test]
    fn cancellation_detection_accepts_american_and_british_spellings() {
        assert!(mesh_setup_was_cancelled("User canceled."));
        assert!(mesh_setup_was_cancelled("User cancelled."));
    }
}
