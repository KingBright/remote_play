#[cfg(target_os = "macos")]
mod macos_clipboard;

#[cfg(target_os = "macos")]
pub use macos_clipboard::MacClipboardProvider;

#[cfg(target_os = "linux")]
mod linux_clipboard;

#[cfg(target_os = "linux")]
pub use linux_clipboard::LinuxClipboardProvider;

#[cfg(target_os = "windows")]
mod windows_clipboard;

#[cfg(target_os = "windows")]
pub use windows_clipboard::WindowsClipboardProvider;
