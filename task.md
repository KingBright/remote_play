# Task: remote_play Upgrade Plan

## Goal

Turn the current macOS remote streaming prototype into a more reliable, testable, and maintainable project without losing the low-latency native-stack direction.

## Guiding Rules

- Improve one small slice at a time.
- Every implementation step must have a verification command or manual acceptance check.
- Prefer tests around protocol/network/session behavior before refactoring risky runtime code.
- Keep behavior changes narrow unless a phase explicitly targets architecture.
- Keep platform APIs behind `remote_core` traits so macOS testing does not lock the project away from Windows/Linux support.
- Never trade away realtime performance for cross-platform convenience; shared abstractions must preserve native fast paths and avoid mandatory copies.

## Phase 0: Baseline

- [x] Analyze project structure and current health.
- [x] Record findings in `.code_analysis/ANALYSIS_REPORT.md`.
- [x] Verify baseline commands:
  - `cargo check --workspace` passes.
  - `cargo test --workspace` passes but runs 0 tests.
  - `cargo clippy --workspace --all-targets -- -W clippy::all` passes with warnings.
  - `cargo fmt --check` currently fails.

## Phase 1: Quality Foundation

- [x] Step 1.1: Format and warning cleanup.
  - Scope: run `cargo fmt`, remove obvious unused imports/fields where safe, keep behavior unchanged.
  - Verification: `cargo fmt --check`, `cargo check --workspace`.

- [x] Step 1.2: Add protocol round-trip tests.
  - Scope: `protocol/src/lib.rs`.
  - Coverage: `RtpPacket`, `ControlMessage::StartStream`, `ControlMessage::HostTelemetry`, input events.
  - Verification: `cargo test -p protocol`.

- [x] Step 1.3: Add jitter buffer unit tests.
  - Scope: `client/src/jitter_buffer.rs`.
  - Coverage: in-order packets, out-of-order packets, stale packets, skipped missing packet behavior, sequence wraparound.
  - Verification: `cargo test -p client jitter`.

- [x] Step 1.4: Add UDP multiplexer tests where feasible.
  - Scope: `remote_core/src/net.rs`.
  - Coverage: control/RTP send-receive, fragmented payload reassembly, invalid header handling.
  - Verification: `cargo test -p remote_core net`.

## Phase 2: Network Robustness

- [x] Step 2.1: Harden fragment parsing.
  - Scope: `remote_core/src/net.rs`.
  - Work: validate `chunk_idx < total_chunks`, reject zero chunks, prevent panics, track fragment age.
  - Verification: targeted unit tests plus `cargo test --workspace`.

- [x] Step 2.2: Bound fragment cache by age and size.
  - Scope: `remote_core/src/net.rs`.
  - Work: replace ad hoc `fragments.len() > 10` cleanup with explicit TTL/count limits.
  - Verification: tests for expired/incomplete fragments.

- [x] Step 2.3: Reduce noisy hot-path logging.
  - Scope: `remote_core/src/net.rs`, `client/src/video_decode.rs`, `host/src/video_encode.rs`.
  - Work: remove or gate per-packet/per-NALU `println!` calls.
  - Verification: `cargo check --workspace`; runtime logs should remain readable.

## Phase 3: Unified High-Performance Data Plane

- [x] Step 3.1: Define the unified data plane protocol.
  - Scope: `protocol/src/lib.rs`, `remote_core`.
  - Work: design one shared transport envelope for video, audio, control, clipboard, file transfer, and arbitrary future payloads. The envelope must include stream id, lane, content kind, priority, sequencing, deadlines, chunk metadata, optional reliability metadata, and integrity checksum where appropriate.
  - Realtime rule: video/audio/input/control that affects interaction must use low-latency realtime lanes with deadline-first scheduling, minimal copies, no head-of-line blocking behind file/clipboard payloads, and permission to drop stale packets instead of waiting.
  - Non-realtime rule: clipboard/file/arbitrary large objects may accept higher latency and must use reliable ordered delivery, retry/cancel, progress, checksums, and backpressure.
  - Verification: protocol round-trip tests, lane classification tests, and benchmarks comparing the current RTP-like path against the new envelope before migrating audio/video.

- [x] Step 3.2: Add realtime lane primitives.
  - Scope: `remote_core`.
  - Work: provide zero/low-copy packet creation, minimal envelope overhead, realtime sequencing, deadline metadata, stale packet discard, jitter-buffer compatibility, and priority scheduling for video/audio.
  - Verification: microbenchmarks for encode/envelope/decode overhead, packet allocation counts where feasible, and tests for stale realtime packet handling.

- [x] Step 3.3: Add reliable large-object transfer primitives.
  - Scope: `remote_core`.
  - Work: build chunking, reassembly, acknowledgements, retry/cancel, progress reporting, and backpressure for non-real-time payloads.
  - Verification: unit tests for complete transfer, dropped chunks, duplicate chunks, cancellation, and checksum failure.

- [ ] Step 3.4: Migrate audio/video onto the unified data plane with no performance regression. (benchmarking in progress)
  - Scope: host/client streaming path.
  - Work: route H.265 video and Opus audio through the unified envelope realtime lanes while preserving current timestamps, SSRC/session isolation, jitter handling, and low latency.
  - Verification: benchmark current path vs unified path for serialization overhead, allocation count, packet size overhead, throughput, and latency. Migration is accepted only if overhead is negligible or demonstrably offset by the new design.
  - Progress: added `protocol/examples/transport_bench.rs` and `remote_core/examples/loopback_transport_bench.rs`.
  - Progress: added compact realtime wire encoding for `DataEnvelope` and UDP multiplexing header `0x05`; `UdpSender::send_data` now automatically uses the compact realtime path for realtime envelopes while preserving full `0x04` envelopes for reliable/non-realtime traffic.
  - Benchmark note, 2026-05-15: full bincode `DataEnvelope` overhead is +36 bytes versus the current RTP-like packet; compact realtime overhead is +6 bytes. Protocol encode/decode is materially faster on the compact path. UDP loopback is near parity for 64 B and 1,400 B payloads, slightly slower at 512 B in one 5,000-iteration run, and faster for 8,000 B payloads. Migration is still pending allocation, jitter, packet loss, mixed-traffic, and real host/client smoke benchmarks.
  - Progress, 2026-05-17: added `remote_core::media_plane` adapters for RTP-like video/audio packets <-> realtime `DataEnvelope`, with tests for video, audio, unsupported payload types, and non-media envelopes.
  - Progress, 2026-05-17: client now consumes realtime data-plane media packets by adapting them back into the existing jitter/decode path. Host remains legacy by default, but can opt into media-over-data-plane with `REMOTE_PLAY_DATA_PLANE_MEDIA=1`.
  - Benchmark note, 2026-05-17: added `remote_core/examples/media_adapter_bench.rs`. Full RTP -> compact data-plane -> RTP adapter roundtrip remains faster than the legacy bincode RTP encode/decode benchmark in the tested payload sizes, with the same +6 byte compact packet overhead. Migration is still pending runtime smoke testing, allocation checks, and mixed traffic tests.
  - Progress, 2026-05-17: corrected audio adapter semantics so Opus RTP sample-clock timestamps are preserved but not treated as millisecond deadlines. Added UDP roundtrip tests for video media packets, audio media packets without deadlines, and reliable-object envelopes staying on the full data path.
  - Progress, 2026-05-17: added `remote_core::data_plane::LaneScheduler` as a tested in-memory mixed-traffic scheduler. It prioritizes realtime by priority/deadline/sequence, keeps reliable objects FIFO, and drops stale realtime packets instead of blocking bulk work.
  - Progress, 2026-05-17: added scheduler capacity/backpressure behavior. Realtime queue overflow drops the lowest-urgency realtime packet; reliable queue overflow rejects new reliable payloads and records backpressure stats.
  - Progress, 2026-05-17: fixed video timestamp semantics for scheduled data-plane media. The adapter now unwraps wrapped `u32` millisecond RTP timestamps to the nearest `u64` sender timestamp so scheduler stale checks do not incorrectly drop valid video packets.
  - Progress, 2026-05-17: added scheduled media loopback smoke coverage for RTP-like video/audio -> media adapter -> `ScheduledDataSender` -> UDP loopback -> media adapter back to RTP-like packets, while queued bulk is present.
  - Progress, 2026-05-17: added `ScheduledDataSender::stats()` with entrance queue, entrance full/closed, scheduler drop/rejection, sent realtime/reliable, and send error counters for runtime smoke visibility.
  - Progress, 2026-05-17: host now prints those scheduled sender counters once per second when `REMOTE_PLAY_DATA_PLANE_MEDIA=1`, including entrance queue pressure, realtime drops, reliable rejections, sent realtime/reliable counts, and send errors.
  - Progress, 2026-05-17: added a high-pressure scheduled sender loopback regression where many reliable-object packets overflow the bulk queue but video/audio still transmit first.
  - Progress, 2026-05-17: added a host-style mixed data-plane regression where file transfer queues reliable packets first, video/audio are then submitted through the same `ScheduledDataSender`, UDP observes video then audio before file packets, and the file still completes with checksum-verified bytes.
  - Progress, 2026-05-17: added `remote_core/examples/headless_smoke_client.rs` and `scripts/smoke_dataplane_media_file.sh` so runtime smoke can exercise the real host process without the GPUI client.
  - Progress, 2026-05-17: added `remote_core::audio::InterleavedAudioFrameChunker` and updated host Opus encoding to packetize arbitrary device callback buffers into fixed 20 ms Opus frames before sending. This fixes the previous silent audio path where non-Opus-sized capture buffers were skipped.
  - Runtime smoke, 2026-05-17: `REMOTE_PLAY_EXPECT_AUDIO=1 scripts/smoke_dataplane_media_file.sh` passed locally with `REMOTE_PLAY_DATA_PLANE_MEDIA=1` and `REMOTE_PLAY_FILE_TRANSFER=1`: `data_video=201`, `data_audio=371`, `data_audio_configs=1`, `remote_mic_configs=1`, `remote_system_configs=0`, `legacy_video=0`, `legacy_audio=0`, host-to-client file transfer completed, host scheduled sender reported realtime and reliable sends with zero send errors.

