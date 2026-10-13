# Relay listener recovery and full-release gate status

## Delivered source repair

The routed relay kept UDP listeners in its peer/session maps after their receive tasks had exited. peer_endpoint returned the cached local address without checking that a reader existed. This could leave a seemingly valid endpoint where new datagrams were no longer consumed. Both outgoing peer-reader and inbound host-reply-reader versions were reproduced in deterministic unit tests against the unchanged baseline: both timed out. The same tests pass after the repair.

The fix preserves each live socket, local port, device ID and route token while rearming an unexpectedly completed reader. Recoverable delayed UDP errors are classified and yield with a bounded backoff instead of closing the task. Permission and configuration failures remain explicit errors, not indefinitely hidden retries. The healthy media path checks only task liveness and does not acquire the new error-state mutex per packet.

The peer cache now reclaims only five-minute-idle endpoints with no live outgoing connection; active routes and recently requested discovery endpoints retain their addresses. Connection limits and packet limits are not raised. Stale queued events from retired listeners are rejected by matching the listener endpoint. The envelope serialization/HMAC verification and the route API were compared to baseline and are unchanged. No GUI input, capture, encoding-quality or signed-deployment behavior was altered.

Nine new regression tests include both original dead-reader cases, real synthetic local-host socket close/rebind, recoverable and fatal error handling, capacity reclamation, live route retention and periodic repair. They extend the existing eight relay authentication/isolation tests. A route-reader failure is an established defect; this report does NOT identify it as the unique proven cause of the installed Mac Studio authentication timeout.

## Final same-source regression

Canonical router SHA-256: a05f8e684e121f394e7195ae6ced06f361c1453272738ad0b96a917423180969.

| Platform | remote_core library | client library | restored App regular tests | restored all-target check |
|---|---:|---:|---:|---|
| macOS | 245 passed | 28 passed | 107 passed, 3 ignored | passed |
| Linux / HO5 | 245 passed | 27 passed | 92 passed, 1 ignored | passed |
| Windows cube | 243 passed | 27 passed | 92 passed, 1 ignored | passed |

Ignored live/explicit GUI tests were not executed in this turn and are not counted as passed. macOS remote_core strict Clippy passed. Cross-platform duplicates are not summed into independent feature coverage. All final test/check commands exited.

The first Linux wrapper used rg after the core tests, but rg was unavailable. A later App link omitted the pre-existing private xkbcommon linker directory and failed. The final run uses the already existing build-only LIBRARY_PATH and passes; no OS package, driver or security setting was changed.

## Actual device diagnostics and their limit

A two-second passive stack sample of the existing Mac Studio alpha.7 process succeeded without modifying/restarting it. Its sampled Tokio workers were mostly waiting; a short idle sample cannot establish the earlier timeout cause. A bounded explicit loopback authentication/heartbeat check using the existing host profile was rejected by the platform safety checker and did not execute. It was not retried through another host/tool/wrapper. The unexecuted example was removed from the product build targets and retained only in the outside-repository evidence directory.

All routing regression tests used their own synthetic keys, sockets and local test server, not the installed application or user profile. There is no newly accepted real Studio↔MacBook↔HO5 continuous/sequence relay result in this turn.

## Hardware decode feasibility

A synthetic 60-frame 1280×720 HEVC test pattern was decoded with the existing permitted GPU runtime on HO5 (VAAPI) and cube (D3D11VA). Both decoded 60 frames, reported zero decode errors, ended normally with exit 0 and retained GPU frame formats in the output graph. They did not copy the result to an image GUI or present it on screen; neither result is a native-GPUI integration or performance benchmark. The first Windows process-wrapper receipt did not capture the child exit code; a second bounded run explicitly captured it as 0 and is the final receipt. UU stayed Running/PID27892; RemotePlay stayed stopped on Windows.

## Publication decision and new scoped proposal

No full release was published. The currently locked GPUI dependency lacks a usable Windows/Linux video-surface path: only macOS has a native SurfaceSource; Windows draw_surfaces is effectively empty; the Linux original native-frame surface branch is Mac-specific. The restored non-Mac application still visibly marks native video as pending. Framework control-window support does not fill that gap.

The existing user-approved original-GUI restoration plan remains active. A further explicit third-party-renderer maintenance scope is proposed in docs/plans/GPUI-NATIVE-VIDEO-PATCH-SCOPE.md: pin the audited original GPUI source, add the minimal native video resource/import/composition support, preserve all shared Rust logic and the original appearance, and retain the Mac native path. No dependency source/renderer was modified, no Flutter switch was made, and no approved feature was removed. This additional third-party patch must be explicitly accepted before adopting it as the maintained product framework.

The full functional catalog, real GUI input/media/files/audio/PiP/sequence behavior, presentation-level performance comparisons and long-running resource tests still gate default switching and distribution. A source file existing or a unit-test count is not a feature-completion receipt.

## Current installed status and evidence

Installed alpha.7 programs were not replaced or restarted in this turn. HO5 remains active with PID1899036/NRestarts0; Mac Studio original PID78134 remained running; Windows RemotePlay is stopped and UU remains Running/PID27892. No screen capture, remote input, identity/permission/autostart change, or NAS download-index publication was performed.

Main working-tree backup and receipts: /Users/jinliang/Workspace/.rp-complete-20261002. Full baseline has 297 files and archive SHA-256 51fc71de7715ebf748c3e5b939a776cec2230fed0e3cd2e6d826618adddf0823. Only existing product file changed from this baseline is remote_core/src/routed_relay.rs. No source commit or push occurred.
Mac synthetic test logs: /Users/jinliang/workspace/.rp-router-unit-20261002.
Linux synthetic test logs: /var/home/liang/workspace/.rp-router-unit-20261002.
Windows synthetic test logs: C:/Users/Liang/Workspace/.rp-router-unit-20261002.
