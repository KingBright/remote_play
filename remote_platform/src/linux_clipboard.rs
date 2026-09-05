use async_trait::async_trait;
use protocol::{ClipboardBundle, ClipboardItem, ClipboardText};
use remote_core::clipboard_plane::{ClipboardSyncPolicy, validate_clipboard_bundle};
use remote_core::{
    ClipboardBackendCapabilities, ClipboardFileReference, ClipboardFileReferenceProvider,
    ClipboardProvider, PlatformKind,
};
use std::error::Error;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct LinuxClipboardProvider {
    cached: Mutex<Option<ClipboardBundle>>,
}

impl LinuxClipboardProvider {
    pub fn new() -> Self {
        Self {
            cached: Mutex::new(None),
        }
    }
}

impl Default for LinuxClipboardProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ClipboardProvider for LinuxClipboardProvider {
    fn platform(&self) -> PlatformKind {
        PlatformKind::Linux
    }

    fn capabilities(&self) -> ClipboardBackendCapabilities {
        ClipboardBackendCapabilities {
            text: true,
            image: false,
            file_references: true,
            file_bytes: false,
        }
    }

    async fn read_clipboard(
        &mut self,
        _policy: ClipboardSyncPolicy,
    ) -> Result<Option<ClipboardBundle>, Box<dyn Error + Send + Sync>> {
        // Try reading via wl-paste or xclip if available, or fall back to cached
        let mut text_opt = None;
        if let Ok(output) = std::process::Command::new("wl-paste")
            .arg("--no-newline")
            .output()
        {
            if output.status.success() {
                if let Ok(txt) = String::from_utf8(output.stdout) {
                    if !txt.is_empty() {
                        text_opt = Some(txt);
                    }
                }
            }
        }
        if text_opt.is_none() {
            if let Ok(output) = std::process::Command::new("xclip")
                .args(["-selection", "clipboard", "-o"])
                .output()
            {
                if output.status.success() {
                    if let Ok(txt) = String::from_utf8(output.stdout) {
                        if !txt.is_empty() {
                            text_opt = Some(txt);
                        }
                    }
                }
            }
        }

        if let Some(text) = text_opt {
            let bundle = ClipboardBundle::text(unix_now_ms(), text);
            return Ok(Some(bundle));
        }

        let lock = self.cached.lock().map_err(|e| e.to_string())?;
        Ok(lock.clone())
    }

    async fn write_clipboard(
        &mut self,
        bundle: &ClipboardBundle,
        policy: ClipboardSyncPolicy,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        validate_clipboard_bundle(bundle)?;
        policy.validate_bundle(bundle)?;

        for item in &bundle.items {
            if let ClipboardItem::Text(text_item) = item {
                // Try writing via wl-copy or xclip
                let _ = std::process::Command::new("wl-copy")
                    .arg(&text_item.text)
                    .output();
                let mut child = std::process::Command::new("xclip")
                    .args(["-selection", "clipboard"])
                    .stdin(std::process::Stdio::piped())
                    .spawn();
                if let Ok(ref mut proc) = child {
                    if let Some(ref mut stdin) = proc.stdin {
                        use std::io::Write;
                        let _ = stdin.write_all(text_item.text.as_bytes());
                    }
                }
            }
        }

        let mut lock = self.cached.lock().map_err(|e| e.to_string())?;
        *lock = Some(bundle.clone());
        Ok(())
    }
}

#[async_trait]
impl ClipboardFileReferenceProvider for LinuxClipboardProvider {
    fn platform(&self) -> PlatformKind {
        PlatformKind::Linux
    }

    async fn read_clipboard_file_references(
        &mut self,
    ) -> Result<Vec<ClipboardFileReference>, Box<dyn Error + Send + Sync>> {
        Ok(Vec::new())
    }

    async fn write_clipboard_file_references(
        &mut self,
        _references: &[ClipboardFileReference],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        Ok(())
    }
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
