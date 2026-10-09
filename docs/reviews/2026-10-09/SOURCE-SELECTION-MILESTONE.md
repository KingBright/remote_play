# Enumerated-source selection milestone, 2026-10-09

Source fix **80de0721cbd256b5673335569b0b43e0d9a9e7b8** makes the existing bounded original-GPUI entry able to select Linux's real MainDisplay/Desktop catalog item. The ordinary menus and bounded entry use the same bound source controller and production stream settings. This is a tested implementation and signed candidate checkpoint; the candidate has not run on Studio, been installed, or received real screen acceptance.

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

Commands and log hashes are in [source-selection-milestone.json](evidence/source-selection-milestone.json). Ignored physical/live tests remain unexecuted. Already-passed release suites were not repeated; this fix changes no Python implementation.

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

## Actual validation boundary

The prepared minimal path keeps HO5's producer unchanged and uses the newly signed production bundle on Studio as a temporary staged process. Before starting it, run the repository installer in its default read-only plan mode to compare old/new identities. Stop only the existing RemotePlay user launch agent, require the old owner gone, run one staged candidate with the existing protected profile and the exact HO5 device/desktop selector, record first decoded/scene timestamps and a subsequent 30-second interval, then restore the unchanged canonical launch agent. A second bounded run can record a new connection after shutdown. No installation replacement, new network identity, OS permission grant, synthetic frame or extra runtime is part of this plan. A staged process must be reported separately from the installed app/version. Any capture/consent boundary must stop that dependent test rather than alter security.

**This path was blocked before upload.** Automatic approval review rejected Remote Hosts' transfer of the proprietary compiled package to Studio, saying the destination/disclosure lacked explicit authorization in trusted user messages. No transfer operation or candidate launch was accepted, and no workaround was used. A specific approval question for the package plus public verifier scripts was presented; the response is pending. A subsequent workspace snapshot reported no active terminal or transfer and no retained transfer for this workspace. Installed apps and services have not been changed during this fix phase.

There is also a capture implementation boundary to investigate if transmission is approved. The inspected HO5 prior-release source enumerates MainDisplay/Desktop. Its Linux encoder command uses `x11grab` when DISPLAY is present and otherwise `kmsgrab /dev/dri/card0`; it contains no Wayland portal path in that inspected branch. A separate backend-name function returns `pipewire/wayland` when WAYLAND_DISPLAY exists, which does not prove PipeWire capture. Source paths, hashes and read operation IDs are recorded in the JSON. This inspection is not independent installed-binary compiler provenance or a fresh live capture result. No new HO5 build or capture-default change was made to hide this boundary.

Real screen first-frame, 30-second sustained capture, visible presentation, disconnect recovery, input, audio, visual acceptance and all twelve platform directions remain **not tested for this candidate**. The earlier owned-window baseline in `4f3516e` remains a separate result from installed older apps and synthetic content.