- [ ] Step 3.5: Add clipboard sync on top of the unified data plane.
  - Scope: protocol plus app service layer.
  - Work: support text and image clipboard first, plus file clipboard entries where feasible; include opt-in privacy controls.
  - Verification: two app instances can exchange text clipboard payloads in tests or a local manual smoke test.
  - Progress, 2026-05-17: added protocol-level `ClipboardBundle` with text, image, and file items so clipboard sync is not limited to strings.
  - Progress, 2026-05-17: added `remote_core::clipboard_plane` to encode clipboard bundles as reliable `ContentKind::ClipboardBundle` object chunks with CRC32, then reassemble and decode them on receipt.
  - Progress, 2026-05-17: added `ClipboardSyncPolicy` so text/image clipboard is allowed by default while file clipboard entries require explicit opt-in and all item classes have size caps.
  - Progress, 2026-05-17: added UDP data-path loopback coverage for a mixed text/image/file clipboard bundle.
  - Progress, 2026-05-17: added cross-platform clipboard/file clipboard traits (`ClipboardProvider`, `ClipboardFileStore`) so macOS/Windows/Linux adapters can share the same bundle/data-plane contract.
  - Progress, 2026-05-17: added `MemoryClipboardProvider` for platform-independent clipboard service tests and `FilesystemClipboardFileStore` for small-file clipboard roundtrips with safe file names.
  - Progress, 2026-05-17: added macOS-native `MacClipboardProvider` backed by NSPasteboard for text plus PNG/TIFF image clipboard data. File clipboard remains explicitly unsupported in this backend until file URLs and large-file streaming are wired safely.
  - Progress, 2026-05-17: added `remote_core::clipboard_sync::ClipboardSyncEndpoint`, a platform-neutral sync state machine that polls a `ClipboardProvider`, emits reliable data-plane envelopes, applies received bundles to the peer provider, and suppresses same-content echo loops.
  - Progress, 2026-05-17: documented that clipboard sync runtime integration should feed outgoing bundles through the lane-aware sender as reliable-object traffic and route inbound `ContentKind::ClipboardBundle` data-plane packets back into `ClipboardSyncEndpoint`.
  - Progress, 2026-05-17: moved the macOS clipboard backend into a shared `remote_platform` crate so host/client can use the same platform adapter without putting AppKit/NSPasteboard APIs inside `remote_core`.
  - Progress, 2026-05-17: added `remote_core::clipboard_runtime::run_clipboard_sync`, a reusable runner that polls a provider, handles inbound clipboard envelopes, and sends outgoing reliable clipboard chunks through `ScheduledDataSender`.
  - Progress, 2026-05-17: host and client now have guarded runtime clipboard sync behind `REMOTE_PLAY_CLIPBOARD_SYNC=1`. Host wires it per active stream; client starts/stops it with the selected host; both route inbound `ContentKind::ClipboardBundle` packets to the sync runner.
  - Verification, 2026-05-17: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -W clippy::all` pass. Clippy is currently clean after removing the VideoToolbox callback `transmute`.

- [ ] Step 3.6: Add file transfer on top of the unified data plane.
  - Scope: app service layer and UI.
  - Work: send files with name, size, MIME/type hints, progress, cancel, overwrite behavior, and safe destination handling.
  - Verification: transfer a small file and a large file locally; verify checksum and cancellation behavior.
  - Progress, 2026-05-17: added protocol-level `FileTransferManifest` and `ContentKind::FileManifest` so file metadata is reliable but file chunks remain raw payload bytes.
  - Progress, 2026-05-17: added `remote_core::file_transfer::FileTransferReader`, which computes file size/checksum by streaming and then emits manifest plus fixed-size `FileChunk` envelopes without loading the full file into memory.
  - Progress, 2026-05-17: added `IncomingFileTransfer`, which validates safe file names, writes chunks directly to disk by offset, supports out-of-order chunks, enforces receive policy, and verifies full-file CRC32 on completion.
  - Progress, 2026-05-17: added UDP data-path loopback coverage for manifest plus file chunks through `UdpSender::send_data`.
  - Progress, 2026-05-17: added `remote_core::file_transfer_runtime::run_file_transfer_runtime`, a service runner with `SendFile` commands, outgoing/incoming progress events, scheduled-sender output, inbound manifest/chunk routing, and receive-directory materialization.
  - Progress, 2026-05-17: added runtime loopback coverage for source command -> `ScheduledDataSender` -> UDP data plane -> target runtime -> disk, plus zero-byte file completion after manifest receipt.
  - Progress, 2026-05-17: host/client now start the file-transfer runtime behind `REMOTE_PLAY_FILE_TRANSFER=1`, route inbound `ContentKind::FileManifest`/`FileChunk` packets to it, and support manual smoke sends with `REMOTE_PLAY_SEND_FILE` on the client or `REMOTE_PLAY_HOST_SEND_FILE` on the host.
  - Progress, 2026-05-17: added host/client file-transfer event logging plus `REMOTE_PLAY_FILE_RECEIVE_DIR` override for manual smoke tests.
  - Progress, 2026-05-17: added platform-neutral `ClipboardFileReferenceProvider` and `remote_core::clipboard_file_runtime::run_clipboard_file_sync` so file clipboard references issue file-transfer commands instead of embedding file bytes in clipboard bundles.
  - Progress, 2026-05-17: macOS `MacClipboardProvider` now reads/writes NSPasteboard file URL references, while still keeping file bytes out of the clipboard bundle path.
  - Progress, 2026-05-17: host/client can enable experimental file clipboard with `REMOTE_PLAY_FILE_CLIPBOARD=1`; received files are materialized by file transfer and then written back to the OS clipboard as file references.
  - Progress, 2026-05-17: added protocol-level `FileTransferGroup` metadata so multi-file transfers can preserve group id, file index/count, relative path, aggregate byte count, and group checksum.
  - Progress, 2026-05-17: added `SendFileGroup` runtime commands plus outgoing/incoming group events. The receiver now emits `IncomingGroupCompleted` only after every file in the group has materialized and verified.
  - Progress, 2026-05-17: file clipboard now uses grouped background file transfer and publishes OS clipboard file references atomically after the full group completes. Single-file non-group completion remains supported.
  - Progress, 2026-05-17: `SendFileGroup` now expands directory references recursively into regular file transfers with preserved relative paths, and incoming group completion publishes rebuilt top-level folder paths for OS clipboard use.
  - Progress, 2026-05-17: added protocol-level `FileTransferControl` and `ContentKind::FileControl` for `CancelTransfer` and `CancelGroup`.
  - Progress, 2026-05-17: file-transfer runtime now accepts cancel commands, stops outgoing readers, emits cancelled events, sends remote cleanup controls, and removes partial or unfinished grouped files on the receiving side without publishing clipboard paths.
  - Progress, 2026-05-17: host/client now route `FileControl` packets into the file-transfer runtime and log outgoing/incoming cancellation events.
  - Progress, 2026-05-17: added a client-side transfer-center state reducer plus tests, and surfaced recent transfer progress/status/cancel buttons inside the existing control panel.
  - Progress, 2026-05-17: client control panel now exposes manual `Send Files` and `Send Folder` actions through a cross-platform file dialog. Single-file selections use `SendFile`; multi-file and folder selections use `SendFileGroup`.
  - Progress, 2026-05-17: file-transfer runtime now accepts receive-config updates for destination and overwrite policy. The client control panel shows the current receive folder, lets the user choose a new folder, and toggles overwrite behavior without restarting the stream. Config changes apply immediately when inbound transfer state is idle and wait for the next idle boundary if a file/group is already active.
  - Verification, 2026-05-17: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -W clippy::all` pass. Workspace tests now cover 139 unit tests, and clippy is clean.

