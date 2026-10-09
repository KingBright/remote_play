# Enumerated-source selection milestone, 2026-10-09

Source fix **80de0721cbd256b5673335569b0b43e0d9a9e7b8** makes the existing bounded original-GPUI entry able to select Linux's real MainDisplay/Desktop catalog item. The ordinary menus and bounded entry use the same bound source controller and production stream settings. The signed candidate has now selected HO5's actual catalog item and completed two bounded decode/GPUI-scene runs on Studio, including a fresh connection after shutdown. HO5's observed capture backend is `x11grab`; complete Wayland desktop content and visible presentation remain unverified. The installed applications remain unchanged.

## Change and preservation

The four-file commit contains `remote_core/src/view_commands.rs`, `app/src/desktop/original_owner.rs`, `app/src/restored_acceptance.rs` and `app/src/restored_ui.rs`. A queued choice now carries the actual `CaptureSourceInfo`. Dispatch requires one matching typed source ID, unchanged source metadata, the same connection allocation and the same network share scope. The existing command ticket also rejects cancelled or replaced work. MainDisplay is an explicit enumerated source; an empty display catalog no longer invents it. Display and Window IDs remain distinct even if their numeric values match.

With the existing explicit bounded output/device settings, the new selector is:

```text
REMOTE_PLAY_RESTORED_TEST_SOURCE={"source":"MainDisplay","title":"Desktop"}
```

It is resolved only against that active peer's actual catalog and then calls `start_capture_source_switch`, which calls `switch_bound_enumerated_source`. No selector starts an implicit stream. Legacy exact Window/title/PID selectors remain supported; mixed JSON/legacy arguments, invalid IDs, unexpected fields and duplicate catalog entries are rejected. The receipt records `enumerated_match` and `command_queued` separately from frame/video observations. A queued command does not establish a successful stream or visible presentation.

No producer, protocol, authentication, input, decoder, native-buffer, signing-policy or system-capture-default change is in the commit. There is no new GUI, decoder or synthetic producer. All 105 pre-existing dirty paths retained their bytes; they are not included in this source commit. No repository copy or new target directory was made.

## Tests and compiler evidence

The display-switcher regression first failed against the old behavior: an empty catalog fabricated a MainDisplay. After the fix:

| Check | Result | Boundary |
| --- | --- | --- |
| `remote_core` view-command tests | 9 passed | Missing display/window, duplicate/typed IDs, source removal/reuse/metadata change, cancellation/disconnect, connection/network/session isolation |
| Original-GPUI app library with native video | 110 passed, 2 ignored | Existing app tests plus strict source selector and real-catalog display menu regression |
| Native release build | Exit 0, 87 s | Original GPUI/native-video compiled identity; no physical rendering claim |

Commands and log hashes are in [source-selection-milestone.json](evidence/source-selection-milestone.json). The two ignored Rust physical/live tests remain unexecuted; the separate bounded actual-product runs are recorded below. Already-passed release suites were not repeated; this fix changes no Python implementation.

The build used `/Users/jinliang/rust-target`, `--release --locked --offline -j 2`, and the actual checkout at the source commit. The compiler emitted **597 local file dependencies**, all in the pre-build 878-file source manifest and all unchanged since the manifest. All four modified files were consumed. The unrelated dirty Android JNI source was not consumed. App/core compiler artifact events report `fresh=false`. These checks supplement source hashes rather than treating hashes as proof that an incremental build used them.

Raw executable SHA-256: `5157806cb08557d0386d626e2ac55402825a4d1af0b169f408e5c116205101e9`. `--product-info-json` and the release GUI gate report macOS/aarch64, alpha.8, `restored-original-gpui`, original GUI compiled and native video compiled. The gate explicitly leaves visual and functional acceptance unevaluated. Pre-build binary `6d54558fbf11bbdf8d4828d8ab94fd754929d705e7b9b0b535c7718e678181df` was retained as a local build rollback; this is not an installed-app rollback receipt.

## Signed candidate

The authorized `scripts/package_macos_unified.sh` produced the new unique build **20261009.205212**. Packaging explicitly used the three existing uncommitted overlays recorded in the provenance: `scripts/package_macos_unified.sh`, `scripts/verify_prebuilt_binary.py`, and `VERSION`. This package is therefore source-commit-plus-recorded-packaging-overlays, not a claim of packaging from a wholly clean worktree.

Archive: `/Users/jinliang/rust-target/package/macos/RemotePlay-macos-arm64-2.0.0-alpha.8-20261009.205212.zip` (8,599,856 bytes).

- Archive SHA-256: `f3e1716d6ca30d2411d4c4daa638f9ed359a1bfa47d1463d0a973f03bfbcc729`.
- Signed executable SHA-256: `13345e66c9499ece95a7ea52dca44dff26cbd4cb949115d67f24465bf416193f`.
- Certificate SHA-1: `8eaa97365d97b8a987cdf1232430eddf9aba8ecf`.
- Designated requirement: `identifier "com.remoteplay.unified" and certificate leaf = H"8eaa97365d97b8a987cdf1232430eddf9aba8ecf"`.
- Signature verified; notarization not verified. No ad-hoc fallback, replacement certificate or private-key transfer.

The adjacent `.verification.json` and `.provenance.json` are retained with the archive. The provenance embeds the source manifest, compiler receipt, tests and explicit acceptance boundaries; its SHA-256 is `1f267ae5a24b309a1955bac7644b37821acb88006c26779f2545d43cf8419b47`. Prior versioned packages remain untouched. Nothing was published or pushed.

