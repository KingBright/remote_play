# Runtime Smoke Checks

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
