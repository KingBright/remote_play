# Runtime Smoke Checks

## Current Four-Platform Acceptance Entry

Changes to capture/publish/pull, codecs, source selection or end-to-end behavior
must use [the directed acceptance matrix](testing/FOUR-PLATFORM-MATRIX.json).
It has twelve cross-platform directions (capture/publisher → viewer), four
same-platform rows and a separate synthetic loopback row. Current device versions,
access blockers and unsupported source capabilities are recorded separately from
executed acceptance. A source list, decoded frame and visible frame are distinct
claims; source hashes must be paired with the compiled artifact and actual process.

```bash
python3 scripts/check_platform_matrix.py
python3 scripts/check_platform_matrix.py --require-cross-platform
```

The first command checks supplied record consistency; the second exits 1 until
all twelve directions have complete acceptance evidence. Neither inspects devices,
independently verifies a screenshot/performance report, nor authorizes deployment.
Unknown, failed and unsupported are separate results. In particular, unsupported
window capture cannot silently pass as full functional preservation.

The compact [current experiment record](reviews/2026-10-09/PRODUCT-LOOPBACK-CHECKPOINT.md#follow-up-vui-fix-and-real-product-observations)
links the raw small receipts, exact source/artifact hashes, environment, method,
observations, limitations and reproduction commands. This is the repository entry
for the current experiment, without copying targets, binaries or all logs.

The [real-device checkpoint](reviews/2026-10-09/REAL-DEVICE-CHECKPOINT.md) records
MacBook/Mac Studio exact installed signature/version observations and HO5 user-service
state. The existing Computer Use card was reported approved; same-app recovery timed
out with no window. No physical direction or real first frame has passed. This record
is separate from the earlier synthetic experiment.

Core regression cases, with minimal sufficient real sources on each available
platform, are mandatory:

- Enumerate applications/windows and bind the exact source ID, owning process,
  connection and source revision; reject disappeared/ambiguous targets.
- Capture a selected application's actual window; move, resize and close it.
  Permission failure or missing window must not redirect capture/input to a desktop
  or another same-titled window.
- Publish and pull at least two distinct windows concurrently where supported;
  verify different target identities/content, input isolation, active audio ownership,
  cancellation and complete release of subscriptions/native resources.
- Switch sources quickly with reordered/late replies, interrupt the connection,
  repeat connect/retry actions, and exercise timeout/recovery. No stale response may
  resurrect a dismissed view or deliver input to another source.
- Verify first visible frame, advancing/continuous content, audible audio/mute,
  actual intended OS input, and resume/disconnect behavior independently. Collect
  sufficient negative evidence for locked, paused, stale-revision and closed sources.

Use existing `app/examples/window_scoped_acceptance.rs` with
`scripts/acceptance/WindowControlFixture.swift` for controlled real macOS windows;
the current shared `scripts/tests/run_gui_slice_regressions.py` and production
owner/component tests cover state/dispatch invariants only. Existing real-capture
fixtures require an explicit owned source/profile and existing OS consent; their
presence or earlier historical result does not establish current-candidate coverage.
The synthetic `scripts/run_product_loopback.py` below does not replace these cases.

For controlled loopback preflight/window runs, build only the explicit example in
the already configured target, then use `scripts/run_product_loopback.py preflight`
and `window` with `--binary`; see the experiment record for the exact commands.
Every launched fixture has a hard owned-process deadline and saved output/exit.
No automatic permission approval, TCC changes, replacement identity, capture-default
change, unreviewed installation or simultaneous duplicate runtime is permitted.

## Headless Data-Plane Media And File Smoke

Use this check when changing the unified data plane, scheduler, media adapter, or file-transfer runtime:

```bash
scripts/smoke_dataplane_media_file.sh
```

The script starts the unified `remote_play` binary in headless passive-host mode with:

- `REMOTE_PLAY_DATA_PLANE_MEDIA=1`
- `REMOTE_PLAY_FILE_TRANSFER=1`
- `REMOTE_PLAY_HOST_SEND_FILE=<temp file>`

It then runs `remote_core/examples/headless_smoke_client.rs`, which sends `StartStream` and heartbeats over the real control protocol, receives data-plane media and file-transfer packets, routes file packets into the normal file-transfer runtime, and verifies:

- video arrives through the data-plane media path;
- audio can optionally be required with `REMOTE_PLAY_EXPECT_AUDIO=1`;
- when data-plane audio is required, an `AudioStreamConfig` packet is required before the run is accepted;
- optional macOS system audio can be exercised by running the script with `REMOTE_PLAY_SYSTEM_AUDIO=1`;
- no legacy RTP video is required for this mode;
- the host-sent file is materialized and checksum-verified by the receiver runtime;
- the host can stop cleanly after `StopStream`.

The smoke requires macOS capture permissions for the terminal or app context running `cargo run -p remote_play_app --bin remote_play`. If screen capture is blocked, the client will fail with no observed video packets, which is a valid environment failure rather than a transport pass.

Viewer microphone talkback is intentionally not part of the default headless smoke because it needs a real input device on the viewer side, a real output device on the controlled side, and OS audio permissions. For a manual local or two-device check, start both sides with `REMOTE_PLAY_TALKBACK=1`; the client will send `ViewerMicrophoneTalkback` as `session_id + 100`, and the host will play only that active-session client-to-host stream. The client control panel exposes Off, Always, PTT, local mic mute, remote playback mute, and remote playback volume. Keep this off during normal smoke runs to avoid accidental feedback.

## Unified GUI Runtime Notes

The old standalone `client` and `host` binaries have been retired. Use the unified app for GUI runs:

```bash
cargo run -p remote_play_app --bin remote_play
```

For automation or passive-host-only smoke runs, use `REMOTE_PLAY_HEADLESS=1` and disable unneeded services with environment flags.

Recent local run, 2026-05-17:

- `REMOTE_PLAY_EXPECT_AUDIO=1 scripts/smoke_dataplane_media_file.sh`
- `data_video=201`
- `data_audio=362`
- `data_audio_configs=1`
- `remote_mic_configs=1`
- `remote_system_configs=0`
- `legacy_video=0`
- `legacy_audio=0`
- file transfer completed for `smoke-source.bin`
- host scheduled sender reported realtime and reliable sends with zero send errors

Recent local run with experimental macOS system audio, 2026-05-17:

- `REMOTE_PLAY_SYSTEM_AUDIO=1 REMOTE_PLAY_EXPECT_AUDIO=1 scripts/smoke_dataplane_media_file.sh`
- `data_video=198`
- `data_audio=716`
- `data_audio_configs=2`
- `remote_mic_configs=1`
- `remote_system_configs=1`
- `legacy_video=0`
- `legacy_audio=0`
- file transfer completed for `smoke-source.bin`

## Two-Mac Public WebSocket Relay Smoke

The 2026-08-12 packaged-runtime check used the deployed relay endpoint:

```text
wss://relay.hackerlife.fun:8443/v1/relay
```

The local Mac ran the packaged unified runtime in headless passive-host mode with data-plane media and file transfer enabled. Mac Studio ran a second packaged unified runtime as the local relay tunnel plus the release `headless_smoke_client`. The real protocol path completed with:

- `data_video=284`
- `data_audio=558`
- `data_audio_configs=1`
- `remote_mic_configs=1`
- `telemetry=11`
- `legacy_video=0`
- `legacy_audio=0`
- a 65,536-byte file with matching source and receiver SHA-256

Swapping the roles proved that `StartStream` also reaches Mac Studio through the same public relay. Media capture on that direction is currently blocked by macOS Screen Recording TCC for the newly packaged app. Grant Screen Recording permission on Mac Studio, restart RemotePlay, and repeat the reverse smoke before treating two-direction media and GUI video as accepted.
