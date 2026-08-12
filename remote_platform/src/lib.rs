#[cfg(target_os = "macos")]
mod macos_clipboard;

#[cfg(target_os = "macos")]
pub use macos_clipboard::MacClipboardProvider;
