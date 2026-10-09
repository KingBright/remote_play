# Full-product loopback checkpoint (2026-10-09)

## Scope

The fixture uses the production RestoredDashboard, original GPUI/Ely renderer,
WorkspaceConnection, discovery, Session-v2 commands and authenticated UDP transport.
It creates a fresh in-memory paired identity and an isolated `/tmp` profile/preferences
path. Peer announcements and peer listeners use loopback. It neither reads an installed
profile nor captures a system screen, injects OS input, changes permission/security
settings, replaces an installed app, or changes production launch defaults.

`UnifiedRuntimeHandle::from_workspace_owner` attaches the existing service owner; it
creates no second network runtime. `run_restored_workspace_runtime` uses the same
renderer-aware profile lock and shared production dashboard bootstrap. The optional
progress output observes state only; it cannot connect, select a source or unlock input.
The peer offers explicitly generated blue/green HEVC sources, a silent handshake peer,
and delayed/missing source acknowledgements for later UI-driven acceptance.

## Verified checkpoint

The isolated fixture compiled successfully and the compiler exited with code 0 in the
existing `/Users/jinliang/rust-target`, offline with the existing lockfile. The receipt
`/tmp/remoteplay-product-loopback-seeded-build.json` records source hashes stable during
compilation. No build was restarted after the native-window tool timed out.

The sole native launch request returned `Computer Use server error -10005:
timeoutReached`. A read-only process check subsequently found no fixture or ffmpeg
process. The owned fixture directory `/tmp/remoteplay-product-loopback-54824` contains
isolated preferences, an empty encoder-error log and a 352-byte blue HEVC sample;
there is no peer/provenance/dashboard progress or final receipt.

Read-only ffprobe of that existing sample exits 0 and reports HEVC 640x360, limited
range and BT.709 matrix, but no explicit `color_primaries` or `color_transfer`. Those
values are mandatory in the fixture's fail-closed SPS check. The artifacts are
consistent with that check rejecting the sample before peer/window startup. Native
launch did not preserve the program's stderr, so the precise returned Rust error is
not independently recorded. The missing metadata must be corrected and verified in
the generated fixture bitstream rather than guessed or relaxed in the viewer.

`/tmp/remoteplay-product-loopback-checkpoint.json` retains these observations. All
105 unrelated dirty paths retain their recorded hashes. The staging area was empty
and no index lock existed before this scoped checkpoint.

## Acceptance still pending

No full-product window screenshot, authenticated connection, decoded first frame or
visible first frame was observed in this run. Source switch/retry/timeout and input/
audio consistency were not exercised. This is a compiling integration checkpoint,
not an end-to-end or physical product acceptance result. Generated HEVC content can
never establish real system-screen capture acceptance, audible audio playback,
remote OS input injection, scanout timing or long-duration performance.