## Actual bounded runs and their boundary

The original upload rejection recorded in `1a7b8cf` was resolved after the parent provided the user's prior cross-device testing and deployment authorization and explicitly requested one retry of the exact file/workspace/path. That one package retry completed, operation `769075c8-9a50-4a4c-8968-8035e4399d95`, with the expected 8,599,856 bytes and SHA-256. Only the candidate and three public identity/installer files were transferred. One paused public-verifier transfer resumed through its original operation; no uncertain command was replayed. The earlier parameter-only failures started no remote work. No upload rejection was bypassed.

Studio ran `scripts/install_macos_unified.py PACKAGE.zip` without `--apply`. Its plan reported `compatible_upgrade`, identical certificate and designated requirement, no apply blockers and `applied=false`; the external system mesh service was not loaded. The staged candidate and a separate retained copy of the installed app both passed exact identity verification. A preparation comparison initially included archive-only metadata in the app comparison; read-only diagnosis proved the sole differences were archive hash/size. The already-created bundles were retained and fully reverified with the installer identity fields. No bundle was rebuilt, recopied or altered to resolve that diagnostic.

Each run stopped only Studio's existing RemotePlay launch agent, required the old owner and any other `remote_play` process gone, then directly ran the **signed production bundle executable** from `/Users/jinliang/workspace/.rp-source-selection-20261009/candidate/RemotePlay.app`. It used the existing protected profile, explicit HO5 device ID and exact MainDisplay/Desktop selector. The ordinary controller retained the production 1920×1080 stream settings. After the bounded GUI closed its sessions and exited normally, the unchanged canonical launch agent was restored. HO5's producer was neither replaced nor restarted. There was no new identity, permission grant, input injection, synthetic producer, replacement renderer or extra network runtime.

| Observation, HO5 → staged Studio viewer | First run | Fresh connection after shutdown |
| --- | ---: | ---: |
| Bounded GUI duration | 65.003 s | 45.011 s |
| Connection ID | 2612631463 | 1942155545 |
| First sampled decoded-frame observation | 2.700 s | 2.445 s |
| First sampled GPUI-scene observation | 4.001 s | 2.445 s |
| Final decoded-frame count | 3751 | 2568 |
| Decode errors | 0 | 0 |
| Continuous healthy observation span | 60.318 s | 41.985 s |
| Independent ≥30 s decode/scene interval | 30.589 s, 91→1926 frames | 30.699 s, 15→1857 frames |

Both final receipts contain the actual uniquely enumerated MainDisplay/Desktop item (`process_id=null`, source dimensions 0×0 as returned by HO5) and `command_queued=true`. The received image is 1920×1080. Every recorded sample within each healthy interval kept the same MainDisplay/connection, responsive connected peer, confirmed video, nonempty current frame and GPUI scene submission, with increasing decode counts and no video/decode error. The timings come from passive progress sampling of `started.elapsed`; they are observation intervals, not exact network or first-visible-frame latency. For example, first-run decoding was absent at 1.327 s and present at 2.700 s.

The first candidate exited normally. HO5's associated capture process was gone before the next candidate started, and the normal Studio service was restored in between. The second run used a new GUI/network allocation and decoded again. This is **process-level disconnect/reconnect** evidence; same-process reconnect remains untested.

A read-only process observation during the first run identified FFmpeg PID 2260951 as a direct child of the actual HO5 product PID 2232948. Its input was `-f x11grab -video_size 1920x1080 -framerate 60 -i :0`, encoded by `libx265` as `yuv420p` HEVC. This verifies the actual capture-command path rather than relying on the misleading `pipewire/wayland` environment label in the inspected release source. No synthetic frame source was used, but the captured X11/Xwayland content has not been independently compared with the complete visible Wayland desktop. **This is not Wayland portal/desktop acceptance.** No new HO5 build or capture-default change was made to hide that limitation.

The receipt boundary remains `gpui_scene_submission`, with `actual_display_completion_measured=false` and `full_acceptance=false`. macOS's diagnostic snapshot supplies no native surface counters (`native_frame=null`); this is missing telemetry and establishes neither CPU-copy cost nor visible native presentation. No screenshot/scanout, first-visible-frame, input, audio or full visual acceptance was obtained. The twelve-direction matrix remains unchanged; this partial staged-candidate result does not authorize deployment or turn all directions into passes. The older owned-window baseline in `4f3516e` remains separate.

## Final restoration

Studio finished with canonical build `20261004.80`, the original installed signed executable hash unchanged, launch-agent file hash unchanged, and stable normal PID **59676** after the second run. Its staged candidate and rollback copy remain available; the rollback copy has the same verified identity/bytes as the installed app. HO5 finished active/running at original PID **2232948**, NRestarts 0, original installed hash unchanged and no direct FFmpeg capture child. Both renderer-state files report original GPUI.

During each new candidate run, the existing profile's UID, GID, 0600 modes, inode, size and mtime remained exactly unchanged until the candidate exited. Restoring the older installed Studio app caused its ordinary startup to rewrite the existing files; the changed metadata is recorded and normal ownership/modes remain correct. Tools did not inspect/export credential bytes, reset permissions, alter membership or edit private configuration. The 105 unrelated dirty paths remain unchanged. No installer apply, NAS publication, push or additional build was performed.

Raw driver receipts, all passive samples, identity plans, remote file hashes, process observations and derived ≥30-second intervals are embedded in [source-selection-milestone.json](evidence/source-selection-milestone.json).