- [ ] Step 3.7: Add transfer scheduling and QoS.
  - Scope: `remote_core` transport.
  - Work: implement lane-aware scheduling so realtime packets always beat non-realtime transfers; large file transfers use spare bandwidth with rate limiting/backpressure.
  - Verification: simulated mixed traffic tests plus manual check that file transfer does not stall streaming.
  - Progress: scheduler primitive exists in `remote_core::data_plane::LaneScheduler` with queue capacity and backpressure stats; it is not yet wired into the live UDP sender loop.
  - Progress: added `remote_core/examples/scheduler_mixed_traffic_bench.rs`. Heavy-bulk simulations show realtime packets are sent with 0 tick average/max wait while bulk traffic is backpressured, matching the "bulk uses spare capacity only" rule.
  - Progress: added `remote_core::scheduled_sender::ScheduledDataSender`, an optional live sender worker that batches envelopes, applies `LaneScheduler`, and sends through `UdpSender::send_data`. Loopback coverage verifies realtime data is transmitted before queued bulk when the send budget is constrained.
  - Progress: host `REMOTE_PLAY_DATA_PLANE_MEDIA=1` now submits media envelopes through `ScheduledDataSender`. Submission is non-blocking; if the scheduled sender entrance is full, realtime media is dropped rather than stalling capture/encode. The default RTP-like path is unchanged.
  - Progress: host data-plane media mode now emits per-second scheduled sender stats so local smoke tests can see queue pressure, scheduler drops/rejections, sent packet classes, and send errors while streaming.
  - Progress: added live-sender pressure coverage proving reliable-object backpressure can reject excess bulk while realtime video/audio remain first on the wire.
  - Progress, 2026-05-17: host streaming now shares one scheduled data sender for data-plane media, clipboard sync, and file transfer when those features are enabled for an active stream. A runtime regression verifies the shared sender keeps media ahead of file-transfer traffic while still draining the file transfer to disk.

## Phase 3.8: Cross-Platform Platform Boundaries

- [x] Step 3.8.1: Record cross-platform architecture boundary.
  - Scope: `docs/PLATFORM.md`, `remote_core::traits`.
  - Work: document that macOS, Windows, and Linux implementations must sit behind shared traits for media, clipboard, file clipboard, and input injection.
  - Verification: `cargo test -p remote_core traits`.
  - Progress, 2026-05-17: added platform-neutral frame metadata (`PlatformKind`, `VideoFrameHandleKind`, `VideoPixelFormat`, `AudioSampleFormat`) plus `ClipboardProvider`, `ClipboardFileStore`, and `InputInjector` traits.
  - Progress, 2026-05-17: existing macOS video capture/decode frames now identify their handle kind as `MacosCvPixelBuffer`, and macOS input injection implements the shared input trait.
  - Progress, 2026-05-17: recorded the performance rule that cross-platform support must preserve native fast paths and must not add mandatory realtime media copies.

## Phase 3.9: Bidirectional Audio And Conversation Mode

- [x] Step 3.9.1: Split audio sources and product modes.
  - Scope: protocol/docs/runtime config.
  - Work: distinguish remote system audio, remote microphone audio, and viewer microphone talkback. Keep remote machine audio enabled as part of streaming, and make conversation/talkback an explicit feature toggle.
  - Verification: protocol/config tests for source classification and defaults.
  - Progress, 2026-05-17: added protocol-level `AudioSource`, `AudioDirection`, `AudioCodec`, and `AudioStreamConfig` with explicit remote system, remote microphone, remote mixed, and viewer microphone talkback stream constructors. `docs/PLATFORM.md` now records the two product modes and the cross-platform stream model.

- [x] Step 3.9.2: Add platform-neutral audio stream metadata.
  - Scope: `protocol`, `remote_core::media_plane`, audio runtime helpers.
  - Work: carry audio source, direction, channel count, sample rate, and frame duration without increasing realtime hot-path copies. Preserve the compact realtime path for Opus packets.
  - Verification: round-trip tests for remote-system, remote-mic, and talkback Opus packets.
  - Progress, 2026-05-17: added `ContentKind::AudioStreamConfig` on the interactive-control lane so metadata is sent once per stream while Opus packets stay on the compact realtime `AudioOpus` path. Added media-plane roundtrip tests for remote-system, remote-mic, and viewer-talkback configs plus stream-id mismatch rejection.
  - Progress, 2026-05-17: host data-plane audio now announces its current capture stream as remote microphone audio with sample rate, channel count, and Opus frame duration before sending packets. The headless smoke client now validates that required data-plane audio includes this stream config.
  - Progress, 2026-05-17: client audio playback now keeps Opus decoders per stream id, applies announced mono/stereo layouts, and maps decoded samples into the output device channel count. Legacy RTP-like audio still falls back to 48 kHz stereo.

- [x] Step 3.9.3: Capture remote system audio plus remote microphone.
  - Scope: platform audio adapters.
  - Work: macOS should use native system audio capture where available plus microphone capture; Windows should target WASAPI loopback plus mic; Linux should target PipeWire monitor plus mic. Mixing must be low-latency and avoid unnecessary resampling/copies.
  - Verification: local macOS smoke can observe system-audio packets and mic packets separately or as an explicitly mixed stream.
  - Progress, 2026-05-17: `AudioFrame` now exposes `AudioSource`; macOS microphone frames declare `RemoteMicrophone`, and host audio stream configs are derived from frame source rather than hardcoded.
  - Progress, 2026-05-17: added experimental `MacSystemAudioCapturer` using ScreenCaptureKit `SCStreamOutputType::Audio`, enabled with `REMOTE_PLAY_SYSTEM_AUDIO=1`. It decodes f32/i16 CMSampleBuffer PCM into interleaved f32 frames and sends them as a separate `RemoteSystem` Opus stream (`session_id + 2`) while microphone remains `session_id + 1`.
  - Runtime smoke, 2026-05-17: `REMOTE_PLAY_SYSTEM_AUDIO=1 REMOTE_PLAY_EXPECT_AUDIO=1 scripts/smoke_dataplane_media_file.sh` passed with two audio configs: `remote_mic_configs=1`, `remote_system_configs=1`, `data_audio=745`, `legacy_audio=0`, file transfer completed.

