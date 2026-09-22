use async_trait::async_trait;
use protocol::{ClipboardBundle, ClipboardImage, ClipboardItem, ClipboardText};
use remote_core::clipboard_plane::{ClipboardSyncPolicy, validate_clipboard_bundle};
use remote_core::{
    ClipboardBackendCapabilities, ClipboardFileReference, ClipboardFileReferenceProvider,
    ClipboardProvider, PlatformKind,
};
use std::{
    error::Error,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
#[derive(Clone, Copy)]
enum Backend {
    Wayland,
    X11,
}
pub struct LinuxClipboardProvider {
    backend: Option<Backend>,
}

fn installed(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join(name).is_file()))
}
impl LinuxClipboardProvider {
    pub fn new() -> Self {
        let backend = if std::env::var_os("WAYLAND_DISPLAY").is_some()
            && installed("wl-copy")
            && installed("wl-paste")
        {
            Some(Backend::Wayland)
        } else if std::env::var_os("DISPLAY").is_some() && installed("xclip") {
            Some(Backend::X11)
        } else {
            None
        };
        Self { backend }
    }
    async fn read(&self, mime: &str, limit: usize) -> Result<Option<Vec<u8>>> {
        let Some(backend) = self.backend else {
            return Err(
                "install wl-clipboard (Wayland) or xclip (X11) for clipboard sharing".into(),
            );
        };
        let mut command = match backend {
            Backend::Wayland => {
                let mut c = Command::new("wl-paste");
                c.args(["--no-newline", "--type", mime]);
                c
            }
            Backend::X11 => {
                let mut c = Command::new("xclip");
                c.args(["-selection", "clipboard", "-o", "-target", mime]);
                c
            }
        };
        let mut child = command
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?;
        let output = tokio::time::timeout(Duration::from_secs(2), async {
            let mut bytes = Vec::new();
            child
                .stdout
                .take()
                .ok_or("missing clipboard output")?
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .await?;
            if bytes.len() > limit {
                return Err("clipboard exceeds size limit".into());
            }
            if child.wait().await?.success() {
                Ok(Some(bytes))
            } else {
                Ok(None)
            }
        })
        .await?;
        output
    }
    async fn write(&self, mime: &str, bytes: &[u8]) -> Result<()> {
        let Some(backend) = self.backend else {
            return Err("clipboard backend is unavailable".into());
        };
        let mut command = match backend {
            Backend::Wayland => {
                let mut c = Command::new("wl-copy");
                c.args(["--type", mime]);
                c
            }
            Backend::X11 => {
                let mut c = Command::new("xclip");
                c.args(["-selection", "clipboard", "-i", "-target", mime]);
                c
            }
        };
        let mut child = command
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut input = child.stdin.take().ok_or("missing clipboard input")?;
            input.write_all(bytes).await?;
            input.shutdown().await?;
            drop(input);
            if !child.wait().await?.success() {
                return Err("system clipboard write failed".into());
            }
            Ok(())
        })
        .await?
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
        let available = self.backend.is_some();
        ClipboardBackendCapabilities {
            text: available,
            image: available,
            file_references: available,
            file_bytes: false,
        }
    }
    async fn read_clipboard(
        &mut self,
        policy: ClipboardSyncPolicy,
    ) -> Result<Option<ClipboardBundle>> {
        if self.backend.is_none() {
            return Ok(None);
        }
        let mut items = Vec::new();
        if policy.allow_images
            && let Some(bytes) = self.read("image/png", policy.max_image_bytes).await?
        {
            items.push(ClipboardItem::Image(ClipboardImage {
                mime_type: "image/png".into(),
                width: None,
                height: None,
                bytes,
            }));
        } else if policy.allow_text
            && let Some(bytes) = self
                .read("text/plain;charset=utf-8", policy.max_text_bytes)
                .await?
        {
            items.push(ClipboardItem::Text(ClipboardText {
                text: String::from_utf8(bytes)?,
            }));
        }
        if items.is_empty() {
            return Ok(None);
        }
        let bundle = ClipboardBundle::new(
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
            items,
        );
        validate_clipboard_bundle(&bundle)?;
        policy.validate_bundle(&bundle)?;
        Ok(Some(bundle))
    }
    async fn write_clipboard(
        &mut self,
        bundle: &ClipboardBundle,
        policy: ClipboardSyncPolicy,
    ) -> Result<()> {
        validate_clipboard_bundle(bundle)?;
        policy.validate_bundle(bundle)?;
        if let Some(image) = bundle.items.iter().find_map(|item| {
            if let ClipboardItem::Image(image) = item {
                Some(image)
            } else {
                None
            }
        }) {
            if !matches!(image.mime_type.as_str(), "image/png" | "image/tiff") {
                return Err("unsupported clipboard image format".into());
            }
            self.write(&image.mime_type, &image.bytes).await
        } else if let Some(text) = bundle.items.iter().find_map(|item| {
            if let ClipboardItem::Text(text) = item {
                Some(text)
            } else {
                None
            }
        }) {
            self.write("text/plain;charset=utf-8", text.text.as_bytes())
                .await
        } else {
            Err("clipboard bundle has no supported representation".into())
        }
    }
}

#[async_trait]
impl ClipboardFileReferenceProvider for LinuxClipboardProvider {
    fn platform(&self) -> PlatformKind {
        PlatformKind::Linux
    }
    async fn read_clipboard_file_references(&mut self) -> Result<Vec<ClipboardFileReference>> {
        if self.backend.is_none() {
            return Ok(Vec::new());
        }
        let Some(bytes) = self.read("text/uri-list", 1024 * 1024).await? else {
            return Ok(Vec::new());
        };
        Ok(String::from_utf8(bytes)?
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| url::Url::parse(line.trim()).ok()?.to_file_path().ok())
            .map(ClipboardFileReference::new)
            .collect())
    }
    async fn write_clipboard_file_references(
        &mut self,
        refs: &[ClipboardFileReference],
    ) -> Result<()> {
        let mut data = String::new();
        for reference in refs {
            let path: PathBuf = tokio::fs::canonicalize(Path::new(&reference.path)).await?;
            let url = url::Url::from_file_path(path).map_err(|_| "invalid clipboard file path")?;
            data.push_str(url.as_str());
            data.push_str("\r\n");
        }
        self.write("text/uri-list", data.as_bytes()).await
    }
}
