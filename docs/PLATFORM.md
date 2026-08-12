# Cross-Platform Platform Layer

## Goal

The current implementation is developed and tested on macOS, but the product target is cross-platform. Platform-specific APIs must stay behind adapter traits so the transport, scheduler, data plane, session model, and protocol do not become macOS-shaped.

## Boundary Rule

Core crates may define traits, data models, policies, schedulers, and wire formats. Platform crates or modules implement those traits.

No shared transport or protocol code should depend on:

- macOS CoreGraphics, ScreenCaptureKit, CoreVideo, VideoToolbox, or NSPasteboard.
- Windows Win32 clipboard, Windows Graphics Capture, Desktop Duplication, Media Foundation, D3D, or SendInput.
- Linux PipeWire, Wayland, X11, VA-API, xdg-desktop-portal, or uinput.

## Performance Rule

Cross-platform support must not mean lowering every backend to the slowest common path. The shared layer defines contracts and wire formats; each platform implementation should use its native fastest path.

- Realtime media must prefer zero-copy or low-copy handles: CVPixelBuffer/IOSurface on macOS, D3D resources on Windows, and DMA-BUF/PipeWire buffers on Linux.
- Hardware encode/decode paths are preferred over portable software codecs for the default interactive stream. Software fallback is allowed only as a visible compatibility mode.
- Platform-neutral traits must not require frame pixels, images, files, or clipboard payloads to be copied unless that feature inherently needs owned bytes.
- Any cross-platform helper crate must be evaluated as an adapter convenience, not as a reason to give up native APIs or measurable performance.
- A backend that falls back to CPU copies must expose that through capabilities or benchmarking notes before becoming a default path.

## Current Core Abstractions

`remote_core::traits` now contains the platform-facing contracts:

- `VideoCapturer`, `VideoEncoder`, `VideoDecoder`, `VideoRenderer`
- `AudioCapturer`, `AudioEncoder`, `AudioDecoder`
- `ClipboardProvider`
- `ClipboardFileStore`
- `InputInjector`

`VideoFrame` includes a platform-neutral handle kind so implementations can expose zero-copy handles without leaking concrete platform APIs into the data plane. Current handle categories include macOS CVPixelBuffer/IOSurface, Windows D3D resources, and Linux DMA-BUF.

`remote_core::clipboard_provider::MemoryClipboardProvider` is a test and service-layer utility, not a production OS clipboard backend. It lets sync logic be tested without choosing a platform API. `FilesystemClipboardFileStore` supports small-file clipboard tests and safe materialization; large file clipboard entries should move to the dedicated file-transfer stream rather than forcing full-file memory copies.

`remote_core::clipboard_sync::ClipboardSyncEndpoint` consumes these provider traits and owns the platform-neutral sync behavior: polling local changes, producing reliable data-plane envelopes, applying remote bundles, and suppressing same-content echo loops. Platform backends should stay thin and focused on OS format bridging; they should not duplicate transport, scheduling, or peer-sync state.

`remote_platform` is the first shared platform-adapter crate. It is intentionally outside `remote_core`, so OS APIs can be shared by host/client today and later by a unified binary without leaking AppKit, Win32, or Linux desktop APIs into the transport/data-plane layer.

## Clipboard And File Clipboard

Clipboard content uses `protocol::ClipboardBundle` with text, image, and file items. This avoids treating clipboard sync as text-only.

Platform responsibilities:

- macOS: bridge NSPasteboard text/images/file URLs into `ClipboardBundle`.
- Windows: bridge Win32 clipboard formats, bitmap/DIB data, and file drop lists into `ClipboardBundle`.
- Linux: bridge Wayland/X11 clipboard protocols through the desktop portal where available; support text/images/file references when the compositor allows it.

File clipboard is a sensitive cross-machine operation. `ClipboardSyncPolicy` defaults to text and image only. File bytes require explicit opt-in and should use the file-transfer path for large payloads instead of requiring the full file in memory.

The shared file-transfer path is `remote_core::file_transfer`. Platform code should only choose files, provide safe paths and MIME/type hints where available, and materialize received files in an OS-appropriate destination. The transfer itself is platform-neutral: manifest metadata plus raw file chunks over the reliable-object lane, with full-file checksum verification and no full-file memory requirement.

For OS file clipboard behavior, the preferred flow is grouped file references plus background streaming transfer. When a user copies multiple files or folders, the platform adapter reports the paths as one clipboard group; the transfer runtime streams each regular file with `FileTransferGroup` metadata and preserves directory-relative paths; the receiving side writes top-level file/folder references back to the OS clipboard atomically only after the whole group completes. This matches normal paste expectations better than publishing files one by one while still keeping large bytes out of memory clipboard bundles.