- [ ] Step 3.9.4: Add viewer microphone talkback path.
  - Scope: client capture, host playback, runtime routing.
  - Work: capture the viewer microphone, encode Opus, send over realtime audio lane to the controlled machine, and play it there. Keep it explicitly gated to prevent accidental feedback; GUI mute and push-to-talk controls are handled in Step 3.9.5.
  - Verification: headless loopback test for talkback packets plus manual two-device conversation smoke.
  - Progress, 2026-05-17: added protocol-level stream id helpers for the session audio streams: remote microphone `session_id + 1`, remote system `session_id + 2`, and viewer talkback `session_id + 100`.
  - Progress, 2026-05-17: added a client talkback runtime behind `REMOTE_PLAY_TALKBACK=1`. It captures the viewer microphone through `cpal`, chunks/encodes 20 ms Opus frames without blocking the audio callback, periodically sends a `ViewerMicrophoneTalkback` config, and sends packets through the realtime data-plane audio lane using the scheduled sender.
  - Progress, 2026-05-17: added host talkback routing and playback behind the same flag. The host creates a per-session inbound queue, accepts only active-session `ClientToHost` viewer-talkback configs, decodes Opus with a shared jitter buffer, and maps samples to the output device channel count.
  - Verification, 2026-05-17: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (143 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all` pass. Default data-plane smoke passed with `data_video=208`, `data_audio=366`, `data_audio_configs=1`; system-audio smoke passed with `data_video=202`, `data_audio=723`, `data_audio_configs=2`. Manual two-device conversation smoke is still pending.

- [ ] Step 3.9.5: Add audio controls and safety behavior.
  - Scope: GUI/runtime.
  - Work: expose remote audio mute, remote mic mute, local mic mute, push-to-talk or always-on mode, volume controls, and clear permission/error states.
  - Verification: UI state tests where possible plus manual smoke.
  - Progress, 2026-05-17: added client audio-player settings for remote system audio mute, remote microphone mute, and 0-200% local playback gain. Settings are applied at decode/playback, outside the realtime transport packet format.
  - Progress, 2026-05-17: added talkback GUI modes behind `REMOTE_PLAY_TALKBACK=1`: Off, Always, and PTT. Local mic mute and PTT state gate encoding/sending before Opus packets are produced, so muted talkback does not consume encode bandwidth.
  - Progress, 2026-05-17: added active-session `ControlMessage::AudioControl` for viewer-talkback playback mute/volume on the controlled machine. Host playback applies mute/gain after Opus decode and rejects controls for non-active sessions.
  - Progress, 2026-05-17: talkback stream configs now refresh periodically after start to avoid losing the initial config if it beats host `StartStream` setup.
  - Verification, 2026-05-17: targeted protocol/client/host audio-control tests pass, plus full `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (143 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`. Default and system-audio data-plane smokes both passed; real two-device conversation smoke remains intentionally deferred.
  - Progress, 2026-05-17: polished the client GUI away from demo styling. The host list now uses configured peers from `REMOTE_PLAY_HOSTS` with only `127.0.0.1:39271` as the fallback, the fake LAN host was removed, and the streaming overlay is a compact session HUD with cleaner stream/audio/talkback/transfer controls.
  - Product rule, 2026-05-17: GUI work must be treated as product surface, not demo scaffolding. Keep controls simple, dense, visually consistent, and cross-platform friendly while preserving the native low-latency runtime paths.
  - Verification, 2026-05-17: GUI polish passes `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (144 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`. Default smoke passed with `data_video=201`, `data_audio=362`, `data_audio_configs=1`; system-audio smoke passed with `data_video=198`, `data_audio=716`, `data_audio_configs=2`.

## Phase 4: EasyTier Auto Mesh Networking

- [x] Step 4.1: Decide EasyTier integration mode.
  - Scope: project architecture and packaging.
  - Work: choose between bundled `easytier-core` sidecar process, direct crate/library integration, or system service integration. Default target is bundled sidecar first because it is lower risk and easier to isolate.
  - Verification: short design note in `task.md` or `docs/NETWORKING.md` with chosen mode, tradeoffs, and rollback path.
  - Progress, 2026-05-17: chose bundled `easytier-core` sidecar as the first integration mode and documented the reason in `docs/NETWORKING.md`: low packaging risk, clean rollback to LAN/manual hosts, no changes to the realtime media/data-plane packet path, and portable lifecycle management across macOS/Windows/Linux.

- [ ] Step 4.2: Add mesh identity and pairing model.
  - Scope: new networking/config module.
  - Work: generate or import network name, network secret, node id, display name, and optional invite code/QR payload. Store secrets in the OS keychain where feasible; fall back to app-private config with clear permissions.
  - Verification: unit tests for config generation, serialization, and redaction.
  - Progress, 2026-05-17: added `remote_core::mesh` with generated mesh config, redacted `MeshSecret`, initial EasyTier peers, invite code encode/decode, join-from-invite behavior, and sidecar command planning. Platform-native secure stores are still pending.
  - Progress, 2026-05-17: updated the default EasyTier bootstrap peer to `tcp://public.easytier.top:11010`, matching the current EasyTier v2 public server guidance.
  - Verification, 2026-05-17: `cargo test -p remote_core mesh` passes 4 mesh tests, `cargo check --workspace` passes, `cargo test --workspace` passes 148 tests, and `cargo clippy --workspace --all-targets -- -W clippy::all` is clean.
  - Progress, 2026-05-17: added a platform-neutral mesh persistence layer. `AppPrivateMeshConfigStore` writes non-secret metadata to `mesh.conf`, routes `network_secret` through the `MeshSecretStore` abstraction, and provides `load_or_generate` for automatic first-run initialization.
  - Progress, 2026-05-17: added `AppPrivateMeshSecretStore` as the fallback secret backend. On Unix, fallback directories are written as `0700` and config/secret files as `0600`; macOS Keychain, Windows Credential Manager/DPAPI, and Linux Secret Service can be added later by implementing `MeshSecretStore`.
  - Progress, 2026-05-17: changed the default user-facing invite to `RPM2` copy-code format. It is grouped, case-insensitive, whitespace/hyphen tolerant, checksum-protected, omits the default EasyTier public peer from the encoded payload, and keeps legacy `rpmesh1|...` import compatibility.
  - Verification, 2026-05-17: mesh persistence tests cover redacted metadata serialization, app-private roundtrip, first-run load-or-generate behavior, missing-secret errors, invalid metadata rejection, and restrictive Unix fallback permissions.
  - Verification, 2026-05-17: mesh copy-code tests cover default invite roundtrip, legacy invite compatibility, whitespace/case tolerant import, non-default peer preservation, UTF-8 fallback secrets, and checksum rejection.

- [ ] Step 4.3: Bundle or install EasyTier automatically.
  - Scope: app packaging and startup scripts.
  - Work: ship EasyTier with the app, detect missing binary, install/update it during app setup, and surface permission prompts only when required.
  - Verification: fresh-machine style dry run: app can locate or install EasyTier without manual terminal commands.
  - Progress, 2026-05-17: added a deterministic `EasyTierBinaryLocator` in `remote_core::mesh`. It supports `REMOTE_PLAY_EASYTIER_BIN` override, app-packaged resource locations, current-executable sibling lookup, and `PATH` fallback for development.
  - Progress, 2026-05-17: locator diagnostics now distinguish missing override, non-file path, non-executable Unix binary, and not-found searches without printing mesh secrets. Unit coverage verifies env override priority, bundled-resource priority over `PATH`, missing diagnostics, and executable-bit checks.

- [ ] Step 4.4: Add EasyTier lifecycle manager.
  - Scope: new `mesh` module, app startup/shutdown.
  - Work: start/stop/restart EasyTier with generated config, monitor health, parse assigned virtual IP, and restart on failure.
  - Verification: unit tests for command construction plus manual local launch check.
  - Progress, 2026-05-17: added `EasyTierSidecarLaunchPlan` with redacted debug output and `EasyTierSidecarManager` with start/stop/drop cleanup. The manager is not yet wired into host/client startup by default.
  - Verification, 2026-05-17: `cargo test -p remote_core mesh` passes 10 mesh tests, including command redaction, binary lookup, manager launch planning, and a short-lived sidecar start/stop smoke using a fake executable.
  - Verification, 2026-05-17: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (154 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all` pass after the locator/lifecycle slice.
  - Progress, 2026-05-17: host and client startup now opt into mesh sidecar startup with `REMOTE_PLAY_MESH=1`. When enabled, they load-or-generate app-private mesh identity, locate `easytier-core`, print only redacted launch args, start the sidecar, and keep the process handle alive for the app lifetime. `REMOTE_PLAY_MESH_DIR` can override the fallback config directory during local testing.
  - Progress, 2026-05-17: added `EasyTierHealthSnapshot`, `EasyTierProcessState`, and `EasyTierHealthState` so runtime/GUI code can distinguish starting, ready, degraded, and stopped mesh states without scraping logs.
  - Progress, 2026-05-17: added `EasyTierCliProbeConfig` for a bounded `easytier-cli node` diagnostic plus conservative virtual-IP parsing. `REMOTE_PLAY_EASYTIER_CLI_BIN` can override the CLI path; otherwise the diagnostic looks next to the sidecar binary.
  - Progress, 2026-05-17: added `EasyTierRestartBackoff`, a capped exponential backoff primitive for the upcoming continuous health/restart worker.
  - Verification, 2026-05-17: `cargo test -p remote_core mesh` passes 22 mesh tests covering health snapshots, virtual-IP parsing, CLI probe output parsing, process-exit state, and restart backoff.
  - Verification, 2026-05-17: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (170 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all` pass after the copy-code pairing slice.
  - Progress, 2026-05-17: added `EasyTierHealthMonitorHandle` and `EasyTierHealthMonitorConfig`. A started sidecar now has continuous process polling, bounded CLI virtual-IP probing, watch-based health snapshots, and automatic restart on exit using capped backoff.
  - Progress, 2026-05-19: default app and macOS mesh-daemon health paths now avoid repeated `easytier-cli node` probes. The runtime uses the stable RemotePlay-derived virtual IPv4 for discovery/health and keeps CLI probing only as an explicit diagnostic/test capability, reducing crash-report risk on macOS.
  - Progress, 2026-05-17: host/client now move the started EasyTier sidecar into the health monitor, keeping automatic lifecycle management alive for the app lifetime while still passing the startup virtual IP into discovery immediately.
  - Verification, 2026-05-17: `cargo test -p remote_core mesh` passes 28 mesh tests, including health monitor ready-state polling and automatic restart of an exited fake sidecar.
  - Progress, 2026-05-17: the client Devices screen now subscribes to the mesh health snapshot and shows a compact Mesh starting/ready/degraded/stopped status row with the virtual IP when ready.
  - Verification, 2026-05-17: full quality gate passes: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (181 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Pending: build the create/join device-group onboarding UI.

- [ ] Step 4.5: Add peer discovery over the mesh.
  - Scope: protocol/client/host discovery.
  - Work: advertise local availability on the EasyTier virtual network and discover paired peers without hardcoded `127.0.0.1` or LAN IPs.
  - Verification: two local app instances or two machines can find each other by paired identity.
  - Decision, 2026-05-17: use a lightweight RemotePlay UDP discovery payload first. It can run over LAN broadcast and over the EasyTier virtual network with the same packet model. mDNS/Bonjour remains a possible platform-native adapter later, but it is not needed for the first reliable slice.
  - Progress, 2026-05-17: added `remote_core::discovery` with `DiscoveryAnnouncement`, capability bits, LAN/mesh scope, TTL, endpoint derivation, and bounded binary encoding under the `RPDISC1` magic. Discovery packets include device identity and control endpoint data, but never include mesh secrets or invite material.
  - Verification, 2026-05-17: `cargo test -p remote_core discovery` passes tests for announcement roundtrip, endpoint derivation, malformed packet rejection, validation, and peer TTL expiry.
  - Verification, 2026-05-17: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (175 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all` pass after the discovery payload slice.
  - Progress, 2026-05-17: added `DiscoveryPeerCache`, `DiscoveryEvent`, snapshots, and `run_discovery_runtime`. The runtime binds UDP, periodically sends announcements to configured LAN/mesh targets, listens for peers, filters out other networks and self, prunes expired peers by TTL, and exposes both event and watch snapshot channels.
  - Verification, 2026-05-17: discovery runtime loopback test proves two local runtimes can discover each other over UDP unicast and derive the correct control endpoints without using real LAN broadcast.
  - Verification, 2026-05-17: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (177 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all` pass after the discovery runtime slice.
  - Progress, 2026-05-17: host and client now start discovery behind `REMOTE_PLAY_DISCOVERY=1`. Host advertises stream capability on control port `39271`; client advertises viewer capability and merges discovered peers into the Devices screen while preserving `REMOTE_PLAY_HOSTS` and loopback fallback entries.
  - Note, 2026-05-17: two separate processes on the same machine now attempt shared discovery-port binding for local development; the unified dual-role app should still collapse this to one owned socket later.
  - Progress, 2026-05-17: discovery sockets now use address reuse plus Unix port reuse, and `REMOTE_PLAY_DISCOVERY_PORT` can override the default `38117` port for development or deployments with a reserved port policy.
  - Progress, 2026-05-17: viewer-only discovery announcements can use `control_port=0`; the client Devices screen filters discovered rows to stream-capable peers with a real control endpoint, so passive viewers do not show as broken connection targets.
  - Progress, 2026-05-17: host/client now carry the EasyTier virtual IP into discovery announcements and mark those packets as mesh scoped when the virtual IP is known. Receivers then connect to the advertised virtual IP while retaining LAN fallback if EasyTier is still starting.
  - Verification, 2026-05-17: `cargo test -p remote_core discovery` passes 9 discovery tests including port parsing, shared-port binding, and loopback runtime discovery. `cargo check --workspace` passes after host/client integration.
  - Verification, 2026-05-17: full quality gate passes: `cargo fmt --check`, `cargo test --workspace` (179 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.

- [ ] Step 4.6: Make setup extremely simple.
  - Scope: UI and onboarding.
  - Work: one-click "Create device group", "Join device group", invite code/QR flow, automatic mesh initialization on app launch, clear state labels for connecting/ready/error.
  - Verification: a user can install, launch, pair two devices, and connect without manually editing EasyTier config.
  - Progress, 2026-05-17: added client-side `MeshPairingControl`, which loads or creates the app-private mesh identity on startup, exposes a copyable `RPM2` invite code, saves imported invite codes, creates fresh device groups, and keeps status/error messages in a small snapshot for the GUI.
  - Progress, 2026-05-17: the client Devices screen now includes a compact Device Group panel with `Copy Code`, `Join Clipboard`, and `New Group`. `Join Clipboard` reads the OS clipboard, validates the invite, saves the new group, and keeps the local display name/device identity distinct from the inviter.
  - Progress, 2026-05-17: when `REMOTE_PLAY_MESH=1` is active, a successful join/new-group action now sends a mesh reload request. The client drops the old EasyTier monitor, starts a new sidecar from the saved config, and bridges the new health snapshots into the same GUI status channel.
  - Verification, 2026-05-17: `cargo test -p client mesh_pairing` passes 4 tests covering persistent invites, joining a group, invalid invite handling, and reload signaling.
  - Verification, 2026-05-17: full quality gate passes: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (185 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-19: the unified `remote_play` GUI now loads the same app-private device-group pairing control and shows a Device Group panel with copy-code, join-from-clipboard, and new-group actions. The unified runtime exposes the pairing control alongside mesh/discovery state so first-run binding is no longer stranded in the legacy client GUI.
  - Verification, 2026-05-19: `cargo fmt --check`, `cargo check --workspace`, `cargo test -p client`, `cargo test -p remote_play_app`, `cargo test --workspace` (222 tests), `cargo clippy --workspace --all-targets -- -W clippy::all`, and `git diff --check` pass after the unified pairing GUI slice.
  - Progress, 2026-05-19: unified pairing actions now hot-reload runtime networking. The unified owner keeps stable mesh-health and discovery-snapshot watch channels while the underlying EasyTier monitor and discovery runtime can be replaced. `Copy Code` remains local, while `Join Clipboard` and `New Group` persist the new device group, show a network-services refresh message, send a reload signal, clear stale discovered peers, restart discovery with the saved group identity, and restart EasyTier when mesh is enabled.
  - Verification, 2026-05-19: focused tests cover discovery being rebuilt from a newly saved device group and the unified pairing control triggering the reload channel without requiring an app restart.
  - Verification, 2026-05-19: full quality gate passes after unified pairing hot-reload: `cargo fmt --check`, `cargo check --workspace`, `cargo test -p client`, `cargo test -p remote_play_app`, `cargo test --workspace` (224 tests), `cargo clippy --workspace --all-targets -- -W clippy::all`, and `git diff --check`.

## Phase 5: Unified Dual-Role Application

- [ ] Step 5.1: Define the unified runtime model.
  - Scope: crate layout and process model.
  - Work: decide whether to merge `host` and `client` into one binary or add a new `remote_play` app crate that embeds both roles.
  - Verification: design note with crate ownership and migration steps.
  - Decision, 2026-05-17: add a unified app crate later rather than hard-merging the existing `host` and `client` binaries immediately. Keep `host` and `client` as compatibility/debug entry points while extracting their reusable runtime services first. The unified app should own one mesh lifecycle, one discovery socket, one device list, and a role state machine where idle/passive means stream-capable and active selection means viewer.
  - Migration order, 2026-05-17: extract host passive service, extract client session service, then create the unified app shell that embeds both services and prevents conflicting local sessions.
  - Progress, 2026-05-18: added a new workspace crate `remote_play_app` under `app/` with a `remote_play` binary placeholder and a tested library shell. The existing `host` and `client` binaries remain intact for compatibility while the unified entry point grows separately.
  - Verification, 2026-05-18: full quality gate passes with the new workspace member: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (205 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-18: added `remote_play_app::UnifiedServiceOwner`, a unified lifecycle owner for one app runtime, optional EasyTier mesh monitor, one discovery runtime, one passive host service task, and one client receiver task. Discovery snapshots are bridged back into the app device list automatically, and owned background tasks/cancel handles are cleaned up on drop.
  - Progress, 2026-05-18: the `remote_play` binary now initializes the unified service owner in a safe default mode that starts no network services until configured, avoiding surprise port binding or sidecar startup during this migration phase.
  - Verification, 2026-05-18: `cargo test -p remote_play_app` now covers the owner starting empty, starting discovery, and applying discovery snapshots into the owned runtime device list.
  - Verification, 2026-05-18: full quality gate passes after the unified owner slice: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (208 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-18: added unified connection orchestration on `UnifiedServiceOwner`. Selecting a streamable device now requires a configured client control sender, allocates a session through the role state machine, sends `ControlMessage::StartStream`, starts optional clipboard/file/talkback side-service controls for the target, and remains in `Connecting` until `mark_viewing_connected` is called by future media/telemetry integration.
  - Progress, 2026-05-18: added unified disconnect orchestration. Active viewing sends `ControlMessage::StopStream` when a control sender is available, stops side-service controls, and returns the role state to `Idle`.
  - Verification, 2026-05-18: `cargo test -p remote_play_app` now covers `StartStream` emission, no-sender rejection without role mutation, explicit connected marking, and `StopStream` emission on disconnect.
  - Verification, 2026-05-18: full quality gate passes after the connection orchestration slice: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (211 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-18: added `client::ClientSessionEvent` and optional event emission from the client session receiver. Host telemetry and accepted media packets can now be forwarded without changing the legacy client binary behavior.
  - Progress, 2026-05-18: `UnifiedServiceOwner` can now bridge client session events back into the role runtime. Matching media or host telemetry for the active connecting session marks it as `Viewing`, matching viewing media/telemetry refreshes activity, and mismatched old-session media is ignored.
  - Verification, 2026-05-18: focused tests cover client session event emission, telemetry forwarding, owner media-event connection promotion, telemetry-event connection promotion, and mismatched media rejection.
  - Verification, 2026-05-18: full quality gate passes after the client-event bridge slice: `cargo fmt --check`, `cargo check --workspace`, `cargo test -p client`, `cargo test -p remote_play_app`, `cargo test --workspace` (217 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-18: added owner-driven session timeout cleanup. `UnifiedServiceOwner::expire_timed_out` now applies the role-state timeout through the owner and clears viewer-side runtime state, and an optional `UnifiedSessionTimeoutMonitorConfig` can poll this automatically in the unified app.
  - Verification, 2026-05-18: focused timeout tests cover explicit owner timeout cleanup and the optional background monitor expiring a stalled connecting session.
  - Verification, 2026-05-18: full quality gate passes after the owner timeout slice: `cargo fmt --check`, `cargo check --workspace`, `cargo test -p remote_play_app`, `cargo test --workspace` (219 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-19: added `UnifiedRuntimeConfig` and `start_unified_runtime` so the unified app can assemble one EasyTier mesh owner, LAN/mesh discovery, passive host service, client receiver, client session event bridge, side-service controls, and timeout monitor from a single runtime config. The `remote_play` binary now starts this unified runtime from environment settings and stays alive until Ctrl-C.
  - Verification, 2026-05-19: focused tests cover dual-role discovery announcement construction and starting the core unified services on ephemeral test ports.
  - Verification, 2026-05-19: full quality gate passes after the unified runtime launcher slice: `cargo fmt --check`, `cargo check --workspace`, `cargo test -p remote_play_app`, `cargo test --workspace` (221 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-19: added the first unified GPUI shell for the `remote_play` binary. The default entry point now opens a clean dashboard with device rows, role status, mesh health, active-session details, and connect/disconnect actions wired to `UnifiedServiceOwner`; `REMOTE_PLAY_HEADLESS=1` keeps the previous non-GUI runtime mode for smoke and automation.
  - Verification, 2026-05-19: `cargo test -p remote_play_app` passes after the GUI shell, covering the existing owner/runtime behavior while the new UI compiles through the app crate.
  - Verification, 2026-05-19: full quality gate passes after the unified GUI shell: `cargo fmt --check`, `cargo check --workspace`, `cargo test -p remote_play_app`, `cargo test --workspace` (221 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-19: wired the unified GUI into the real viewer media path. The client crate now exposes a reusable `ClientMediaRuntime` plus a decoded-frame GPUI surface helper, and `start_unified_runtime` uses it in GUI mode so received media is decoded, audio is played, and latest video frames are rendered in the unified session panel. Headless mode disables viewer media and keeps lightweight sinks for automation.
  - Verification, 2026-05-19: `cargo test -p client` and `cargo test -p remote_play_app` pass after the media-runtime integration.
  - Verification, 2026-05-19: full quality gate passes after the unified GUI media path: `cargo fmt --check`, `cargo check --workspace`, `cargo test -p client`, `cargo test -p remote_play_app`, `cargo test --workspace` (221 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-19: hardened unified viewer-media startup. Audio output initialization can now fail without preventing the unified GUI or video decode path from starting; the runtime exposes a viewer-media status, falls back to sink queues only when the whole media runtime is unavailable, and the GUI surfaces media/audio readiness in the dashboard. The audio player now starts its decode task only after the platform output stream is successfully built and played.
  - Verification, 2026-05-19: `cargo fmt --check`, `cargo check --workspace`, `cargo test -p client`, `cargo test -p remote_play_app`, `cargo test --workspace` (222 tests), `cargo clippy --workspace --all-targets -- -W clippy::all`, and `git diff --check` pass after the media startup fallback slice.

- [ ] Step 5.2: Extract reusable host streaming service.
  - Scope: `host/src/main.rs` and host modules.
  - Work: turn passive streaming behavior into a service object that can run inside the unified app.
  - Verification: `cargo check --workspace`; host-only behavior preserved.
  - Progress, 2026-05-17: added `host/src/service.rs` with `HostServiceConfig` and `run_host_service`. `host/src/main.rs` now handles process-level startup (mesh, discovery, stats) and delegates the passive control loop/stream spawning to the service boundary.
  - Verification, 2026-05-17: `cargo fmt --check` and `cargo check -p host` pass after the extraction.
  - Verification, 2026-05-17: full quality gate passes: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (185 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-18: converted `host` into a library plus thin binary. The existing binary now calls `host::run_host_binary()`, while `HostServiceConfig` and `run_host_service` are public and re-exported for the unified app.
  - Progress, 2026-05-19: retired the standalone `host` binary entry point. The `host` crate remains as the internal passive-streaming library used by the unified `remote_play` app.
  - Verification, 2026-05-18: `cargo check -p host`, `cargo test -p host`, and `cargo check -p remote_play_app` pass after importing the host service boundary into `remote_play_app`.
  - Verification, 2026-05-18: full quality gate passes after the host library split: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (205 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.

- [ ] Step 5.3: Extract reusable client session service.
  - Scope: `client/src/main.rs`, `client/src/render.rs`, client modules.
  - Work: separate "active connection/render" behavior from binary startup.
  - Verification: `cargo check --workspace`; client-only behavior preserved.
  - Progress, 2026-05-17: added `client/src/session.rs` with `ClientSessionReceiverConfig` and `spawn_client_session_receiver`. The UDP receiver loop, media/data-plane demux, host telemetry updates, audio stream config handling, and video jitter/decode queueing now live behind a session receiver boundary instead of inside `client/src/main.rs`.
  - Verification, 2026-05-17: `cargo check -p client` passes after the receiver extraction.
  - Verification, 2026-05-17: full quality gate passes: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (185 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-18: converted `client` into a library plus thin binary. The existing binary now calls `client::run_client_binary()`, while `ClientSessionReceiverConfig` and `spawn_client_session_receiver` are public and re-exported for `remote_play_app`.
  - Progress, 2026-05-19: retired the standalone `client` binary entry point. The `client` crate remains as the internal viewer/media/session library used by the unified `remote_play` app.
  - Verification, 2026-05-18: `cargo check -p client`, `cargo test -p client`, `cargo clippy -p client --all-targets -- -W clippy::all`, and `cargo test -p remote_play_app` pass after importing the client session boundary into `remote_play_app`.
  - Verification, 2026-05-18: full quality gate passes after the client library split: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (205 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.

- [ ] Step 5.4: Implement role state machine.
  - Scope: unified app.
  - Work: idle app is passively available as a streamer; when user actively selects a peer, it becomes viewer for that session. Prevent conflicting local stream/view sessions.
  - Verification: unit tests for role transitions and manual local smoke test.
  - Progress, 2026-05-18: added `remote_core::role` with `RoleStateMachine`, `RolePeer`, `RoleSession`, explicit `Idle`/`Connecting`/`Viewing`/`Serving` states, default conflict rejection, an opt-in inbound conflict policy, activity refresh, stop handling, and connection/session timeouts.
  - Verification, 2026-05-18: `cargo test -p remote_core role` passes 12 focused tests covering active viewing, inbound serving, same-device session replacement, busy conflicts, explicit inbound takeover policy, wrong-session rejection, stop behavior, and timeout-to-idle transitions.
  - Verification, 2026-05-18: full quality gate passes: `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (197 tests), and `cargo clippy --workspace --all-targets -- -W clippy::all`.
  - Progress, 2026-05-18: `remote_play_app::UnifiedAppRuntime` now owns a `RoleStateMachine` and exposes connect, inbound-stream accept, activity, stop, and timeout APIs. This gives the future GUI/runtime one place to decide whether the local process is idle, actively viewing, or passively serving.

- [ ] Step 5.5: Replace fixed host list with device list.
  - Scope: UI and discovery.
  - Work: show paired EasyTier peers, online/offline state, virtual IP, latency, and "Connect" action.
  - Verification: app can connect by device identity rather than hardcoded address.
  - Progress, 2026-05-18: `remote_play_app` now consumes `DiscoveryPeerSnapshot` into an app-level device list, filters stream-capable online devices, marks missing peers offline, preserves LAN/mesh scope, and starts viewing by device identity rather than raw host list entry.

## Phase 6: App Maintainability

- [ ] Step 6.1: Extract stream settings and session commands.
  - Scope: `client/src/render.rs`, new small module if useful.
  - Work: centralize repeated `StartStream` construction for resolution/FPS/bitrate changes.
  - Verification: `cargo check --workspace`; UI behavior unchanged.

- [ ] Step 6.2: Split rendering panel code from session state.
  - Scope: `client/src/render.rs`.
  - Work: reduce the nearly 1000-line render file into smaller responsibilities.
  - Verification: `cargo check --workspace`; manual client launch if feasible.

- [ ] Step 6.3: Remove dead UI path or integrate `host_list.rs`.
  - Scope: `client/src/host_list.rs`, `client/src/render.rs`.
  - Work: either use the existing `HostListView` or replace it with the new unified device list.
  - Verification: `cargo check --workspace`.

## Phase 7: Input Path Completion

- [ ] Step 7.1: Define client input capture behavior.
  - Scope: `client/src/render.rs`, `protocol/src/lib.rs`.
  - Work: map keyboard/mouse events into `InputEvent` with clear coordinate semantics.
  - Verification: compile and protocol tests.

- [ ] Step 7.2: Implement host mouse injection correctly.
  - Scope: `host/src/input_injector.rs`.
  - Work: fetch current cursor position, implement mouse down/up buttons, document permissions.
  - Verification: `cargo check --workspace`; manual host/client input check.

- [ ] Step 7.3: Add input safety controls.
  - Scope: client/host.
  - Work: avoid injecting input unless active stream/session is valid; add disconnect/timeout behavior.
  - Verification: manual session checks and unit tests where possible.

## Phase 8: Unsafe Boundary Review

- [ ] Step 8.1: Document CoreFoundation/CoreVideo ownership contracts.
  - Scope: `host/src/video_encode.rs`, `client/src/video_decode.rs`, `client/src/render.rs`.
  - Work: add narrow comments only where retain/release or callback lifetime is non-obvious.
  - Verification: `cargo check --workspace`.

- [ ] Step 8.2: Wrap retained pixel buffers in safer local abstractions.
  - Scope: `client/src/video_decode.rs`, `client/src/render.rs`.
  - Work: reduce raw pointer handling in higher-level render code.
  - Verification: `cargo check --workspace`; manual stream check.

- [ ] Step 8.3: Review callback refcon allocation.
  - Scope: `host/src/video_encode.rs`, `client/src/video_decode.rs`.
  - Work: make callback sender ownership intentional and leak-free.
  - Verification: `cargo test --workspace`; manual start/stop stream cycles.

## Phase 9: Product Documentation And Operations

- [ ] Step 9.1: Add README.
  - Scope: `README.md`.
  - Content: purpose, platform support, prerequisites, permissions, signing, EasyTier mesh setup, run commands, known limitations.
  - Verification: fresh-read checklist.

- [ ] Step 9.2: Improve launch scripts.
  - Scope: `start.sh`, `setup_stable_signing.sh`.
  - Work: clearer errors, dependency checks, log locations, optional target selection, EasyTier sidecar checks.
  - Verification: shell syntax check and manual dry run where safe.

- [ ] Step 9.3: Add CI-style local verification command.
  - Scope: docs/scripts.
  - Work: one command for fmt/check/test/clippy.
  - Verification: command completes locally.

## Phase 10: Runtime Validation

- [ ] Step 10.1: Manual local loopback smoke test.
  - Scope: unified app launch.
  - Check: app starts, passive streamer is available, active viewer can connect locally, telemetry updates, stop works.
  - Progress, 2026-05-17: added and ran a headless host/client protocol smoke for data-plane video plus file transfer. Full GPUI client smoke remains pending.

- [ ] Step 10.2: Reconfiguration smoke test.
  - Scope: unified app control panel.
  - Check: resolution, FPS, and bitrate changes restart session cleanly without stale packets.

- [ ] Step 10.3: Disconnect and heartbeat smoke test.
  - Scope: unified session lifecycle.
  - Check: stop button and heartbeat timeout both stop streaming.

- [ ] Step 10.4: Cross-network EasyTier smoke test.
  - Scope: two devices on different networks.
  - Check: install/launch initializes EasyTier automatically, devices pair, peers discover each other, stream starts over virtual IP, reconnection works after app restart.
  - Progress, 2026-05-19: added `scripts/package_macos_unified.sh` and produced a signed macOS arm64 app zip with bundled EasyTier binaries. The package was copied to the second Mac over SSH, unzipped, and verified with `codesign --verify --deep --strict`.
  - Progress, 2026-05-19: fixed the EasyTier sidecar launch arguments in `remote_core::mesh`; EasyTier 2.6.4 requires explicit boolean values such as `--dhcp true`, `--latency-first true`, and `--private-mode true`.
  - Verification, 2026-05-19: `cargo fmt --check` and `cargo test -p remote_core mesh -- --nocapture` pass after the EasyTier launch fix.
  - Finding, 2026-05-19: two-machine EasyTier sidecars can start and establish a peer relationship when given a reachable initial peer, but macOS refuses TUN/utun creation from the normal app user (`Operation not permitted`) as soon as a real virtual IPv4 is assigned. Full virtual-IP streaming therefore requires a privileged install step/helper, a documented admin launch path, or a no-TUN transport design before this smoke can pass end to end.
  - Finding, 2026-05-19: direct public UDP to the remote Mac on the default control port did not reach the passive host in the current network, so SSH-only access is insufficient for a raw UDP smoke without either EasyTier virtual routing or a purpose-built UDP relay.
  - Progress, 2026-05-19: changed the default RemotePlay control/data-plane bind port from the common development port to `39271`; the port remains overrideable with `REMOTE_PLAY_HOST_BIND_ADDR` and smoke clients can still override with `REMOTE_PLAY_SMOKE_HOST_ADDR`.

## Phase 11: macOS Mesh Installation

- [ ] Step 11.1: Add a privileged EasyTier bootstrap path.
  - Scope: macOS packaging, launch/install flow, sidecar lifecycle.
  - Work: decide between installer-time privileged helper, launch daemon, or explicit admin setup command for creating the EasyTier TUN/utun path.
  - Verification: non-developer app launch can obtain a real EasyTier virtual IPv4 and report it through the existing health monitor.
  - Progress, 2026-05-19: added hidden maintenance commands to the app binary: `--mesh-ensure-config` creates or loads the private mesh config, and `--mesh-launchd-plist <label>` renders a root LaunchDaemon plist using the exact EasyTier sidecar launch plan.
  - Progress, 2026-05-19: added packaged macOS admin scripts, `install_macos_mesh_daemon.sh` and `uninstall_macos_mesh_daemon.sh`, to install/remove `/Library/LaunchDaemons/com.remoteplay.mesh.plist` with the bundled EasyTier binary.
  - Progress, 2026-05-19: tightened the LaunchDaemon install to write the plist as root-only (`0600`) because the EasyTier launch arguments include the mesh network secret.
  - Progress, 2026-05-19: packaged app launches now auto-enable EasyTier when the bundled `easytier-core` is discoverable, while development runs still stay opt-in if no EasyTier binary is present.
  - Progress, 2026-05-19: when `/Library/LaunchDaemons/com.remoteplay.mesh.plist` exists, the unified app treats the system daemon as the authoritative EasyTier sidecar, uses the expected stable virtual IP for discovery status, and avoids launching either a second unprivileged user sidecar or repeated `easytier-cli` health probes.
  - Verification, 2026-05-19: dry-run plist generation passes `plutil -lint` and includes the stable `--ipv4` launch argument plus sidecar log redirection.
  - Verification, 2026-05-19: macOS package `RemotePlay-macos-arm64.zip` was rebuilt with SHA256 `e23a7bb8a729b53a3552094f7edadc4b122867d5310d26c64d16cb1bc79bee99`, copied to the second Mac, unzipped, code-sign verified, and its generated LaunchDaemon plist passed `plutil -lint`.

- [ ] Step 11.2: Surface mesh permission state in GUI.
  - Scope: unified app GUI and mesh health model.
  - Work: show a clear mesh setup action when EasyTier is running but no virtual IP can be assigned due missing privileges.
  - Verification: app distinguishes "sidecar connected but no virtual IP" from "sidecar failed to start".
  - Progress, 2026-05-19: added `EasyTierHealthIssue::RequiresAdminPrivileges`, captured EasyTier sidecar stdout/stderr into `easytier-sidecar.log`, and classified TUN/utun permission failures into a structured health snapshot issue.
  - Progress, 2026-05-19: switched sidecar launch from EasyTier DHCP to a stable RemotePlay-derived virtual IPv4 in `10.128.0.0/10`, based on each device `node_id`. This avoids waiting indefinitely for DHCP and gives privileged installs a stable address while surfacing missing macOS privileges immediately.
  - Progress, 2026-05-19: updated the unified app UI and legacy client renderer to show `Mesh needs admin setup` when EasyTier exits because macOS refuses virtual network adapter creation.
  - Progress, 2026-05-19: the unified GUI now shows a compact `Setup Mesh` action next to that status. On macOS it opens the packaged installer through the native administrator prompt, then requests a runtime networking refresh so the app can switch to the privileged system daemon without requiring a manual restart when the reload channel is available.
  - Progress, 2026-05-19: the Device Group panel now also exposes `Setup Mesh`, so users can explicitly reinstall/upgrade the privileged mesh setup even when the health state is not currently showing a permission error.
  - Progress, 2026-05-19: added a small `mesh_admin` module with tests for packaged installer path discovery, shell/AppleScript quoting, and administrator-prompt cancellation detection.
  - Verification, 2026-05-19: `cargo test -p remote_core mesh -- --nocapture` passes with coverage for TUN permission diagnostics and sidecar log capture.
  - Verification, 2026-05-19: full quality gate passes after the GUI setup action: `bash -n` for the macOS install/uninstall scripts, `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (233 tests), `cargo clippy --workspace --all-targets -- -W clippy::all`, and `git diff --check`.

- [ ] Step 11.3: Keep privileged daemon synchronized with device-group changes.
  - Scope: macOS LaunchDaemon and hidden maintenance runtime.
  - Work: avoid static EasyTier arguments in the root plist; make the privileged daemon read the user's current mesh config and automatically restart EasyTier when `Join Clipboard` or `New Group` changes that config.
  - Verification: generated plist must not contain `--network-secret`; joining a group should not require reinstalling the daemon to use the new group.
  - Finding, 2026-05-19: local and remote user mesh configs already had the same `network_name` after importing the remote code, so the Join flow itself succeeded. The remaining mismatch was that the root LaunchDaemon still had the old group baked into its static plist arguments.
  - Progress, 2026-05-19: added `remote_play --mesh-daemon-run`. The LaunchDaemon now starts this hidden root daemon instead of `easytier-core` directly. The daemon loads the user's `mesh.conf`/secret, starts EasyTier with the current group, polls for config-key changes every two seconds, and restarts EasyTier when the group changes.
  - Progress, 2026-05-19: `--mesh-launchd-plist` now renders a dynamic plist with `ProgramArguments=[remote_play, --mesh-daemon-run]` and environment variables for `REMOTE_PLAY_MESH_DIR`, `REMOTE_PLAY_DISPLAY_NAME`, and `REMOTE_PLAY_EASYTIER_BIN`. It no longer writes mesh network name or secret into the root plist.
  - Verification, 2026-05-19: generated dynamic plist passes `plutil -lint`, contains `--mesh-daemon-run`, contains `REMOTE_PLAY_EASYTIER_BIN`, and does not contain `--network-secret` or `--network-name`.
  - Progress, 2026-05-19: updated the macOS package script to clear provenance/quarantine-style xattrs before signing, fixing a local `codesign` `Operation not permitted` failure seen while replacing the app signature.
  - Verification, 2026-05-19: rebuilt macOS package `RemotePlay-macos-arm64.zip` with SHA256 `1b02cbd740548ab4af38e458fe65f3efbaacc7f437a31b8b92c7de4adb3137f5`, copied it to the second Mac, unzipped it, verified code signing, and confirmed the remote package generates the dynamic no-secret LaunchDaemon plist.
  - Progress, 2026-05-19: renamed the macOS package output to `RemotePlay Unified.app` / `RemotePlay Unified-macos-arm64.zip` so launching by name cannot collide with Sony PS Remote Play.
  - Verification, 2026-05-19: rebuilt `RemotePlay Unified-macos-arm64.zip` with SHA256 `0a24a3e0ba1d7bdc0f535fa9bd607e7b28e8c227c76d876fe24906db1a545967`, verified code signing locally and on the second Mac, and confirmed packaged headless startup listens on the new default control port `0.0.0.0:39271` on both machines.
  - Progress, 2026-05-19: the unified GUI now always shows a top-bar Mesh maintenance action. It reads `Repair Mesh` after initial setup and `Setup Mesh` only when the health snapshot reports missing admin setup, so already-configured machines can still reinstall the dynamic LaunchDaemon.
  - Verification, 2026-05-19: rebuilt and redeployed `RemotePlay Unified-macos-arm64.zip` with SHA256 `3ee63ff041384b08fe36e2da81d63353cba4b4a6ff5f0a9c4c6c485e24a350db`; local and remote code-sign verification passed.
  - Progress, 2026-05-19: cleaned obsolete generated app bundles and zips from the local build outputs and the second Mac's `~/remote_play_test`, leaving only the current `RemotePlay Unified` package. Existing `/Applications/RemotePlay.app` was confirmed to be Sony's `com.playstation.RemotePlay` and was not touched.
  - Progress, 2026-05-19: hardened `install_macos_mesh_daemon.sh` for repair installs. The script now recognizes `RemotePlay Unified.app`, no longer calls `sudo -u` from the privileged install path, restores mesh config ownership/permissions to the desktop user, and emits a line-number failure hint for GUI/admin-prompt errors.
  - Progress, 2026-05-19: the GUI now preserves the first line of the mesh setup error in the status text instead of only showing `Mesh setup failed`, so the next failed repair attempt should expose the actionable cause.
  - Verification, 2026-05-19: rebuilt and redeployed `RemotePlay Unified-macos-arm64.zip` with SHA256 `a8da8fbffa7c6f535297e25b6520f6e75d03ac3a35d8985f3b877aadb55e799a`; local and remote code-sign verification passed, and the packaged installer script no longer contains `sudo -u`.
  - Verification, 2026-05-19: full quality gate passes after the dynamic daemon and legacy-entry cleanup: `bash -n` scripts, `cargo fmt --check`, `cargo check --workspace`, `cargo test --workspace` (234 tests), `cargo clippy --workspace --all-targets -- -W clippy::all`, and `git diff --check`.
  - Finding, 2026-05-20: two-machine headless RemotePlay loopback isolates the failure below the app protocol. Local headless host/client succeeds with video/audio/telemetry; remote headless loopback receives `StartStream` but macOS TCC denies screen capture; cross-machine headless over EasyTier virtual IP does not deliver `StartStream` in either direction.
  - Finding, 2026-05-20: EasyTier sidecars run and expose virtual IPs, but `easytier-cli peer` lists only the local node, routes to the peer virtual IP go through the physical default gateway, and shared-node handshakes to `public.easytier.top/.cn:11010` are repeatedly closed or time out.
  - Progress, 2026-05-20: updated the mesh default public endpoint from `public.easytier.top` to `public.easytier.cn`, added config migration for stored legacy default peers, disabled `--private-mode` for the default shared-node path, and changed stable virtual IP derivation so all devices in one RemotePlay group share the same EasyTier `/24`.
  - Verification, 2026-05-20: rebuilt and redeployed `RemotePlay Unified-macos-arm64.zip` with SHA256 `c9f274f8c5e278dcbd418d3d151ae0b01aa75b122632140744d091c6885e91c4`; local and remote code-sign verification passed; `cargo fmt --check`, `cargo check --workspace`, `cargo test -p remote_core mesh::tests::`, `cargo test -p remote_play_app`, `cargo clippy --workspace --all-targets -- -W clippy::all`, and `git diff --check` pass.
  - Verification, 2026-05-20: triggered the macOS admin repair locally through the system authorization prompt. Both Macs are now running the new daemon arguments with no `--private-mode` and matching virtual subnet addresses: local `10.154.60.163/24`, remote `10.154.60.14/24`.
  - Finding, 2026-05-20: after repair, macOS routes both peer virtual IPs through `utun`, but EasyTier still has no remote peer in `easytier-cli peer`, ping is 100% loss, and cross-machine headless RemotePlay still does not deliver `StartStream`. Logs continue to show the public shared node closing the EasyTier handshake.

## Current Recommended Next Step

Move to a RemotePlay-owned relay/rendezvous fallback instead of depending on the community public EasyTier shared node. The app protocol and privileged TUN setup are now isolated from the remaining failure; the missing piece is reliable peer discovery/relay infrastructure.

## Phase 12: RemotePlay-Owned Relay Fallback

- [x] Step 12.1: Add transparent relay primitives.
  - Scope: `remote_core::relay`, relay examples.
  - Work: added UDP relay server/tunnel and TCP relay server/tunnel. The local tunnel preserves the existing RemotePlay UDP packet format between app and tunnel, then wraps packets for relay forwarding.
  - Verification, 2026-05-27: `cargo test -p remote_core relay::tests::` passes for UDP and TCP relay tunnels forwarding existing `UdpMultiplexer` control packets both ways; `cargo build -p remote_core --examples` passes.

- [x] Step 12.2: Two-machine headless relay smoke.
  - Scope: remote Mac over SSH-accessible test path.
  - Work: ran a TCP relay server on the remote Mac, connected both local and remote relay tunnels through an SSH local port forward, ran local headless host and remote headless client.
  - Verification, 2026-05-27: remote headless client completed successfully through the relay path with `legacy_video=120`, `legacy_audio=201`, and `telemetry=4`.
  - Finding, 2026-05-27: direct UDP to the remote test machine's arbitrary relay port did not arrive; direct TCP to `198.18.0.36:<port>` connected at the socket layer but did not reach the remote relay process. The SSH local port forward proves the relay protocol works, but a real public relay endpoint is needed for product testing without SSH.

- [ ] Step 12.3: Integrate relay lifecycle into unified runtime.
  - Scope: `app`, `remote_core::discovery`, GUI device model.
  - Work: advertise relay candidates, auto-start local tunnels, and select routes in order: LAN/EasyTier direct, UDP relay, TCP relay.
  - Verification: two-machine GUI connection succeeds without manual tunnel commands.
  - Progress, 2026-05-27: added relay-aware discovery routes. `DiscoveryScope::Relay` and `DiscoveryRouteOverride` let a discovery packet received through a local relay tunnel produce a connect endpoint that points at the local relay control tunnel, while preserving the remote device identity and display name.
  - Progress, 2026-05-27: the unified runtime can opt into TCP relay with `REMOTE_PLAY_RELAY=1` and `REMOTE_PLAY_RELAY_SERVER_ADDR=<host:port>`. Startup now automatically binds a relay control tunnel and a relay discovery tunnel, wires the discovery tunnel as an announce target, and keeps both tunnels alive with reconnect behavior.
  - Progress, 2026-05-27: the app device model now keeps direct and relay candidates for the same device and selects direct LAN/EasyTier routes before relay routes. If relay is the only candidate, the Connect action targets the local relay control tunnel.
  - Progress, 2026-05-27: the unified GUI device row now shows simple user-facing states (`Connectable`, `Online`, `Offline`, `Connected`) instead of exposing LAN/Mesh/Relay jargon in the main list.
  - Verification, 2026-05-27: `cargo test -p remote_core discovery::tests::`, `cargo test -p remote_core relay::tests::tcp_relay_tunnels_forward_existing_udp_protocol_both_ways`, and targeted `remote_play_app` route/relay wiring tests pass with `PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"` so `cmake` is visible to `audiopus_sys`.
  - Progress, 2026-08-12: hardened TCP relay identity binding, group/connection limits, bounded slow-peer queues, exact frame bounds, idle and WebSocket handshake timeouts, and secret-derived control/discovery capabilities. Added binary WebSocket relay transport so the fallback can share the NAS Caddy `8443` listener without replacing FreshLoop.
  - Verification, 2026-08-12: 9 focused `remote_core::relay` tests pass, including real bidirectional UDP protocol forwarding through TCP and WebSocket tunnels. Unified app tests pass for both raw TCP and WebSocket relay ownership/wiring. A static x86_64-musl relay binary was cross-compiled for DSM.
