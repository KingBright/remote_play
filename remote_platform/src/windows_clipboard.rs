use async_trait::async_trait;
use protocol::ClipboardBundle;
use remote_core::clipboard_plane::{ClipboardSyncPolicy, validate_clipboard_bundle};
use remote_core::{ClipboardBackendCapabilities, ClipboardProvider, PlatformKind};
use std::error::Error;
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{HANDLE, HWND};
use windows_sys::Win32::System::DataExchange::{
    CF_UNICODETEXT, CloseClipboard, GetClipboardData, OpenClipboard,
};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};

pub struct WindowsClipboardProvider {
    cached: Mutex<Option<ClipboardBundle>>,
}

impl WindowsClipboardProvider {
    pub fn new() -> Self {
        Self {
            cached: Mutex::new(None),
        }
    }
}

impl Default for WindowsClipboardProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn read_unicode_text() -> Option<String> {
    unsafe {
        if OpenClipboard(HWND::default()) == 0 {
            return None;
        }
        let handle: HANDLE = GetClipboardData(CF_UNICODETEXT);
        if handle.is_null() {
            CloseClipboard();
            return None;
        }
        let locked = GlobalLock(handle.cast());
        if locked.is_null() {
            CloseClipboard();
            return None;
        }
        let mut wide = Vec::new();
        let mut ptr = locked.cast::<u16>();
        while *ptr != 0 {
            wide.push(*ptr);
            ptr = ptr.add(1);
        }
        GlobalUnlock(handle.cast());
        CloseClipboard();
        String::from_utf16(&wide)
            .ok()
            .filter(|text| !text.is_empty())
    }
}

#[async_trait]
impl ClipboardProvider for WindowsClipboardProvider {
    fn platform(&self) -> PlatformKind {
        PlatformKind::Windows
    }

    fn capabilities(&self) -> ClipboardBackendCapabilities {
        ClipboardBackendCapabilities {
            text: true,
            image: false,
            file_references: false,
            file_bytes: false,
        }
    }

    async fn read_clipboard(
        &mut self,
        _policy: ClipboardSyncPolicy,
    ) -> Result<Option<ClipboardBundle>, Box<dyn Error + Send + Sync>> {
        if let Some(text) = read_unicode_text() {
            let bundle = ClipboardBundle::text(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                text,
            );
            validate_clipboard_bundle(&bundle)?;
            *self.cached.lock().unwrap() = Some(bundle.clone());
            return Ok(Some(bundle));
        }
        Ok(self.cached.lock().unwrap().clone())
    }

    async fn write_clipboard(
        &mut self,
        bundle: &ClipboardBundle,
        policy: ClipboardSyncPolicy,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        validate_clipboard_bundle(bundle)?;
        policy.validate_bundle(bundle)?;
        *self.cached.lock().unwrap() = Some(bundle.clone());
        Ok(())
    }
}
