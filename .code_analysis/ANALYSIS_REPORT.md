# Project Analysis: remote_play

Date: 2026-05-15

## 1. Project Background

- One-liner: A macOS-first low-latency remote play / remote desktop streaming prototype written in Rust.
- Target audience: Developer/operator who wants to stream a host Mac screen and audio to a client app with interactive control.
- Business value: Explores a lightweight alternative to Steam Remote Play / Moonlight-style LAN streaming with a custom Rust stack.

## 2. Technical Architecture

- Tech stack: Rust 2024 workspace, Tokio, UDP sockets via socket2/tokio, serde+bincode protocol, ScreenCaptureKit, VideoToolbox HEVC, Opus, CPAL audio, GPUI/Metal client rendering.
- Workspace crates:
  - `protocol`: serialized RTP-like packets and control messages.
  - `remote_core`: shared traits, UDP multiplexer, timing utilities, statistics.
  - `host`: macOS capture, encode, audio capture/encode, input injection, stream orchestration.
  - `client`: UDP receive, jitter buffering, HEVC decode, audio playback, GPUI UI and rendering.
- Runtime flow:
  - Client binds an ephemeral UDP socket, renders a host list, sends `StartStream` to host `:8000`.
  - Host starts ScreenCaptureKit capture, VideoToolbox HEVC encoding, CPAL audio capture, Opus encoding.
  - Host sends video/audio as serialized `RtpPacket`s over a custom UDP multiplexing/framing layer.
  - Client filters packets by session id, reorders with `JitterBuffer`, decodes HEVC through VideoToolbox, and renders CVPixelBuffers with GPUI.
  - Client sends `Heartbeat`; host stops active stream after a 3 second heartbeat timeout.

## 3. Current Health

- `cargo check --workspace`: passes.
- `cargo test --workspace`: passes, but runs 0 tests.
- `cargo clippy --workspace --all-targets -- -W clippy::all`: passes with warnings.
- `cargo fmt --check`: fails due formatting drift in several files.
- Git status before/after analysis: clean except this analysis report file.

## 4. Strengths

- Clear separation between protocol/shared core/host/client crates.
- Uses native low-latency primitives on macOS: ScreenCaptureKit, VideoToolbox, CPAL, Opus, Metal/GPUI.
- Has a first usable end-to-end path: host standby, client host list, stream start/stop, heartbeat, telemetry, resolution/FPS/bitrate controls.
- Custom session id filtering reduces stale packet bleed when stream parameters are changed.
- Basic network jitter handling exists for both audio and video.

## 5. Main Risks

- Reliability risk: UDP fragmentation/reassembly is homegrown, unbounded by time, unauthenticated, and fragile around malformed fragments.
- Safety risk: VideoToolbox/CoreFoundation/CoreVideo code contains multiple unsafe/lifetime boundaries with little encapsulation or tests.
- Product maturity risk: input injection is incomplete; mouse movement is based on `(0,0)`, mouse down/up are unimplemented, and the client does not appear to send input events yet.
- Operability risk: no README, no permission checklist, no runbook beyond shell scripts, and macOS TCC/signing concerns are mostly encoded in scripts.
- Quality risk: zero automated tests; jitter buffer, protocol compatibility, fragmentation, and session handling are currently unprotected.
- Maintainability risk: `client/src/render.rs` is nearly 1000 lines and mixes rendering, session control, telemetry, and configuration changes.

## 6. Recommended Next Steps

1. Add focused unit tests for `protocol`, `remote_core::net` fragmentation/reassembly, and `client::jitter_buffer`.
2. Replace or harden the custom UDP framing with bounded fragment caches, validation, metrics, and packet loss behavior.
3. Extract render/session-control pieces from `client/src/render.rs` into smaller modules.
4. Complete input path end to end: client event capture, protocol mapping, host injection, permission documentation.
5. Wrap unsafe VideoToolbox/CoreVideo ownership in narrower safe abstractions and document each retain/release contract.
6. Add README with supported platform, permissions, signing setup, launch commands, and known limitations.
7. Run `cargo fmt` and clean compiler/clippy warnings before making feature changes.
