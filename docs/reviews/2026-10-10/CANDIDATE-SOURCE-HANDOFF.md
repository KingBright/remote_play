# Complete Linux candidate source handoff

Repository: `https://github.com/KingBright/remote_play.git`
Branch: `codex/gpui-mvvm-migration`
Source implementation base: `7b703fa542f9506df717a402445721fce7fbf882`.
Resolve the branch to its full commit before fetching/building; this document
does not claim a remote ref, compiled binary or installed version.

The Git tree contains the original GPUI entry, Ely adapter, MVVM device state,
native video/input paths, Linux portal/PipeWire implementation and pinned vendor
dependencies. `CANDIDATE-SOURCE-MANIFEST.json` freezes the build/source support
files and classifies all 105 pre-existing dirty paths. Its digest covers sorted
path, Git mode, SHA-256 and byte count. The manifest itself and these new receipt
documents are excluded to avoid circular digests. Dirty Android JNI is preserved
locally; its committed base is listed because Android is outside this Linux
candidate. Optional historical probes/examples are not required product source.

This checkpoint adds existing policy/version documents, source refresh helper,
strict macOS packaging protections and their validation. The old friendly-name
or ad-hoc fallback packager must not accompany the new candidate. No certificate
rotation, private-key transfer, NAS publication or installed app replacement is
part of this commit. The candidate workflow tree matches remote main observed
at `408491bc9fbc276ecf8cfb4e251e269fce22c3c7`; the commit requests `[skip ci]`.
No new Actions are configured or explicitly dispatched.

## HO5 observations and next execution

The existing HO5 checkout is `/var/home/liang/workspace/remote_play`, with existing
`target` cache. It was clean on `main@969ca10922eb17e705877b7ca4cf5ce3bec836c1`
at the 2026-10-10 07:53 UTC check; a second check confirmed this and the expected
GitHub origin. Preserve concurrent work and fail if this state changes.

At 07:57:24 UTC, boot ID `2788a001-2ddf-453b-b616-b58198a9ae3f` and booted
deployment `44.20261006.1` proved the prior maintenance reboot had completed.
The package transaction was empty. PipeWire/SPA, ALSA, clang/libclang, cmake,
xkbcommon-x11, fontconfig and freetype SDK checks passed. Do not repeat package
installation, DNS changes or reboot. FFmpeg ABI 62/60 libraries were observed;
system development headers were absent. Existing verified SDK caches need a
separate check before the native build.

Git delivery was explicitly authorized by the human on 2026-10-10 at 07:50 UTC.
Push this scoped candidate, verify the remote SHA, then fetch into the existing
clean HO5 repository without reset/force/overwrite. Verify manifest hashes and
refresh only changed, hash-verified source mtimes before any build. First run
`cargo check -p host --lib --locked --offline`, then the app's ordinary Linux
check, reusing the existing target. Ordinary checks are not native presentation
or deployment acceptance. The native product also requires
`--features native-linux-video` and verified FFmpeg 8.x headers.

The old paused 17-file HTTP delta operation
`37c1dfde-5dbc-420e-a19d-aeb49e8702d8` is obsolete and has no confirmed source
bytes. It is not resumed or replaced through a new MCP file transfer. Authorized
ordinary Git delivery does not reuse or bypass that source authorization.

Local tests and the durable HO5 environment observation are in
`evidence/candidate-source-handoff.json`. The pending physical test contract is
`../../testing/MAC-HO5-ACCEPTANCE-CONTRACT.md`. No physical acceptance or installed
deployment is asserted by this handoff.

## UI identity incident: deployment hold

The owner's 2026-10-10 screenshot showed alpha.7's white/grey simplified device
list and `Your devices. One workspace.`. A separate read-only observation bound
PID 93750, UID 501 to `/Users/jinliang/Applications/RemotePlay.app/Contents/MacOS/remote_play`
and bundle version 2.0.0/build 20260930.7. The local ownership task reports only
single-file chown, not app installation or launch. The actor, launch time and
exact startup command are not established. This Git/HO5 task has not installed
or launched RemotePlay on MacBook. The screenshot alone does not establish that
the current source broke the interface.

Current source enters `app/src/main.rs:27` → `restored_ui::run_restored_gui`.
`restored_ui.rs` renders a full canvas, floating control island, drawer trigger,
management drawer, session switcher and overlays; shared `original_design.rs`
provides the dark idle stage and status capsules. `app/src/ui.rs` is the legacy
design reference, not the current compiled product entry. GPUI probes are not
product replacements or full restoration acceptance.

Historical alpha.8 release manifest source_id
`67bf20c6295b898e95c803c7898ec7f5e547bb162bae6d3d7c90fe42dfd37c33` recorded
`restored-original-gpui`, with restored_ui SHA-256
`09407754a7b89a5d63625b710d8ed6b7510fab3b6ce865c39c857ad0da047997`.
The current candidate's restored_ui hash is
`4321f25d06d3ae316676453f56961003a77c24ba7f4c8dc79eec0efb7e409979`.
Ely/MVVM changes mean the old alpha.8 record does not prove current visual
equivalence. Its post-install screenshot/live acceptance was explicitly absent.
Do not deploy any UI replacement until actual rendering is compared with the
user-approved baseline. Version/build, renderer labels and source structure
alone are insufficient. Retain rollback artifacts and label development tests
separately from release presentation.
