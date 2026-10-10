# MacBook old GUI identity incident

Status at 2026-10-10: identity discrepancy established; current candidate visual
acceptance pending; replacement deployment held. This report separates direct
local archive observations from evidence reported by the coordinating task.

## Established identity

The owner's screenshot showed `2.0.0-alpha.7 · macos`, white/grey basic buttons,
a left device list and `Your devices. One workspace.`. The coordinating task
reported PID 93750, UID 501, executable
`/Users/jinliang/Applications/RemotePlay.app/Contents/MacOS/remote_play`, bundle
version 2.0.0/build 20260930.7. Its executable SHA-256
`27f4e4f6f7e9cbbbfbf2125f45293b946a5c76bfab1734410b3cd9c9fb5771d0`
matched the historical 20260930 receipt. That receipt identifies legacy
`desktop::run`/egui and records the visual regression. These are reported
read-only observations, not a new hash or process observation by the HO5 task.

The local historical alpha.8 source archive independently contains the exact
caption at `app/src/desktop/mod.rs:1337`. Its normal main entry selects original
GPUI and calls `restored_ui::run_restored_gui`; `desktop::run` requires explicit
`egui-diagnostic`. The current candidate entirely removes that GUI/dependency
and goes directly from `main.rs:27` to restored_ui. It has a full-canvas viewport,
floating controls, drawer, session switcher and overlays. Source structure
supports this mapping; actual current rendering still requires visual review.

## Candidate and visual baseline mapping

- Baseline: original `app/src/ui.rs`, `design_system.rs` and
  `docs/UI_UX_SPECIFICATION.md`, as required by the user's preservation contract.
  `ui.rs` remains the reference, not today's compiled product entry.
- 2026-10-04 alpha.8: historical source_id
  `67bf20c6295b898e95c803c7898ec7f5e547bb162bae6d3d7c90fe42dfd37c33`,
  restored_ui SHA-256
  `09407754a7b89a5d63625b710d8ed6b7510fab3b6ce865c39c857ad0da047997`.
  Signed build 20261004.80 executable hash
  `bfb517967a3171c9ce1b3e6814e8a0167470b7a7cc4a795a9ab7e2dec2e67f5b`.
  The release report explicitly lacks fresh post-install screenshot acceptance.
- Awaiting MacBook candidate: coordinating task reports build 20261009.181822
  from 41907af plus release overlay and original renderer metadata. This task
  has not verified its executable bytes or actual window; it must not be
  presented as a build from today's Git candidate.
- Current Git implementation: 90fcce0 (parent 7b703fa), restored_ui SHA-256
  `4321f25d06d3ae316676453f56961003a77c24ba7f4c8dc79eec0efb7e409979`.
  Ely/MVVM edits make it different from the historical alpha.8 source. Neither
  a version number nor the renderer name proves equivalence to the baseline.

## Direct cause and unresolved startup attribution

The running default user app remained the old alpha.7 executable; the intended
candidate had not replaced that installation. Historical guarded installation
stopped on root-owned profile files, leaving the old app recoverable. This is
an incomplete delivery, not evidence that this turn modified GUI source into
the simplified interface. The local ownership task reports only single-file
chown, no RemotePlay installation or launch. Reported ownership is now 501:20,
0600/readable; concurrent inode/mtime changes prevent a claim that content was
unchanged. No secret content was read for this report.

The coordinating task subsequently identified the automatic reopening trigger:
`gui/501/com.remoteplay.host`, `~/Library/LaunchAgents/com.remoteplay.host.plist`,
`KeepAlive=true`, `RunAtLoad=true`, `REMOTE_PLAY_HEADLESS=0`, pointing to the same
canonical user app. Its observed PID 16888 had PPID 1, launch time 16:28:28 local,
19349 runs and last exit 0. Thus normal GUI exit was followed by launchd restart.
This identifies that restart mechanism; the first launch actor/command remains
unknown. A canceled stop request is not a successful service change. The parent
is handling permission for a precise GUI-agent stop; this task does not retry
it through another route or disturb Remote Hosts. This HO5/Git task has
created no Mac GUI process, watchdog, launch agent, installer or restart loop.

## Required mechanism correction

Treat source, compilation, signed artifact, installation, running process and
actual rendered window as separate boundaries. Bind expected commit/build,
binary SHA-256, canonical path and GUI entry through them; missing or mismatched
identity must refuse candidate launch/installation rather than select an old
package. Current --product-info-json reports version/renderer/native flags but
not compiled commit/build, and instance state compares only renderer. Thus the
complete requested gate is not implemented yet and existing guards must not be
described as solving the entire chain. Keep signed rollback artifacts; label
development/diagnostic results separately from formal release presentation.
Native screenshots and interactions against the user-approved design are a
separate acceptance gate. The dedicated visual-review task owns Mac GUI work;
this task does not start old or new Mac apps or share/write their profiles.

`scripts/gui_launch_lifecycle.py` provides a read-only policy check with a
regression fixture matching the observed old configuration. Unconditional
KeepAlive on a GUI is rejected; crash/nonzero-exit-only policies can allow normal
exit, while path/network OR predicates cannot hide a restart condition. This
helper is not yet integrated into installed launchers/installers and does not
stop the current service. Independent host/GUI lifecycle remains a separate
implementation requirement: preserve single network ownership and persistent
identity rather than creating two competing runtimes.
