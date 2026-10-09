# Linux X11 native-build boundary, 2026-10-09

The X11 repair remains implemented in **b329284**. This continuation prepared its native Linux verification on the existing HO5 workspace, but the source transfer was blocked before any bytes arrived. **No new Linux compiler result or real capture/presentation pass is claimed.** The previous seven local macOS/real-FFmpeg regressions remain their original result and were not repeated.

## Exact scope and preservation

The latest HO5 release source is `/var/home/liang/workspace/.rp-original-release-20261004/incoming`. Its October 4 host/core baseline lacks later modules, so copying only the five repair files would not form a source-consistent current host build. An 88-file host/dependency and Cargo resolver comparison found **17 changed files, 408,802 bytes; 71 exact matches**. The difference includes the later host capture label, shared state modules and updated workspace manifests. App/client/Ely manifests are resolver metadata; this does not validate their complete source or GUI implementation.

The proposed difference stays in that existing source directory. The existing build target is `/var/home/liang/workspace/remote_play/target`. HO5 had 759,794,528,256 free bytes at preflight, well above the 8 GiB start / 4 GiB remaining gates. No build process was active. No repository copy, new target, application build, dependency download or system package installation occurred.

Eleven overwritten files were first hash-verified and retained in `.rp-source-picture-20261009/x11-source-rollback`; six absent paths are explicitly listed. The public-source rollback manifest SHA-256 is `109d094bcde2fe8e67c6f4265c75e8b116c45eaf6f5ded479eb730e295c2ef4c`. No private configuration or credential bytes were read or backed up.

## Transfer blocker and exact recovery handles

A standard RHSYNC1 source-only bundle was created without copying the repository:

- Local path: `/tmp/rp-x11-linux-host-delta-20261009.rhsync`.
- Bundle size: **413,122 bytes**; SHA-256 `06ef220f13dac5368201cd4756e201fe31764687745a900dc617404bd98e2777`.
- Upload destination: `.rp-source-picture-20261009/x11-host-delta.rhsync` on the same HO5 workspace.
- Original paused upload operation: `37c1dfde-5dbc-420e-a19d-aeb49e8702d8`.
- Version-bound sync manifest: `2bc530d887312cbb992a5d8e4d34a9bcd24c305a0b7f29595d294d29e1add399`.

The device reports `source_address_policy_rejected` during `source_client_initialization`, with **confirmed_bytes=0** and **http_request_started=false**. Source authorization remained available. A separate read-only DNS observation found `sdmntprcentralus.oaiusercontent.com` resolving to `198.18.0.25` and `fc00::f`; both are non-public. The transfer implementation rejects non-public DNS addresses before constructing the HTTP client.

This is a source-address policy boundary, not an automatic approval review rejection, expired file authorization, GUI failure or OS capture permission diagnosis. DNS/proxy/security configuration was left untouched. No other upload target, device, source URL, code-edit fallback, raw SSH or shell file-body transfer was attempted. Recover only this original operation after the original source connection/policy boundary is legitimately resolved; do not start a replacement upload. The destination did not exist at final verification. The sync apply and source mtime refresh have not run.

## Controlled geometry and pending test

The existing HO5 environment was DISPLAY=:0, WAYLAND_DISPLAY=wayland-0, XDG_SESSION_TYPE=wayland. Read-only `xwininfo -root -stats` observed root **3840×2160 at 0,0**. That establishes X11/Xwayland root geometry only; no root pixels, physical main-monitor selection, complete Wayland content or visible GPUI image was obtained. No owned-window experiment was repeated.

A new explicit Linux-only ignored test, `ffmpeg_hevc::tests::linux_x11_command_reads_root_metadata_without_capturing`, is prepared in `host/src/ffmpeg_hevc.rs`. It requires an independently observed `RP_X11_EXPECTED_ROOT=WIDTHxHEIGHT` and DISPLAY, checks the production metadata reader, and invokes the actual Linux `capture_command` constructor. It validates whole-source input size/origin, planned output dimensions, libx265, yuv420p and the actual x11grab label. It **never spawns that command**. This new native test has **not compiled or run** because transfer was blocked; its existence is not acceptance evidence.

After a completed hash-verified difference, with no concurrent build, use the already-present `scripts/refresh_transferred_sources.py` on only the 17 changed files before compiling. Reuse the existing target and locked offline dependencies. The pending checks are:

```text
CARGO_TARGET_DIR=/var/home/liang/workspace/remote_play/target cargo check -p host --lib --offline --locked -j 2
CARGO_TARGET_DIR=/var/home/liang/workspace/remote_play/target cargo test -p host --lib linux_capture_geometry::tests --offline --locked -j 2 -- --include-ignored --nocapture
RP_X11_EXPECTED_ROOT=<fresh independent root size> CARGO_TARGET_DIR=/var/home/liang/workspace/remote_play/target cargo test -p host --lib ffmpeg_hevc::tests::linux_x11_command_reads_root_metadata_without_capturing --offline --locked -j 2 -- --ignored --exact --nocapture
```

Only the synthetic raw-RGB quadrant fixture may launch FFmpeg in those checks. It does not use a capture device. A completed cached build alone is insufficient: retain compiler consumption/provenance for the changed Linux source and verify intended command behavior.

## Final device and repository state

All seventeen remote source paths still match their pre-transfer versions, including six still-absent new paths. HO5 remains active/running at PID **2232948**, NRestarts **0**, installed executable SHA-256 `4cd160cef5da7644486e7f46a695ad9753e8994820986c00723334a786adac2c`, with no direct FFmpeg child and no build process. No production app, launch service, profile, membership, native viewer or OS permission was changed. Existing signed candidate/rollback packages and published artifacts were untouched. All 105 pre-existing local dirty paths preserve their exact status and SHA-256.

The [Wayland adapter proposal](../../plans/LINUX-WAYLAND-CAPTURE-ADAPTER.md) remains unimplemented. It can activate existing frame traits through narrow Linux encoder/lease seams, but source interaction, cancellation, compressed-frame backpressure and lease ownership together touch substantial platform wiring. It requires a concrete scope decision before implementation; the X11 small-fix authorization is not native Wayland approval.

Complete remote receipts, source versions, DNS observations and pending acceptance boundaries are saved in [x11-linux-native-build-boundary.json](evidence/x11-linux-native-build-boundary.json).