Manual file/folder selection and receive-folder selection are UI concerns. The current client uses a cross-platform file dialog to select paths, then hands those paths to the platform-neutral file-transfer runtime. The dialog dependency must never become part of `remote_core`, and selected directories must continue to flow through `SendFileGroup` so the high-performance streaming path and cancellation semantics stay shared. Receive destination and overwrite choices are represented as runtime config commands in `remote_core`, so future Windows and Linux shells can provide native pickers without forking the transfer engine.

The first macOS backend is `remote_platform::MacClipboardProvider`. It uses NSPasteboard directly and currently supports text, PNG/TIFF image data, and file URL references. It intentionally reports no file-byte support: macOS file clipboard uses file URLs plus safe materialization through the dedicated file-transfer stream, not by pretending every file clipboard item is an in-memory byte blob.

## Media Capture And Rendering

The target implementation shape is:

| Area | macOS | Windows | Linux |
| --- | --- | --- | --- |
| Screen capture | ScreenCaptureKit | Windows Graphics Capture or Desktop Duplication | PipeWire through xdg-desktop-portal, with X11 fallback where needed |
| Video encode/decode | VideoToolbox | Media Foundation or hardware codec through D3D | VA-API, Vulkan Video, GStreamer, or FFmpeg backend |
| Render | Metal | D3D11/D3D12 or wgpu | Vulkan/OpenGL/wgpu |
| Audio capture/playback | CoreAudio/cpal | WASAPI/cpal | PipeWire/PulseAudio/ALSA through cpal where possible |
| Input injection | CoreGraphics event APIs | SendInput | uinput, XTest, or compositor/portal-specific integration |

The preferred media path is zero-copy where the platform allows it:

- macOS: CVPixelBuffer/IOSurface to VideoToolbox/Metal.
- Windows: D3D texture/resource to Media Foundation/D3D renderer.
- Linux: DMA-BUF/PipeWire buffer to encoder/renderer.

CPU copies are acceptable as fallback paths but should be visible in capability reporting and benchmarks.

Audio has two product modes:

- Remote machine audio: capture the controlled machine's system output plus its microphone when enabled, then send that mix to the viewer with low-latency Opus.
- Conversation mode: capture the viewer microphone and send it back to the controlled machine for playback, so remote-control sessions can include live talkback.

The protocol model is stream-based rather than role-based. Each Opus stream advertises `AudioStreamConfig` with a stream id, source, direction, sample rate, channel count, and frame duration. The current source set is remote system audio, remote microphone audio, optional remote mixed audio, and viewer microphone talkback. This keeps the unified app model clean: the active controller may send a client-to-host talkback stream, while the controlled machine may send host-to-client system and microphone streams over the same data plane.

System-output capture and audio playback are platform-specific and must stay behind platform adapters: macOS should prefer ScreenCaptureKit/CoreAudio process or system audio capture where permissions allow it; Windows should use WASAPI loopback for system sound plus WASAPI input/output; Linux should use PipeWire monitor streams where available. Microphone capture through `cpal` is acceptable as a portable adapter convenience, but native low-latency paths remain preferred when they are measurably better.

The current macOS implementation uses `cpal` input capture for the remote microphone stream and an experimental ScreenCaptureKit system-audio stream behind `REMOTE_PLAY_SYSTEM_AUDIO=1`. The two sources are sent as separate Opus streams over the same realtime audio lane, with stream configs declaring `RemoteMicrophone` and `RemoteSystem`. Viewer microphone talkback is available behind `REMOTE_PLAY_TALKBACK=1`: the viewer side captures microphone audio with `cpal`, encodes Opus, and the controlled machine decodes/plays it through its default output device. The client GUI can switch talkback off, always-on, or push-to-talk; local mic mute gates encoding before packets are produced, and controlled-machine talkback playback volume/mute is applied at decode/playback. This is the first portable adapter path; platform-native low-latency backends can replace it later without changing protocol semantics.

## Implementation Rule For New Features

When adding a feature that touches OS APIs:

1. Add or extend a `remote_core` trait first.
2. Keep the protocol/data-plane representation platform-neutral.
3. Implement the macOS adapter behind `#[cfg(target_os = "macos")]`.
4. Add a doc note for the Windows/Linux adapter contract before relying on the feature in shared code.
5. Verify at least the trait-level logic and data-plane path with unit or loopback tests.

This keeps today's macOS iteration useful while leaving a clean runway for Windows and Linux support.
