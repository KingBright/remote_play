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

## Follow-up VUI fix and real product observations

Hypothesis: writing complete VUI into the known synthetic BT.709 input fixes the
startup blocker while retaining the product's strict metadata checks. The fixture
now explicitly applies `hevc_metadata=colour_primaries=1:transfer_characteristics=1:
matrix_coefficients=1:video_full_range_flag=0` after VideoToolbox encoding. This is
fixture-only generated content; no capture metadata is inferred or viewer check relaxed.

All necessary small raw evidence is in [evidence/loopback-vui.json](evidence/loopback-vui.json),
19,707 bytes at collection. It contains environment, base commit d0d9271, compiled
source hashes, binary SHA-256, full-log checksums and short excerpts, preflight/exit
receipts, source/decoder/frame states before and after switching/disconnect, and limitations.
The compiled artifact SHA-256 is
`38c335539870c6c0f1f1833d0888c91d957f95d14aa7804042835a72e495ef9e`.
The source hashes distinguish the actual dirty source build from base commit d0d9271;
do not relabel a historical build as the future final commit. The runner changed
after preflight to retain a bundle launch path; both recorded versions are preserved.

Environment: local arm64 macOS 26.5, FFmpeg 8.0.1, existing rust-target, production
original GPUI/Ely and VideoToolbox decoder. The scoped build exited 0 in 16.099 s.
Preflight exited 0 in 1.683 s without GUI/network/profile use: production SPS parser
and independent ffprobe read explicit BT.709 primaries/transfer/matrix and limited
range for both sources. Independent FFmpeg decoded exactly one 640×360 YUV420 frame
per sample (345,600 bytes). Blue/green frame MD5 values were respectively
`087327ad018b94222f6030a06f198324` and `104dec42e30dca5d86a5915d728bd02b`.
This independent preflight decode is separate from product VideoToolbox decode.

The first post-fix launch used an unbundled CLI path. Product progress showed the
dashboard rendered, but CUA could not bind the app, so only its recorded PID/group
62183 was stopped (exit -15 at 114.39 s). Retaining the explicit temporary bundle
path corrected NSBundle identity without another compilation. The successful run
used PID 64144; its watchdog was 210 s, product self-exit 180 s, actual exit 0 at
181.36 s. UI actions used real production controls, not a command-file/debug UI:

1. Connect Stream on the explicit loopback device. The peer dropped its first Hello
   and Open response. Receipts record hello_count=2/open_count=2, then encrypted
   media, confirmed subscription and product decode.
2. The actual window screenshot showed a blue canvas. At the first saved snapshot,
   decoded_frames=810, decode_errors=0, source=MainDisplay and scene submission=true.
   This screenshot establishes visible content separately from scene submission;
   scanout completion/time was not instrumented.
3. Clicking near the top revealed the auto-hidden control island. Apps displayed
   explicit synthetic blue/green/timeout catalogue entries. Selecting green showed
   an actual green canvas; the peer and product agreed on Window(202), revision 1001.
   The saved green snapshot records 2,682 decoded frames, zero errors, input locked,
   no input events, audio requested but no audio-owner/packets. Neither real OS input
   nor audible audio playback was accepted.
4. Formal Disconnect returned the Ready device drawer. At elapsed 178,648 ms,
   current_frame=null, sessions=[], pending=0 and input remained locked. The process
   then exited on its own bounded timer. Reconnect, rapid switching, timeout,
   multi-window concurrency and physical capture were not executed in this run.

CUA screenshots remain in this task's tool trace; no image file was exported and
there is no screenshot-file checksum. The passive JSON observations are retained,
but are not a substitute for screenshots or an independent verifier.

Reproduction (reuse the configured target; no install/sign/publish):

```bash
cargo build --offline --locked --target-dir /Users/jinliang/rust-target -p remote_play_app --example product_loopback_e2e
python3 scripts/run_product_loopback.py preflight --binary /Users/jinliang/rust-target/debug/examples/product_loopback_e2e
python3 - <<'PY'
from pathlib import Path
import plistlib
contents = Path('/tmp/RemotePlay Product Loopback.app/Contents')
(contents / 'MacOS').mkdir(parents=True, exist_ok=True)
binary = Path('/Users/jinliang/rust-target/debug/examples/product_loopback_e2e')
link = contents / 'MacOS/product_loopback_e2e'
if not link.exists():
    link.symlink_to(binary)
assert link.resolve() == binary
(contents / 'Info.plist').write_bytes(plistlib.dumps({
    'CFBundleExecutable': 'product_loopback_e2e',
    'CFBundleIdentifier': 'com.remoteplay.fixture.productloopback',
    'CFBundleName': 'RemotePlay Product Loopback',
    'CFBundlePackageType': 'APPL', 'CFBundleVersion': '1',
    'NSHighResolutionCapable': True,
}))
PY
python3 scripts/run_product_loopback.py window --binary '/tmp/RemotePlay Product Loopback.app/Contents/MacOS/product_loopback_e2e'
```

The temporary bundle has identifier `com.remoteplay.fixture.productloopback` and
executable symlink to the same verified example; Info.plist includes its executable,
CFBundleName/PackageType and NSHighResolutionCapable. Do not use an installed product
bundle or create a replacement signing identity. The runner checks the 4 GiB reserve
and maintenance state, saves running/exit/output/hash receipts and terminates only
its own new process group if the deadline/floor is reached. Child encoders/probes
also have 12-second bounds. CUA binding/inventory calls used a 5-second Promise race;
the process watchdog is independent of the UI service.

The [directed matrix](../../testing/FOUR-PLATFORM-MATRIX.json) is the canonical coverage
record; [Runtime Smoke Checks](../../RUNTIME_SMOKE.md#current-four-platform-acceptance-entry)
is the clear repository entry. The matrix includes actual RemoteHosts device/runtime
observations and the unsuccessful read/ADB attempts, without reading keys or changing
permissions. Local synthetic success is not four-platform or application-capture acceptance.
The fixed matrix checker exits 0 for the complete record structure and 1 with
`--require-cross-platform`: all twelve physical directions remain not_tested.
