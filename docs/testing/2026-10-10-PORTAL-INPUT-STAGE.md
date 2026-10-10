# Linux Portal and Mac selected-source input stage — 2026-10-10

The Linux source now has a consented RemoteDesktop input path, wired into the
existing original GPUI/Ely application and the common host subscription service.
This is a tested source checkpoint. The normal HO5 current maintenance upgrade
is recorded separately in the linked delivery evidence; physical remote control
has not passed acceptance.

## Behavior and ownership

- **Window / Display** retain the existing view-only ScreenCast selection.
- **Display + control** is a separate explicit local action. It creates a
  RemoteDesktop session, selects keyboard and pointer devices, selects a monitor
  through ScreenCast on that same session, and starts through RemoteDesktop.
  No persistence, restore token, clipboard grant, touch grant, EIS connection or
  global uinput fallback is added.
- Capability is computed from the returned device grants, a live session, the
  selected monitor stream, positive compositor logical size, and the first
  validated owned frame. Unknown or partial grants do not enable control.
- Normalized pointer coordinates use that stream's logical axes. Monitor origin
  and captured pixel dimensions are not substituted for logical coordinates.
  ScreenCast v6 control requires a stable PipeWire serial; native capture prefers
  `target.object` when the serial is supplied.
- Generic Wayland window sharing remains view-only because this adapter has no
  verified window keyboard-focus binding. Unsupported relative pointer/touch
  events, unknown keys and buttons are rejected explicitly.
- A changed format epoch/geometry disables the old input mapping, releases held
  input once, and retains video. Raw rotation/crop without a verified mapping is
  not accepted for control. Compositor-oriented portrait frames are covered by
  fixed tests; physical rotation is still untested.

`PreparedPortalCapture::input_status()` is the Linux adapter's single readiness
result. The common `Subscription::supports_input_now()` uses it for current
capability and replies. `ActiveInput` holds the actual resource as well as the
authenticated subscription. Existing peer/source-revision validation precedes
dispatch; legacy unscoped input cannot reach a Portal source.

Release runs before replacing a subscription, switching sources, unsubscribe,
close and stop. Revocation/worker exit also releases through the original unique
portal owner before closing the session. A bounded release failure closes the
grant and returns an error instead of reporting successful input release.
Recreating a completed stream on the same grant preserves the lease after
teardown. Existing X11 and platform-specific input paths remain present.

The macOS source/GUI patches through
`a770d95419191f99e7c6ebffbea84e066dbf8817` are integrated with the common service.
Each selected display retains its actual ID and reaches
`MacInputInjector::for_display(id)`, including negative origins and portrait
logical coordinates; there is no selected-display fallback to the main screen.
Catalog geometry capability is distinct from the running process's read-only
post-event permission check and the Accessibility check for window control.
Subscription replies require current readiness; denied input reports its actual
reason while retaining video. Constructors and each event recheck permission.
The user can explicitly reselect a denied source after correcting the host
condition, then wait for its current frame and enable control. Neither a
readiness check nor reselecting the source automatically unlocks the viewer.
Destroyed-window cleanup releases owned presses and clears remembered pointer
state; new input remains bound to the selected window/PID. GPUI shows persistent
blocked-state/focus hints and explicit control actions. Private diagnostic
documents were not copied into this checkpoint.
No network protocol, persistent identity, dependency version or signing policy
has changed.

## Validation and evidence boundary

| Check | Result | Meaning |
| --- | --- | --- |
| Local macOS Host regression | 138 passed, 1 ignored, 0 failed | Includes private-bus Linux adapter, selected-display dispatch and independent permission readiness |
| Local macOS App library / entry | 113 passed, 2 ignored / 1 passed | Full original GPUI suite on the primary branch; no physical presentation/input claim |
| Local original GPUI product | `cargo check -p remote_play_app --locked` passed | Compilation, no product window started |
| Local Ely share controls | 3 passed, 0 ignored, 0 failed | GPUI test-context clicks, selection cancellation and disabled repetition |
| HO5 Host regression | 92 passed, 2 ignored, 0 failed | Native Linux compilation and fixed target effects; no desktop consent fabricated |
| HO5 original GPUI/native video | Native configuration check passed | Compilation separate from actual presentation |
| HO5 App library / entry | 114 passed, 1 ignored / 1 passed | Complete native Linux App regressions; includes Ely share controls |
| HO5 Ely share controls | 3 passed, 0 ignored, 0 failed | Existing target reused; no native visual acceptance |
| Release scripts | 241 total: 234 passed, 7 skipped, 0 failures/errors | `python3 -m unittest discover -s scripts/tests -v` |
| Joint Mac input regression | Host 138 passed/1 ignored; App 116 passed/2 ignored; Python 267 total, 260 passed/7 skipped; 5 native signing fixtures passed | Same shared input integrated with separate Mac GUI/background lifecycle; physical input remains pending |
| HO5 fixed release candidate | Built in 3m 53s; original GPUI/native Linux identity and dynamic dependencies passed | Existing target reused, product actually rebuilt; physical acceptance pending |
| HO5 current maintenance upgrade | Normal same-user stop/atomic replacement/start passed; stable new PID across 6 startup samples and later checks | Same profile and user unit preserved; installed identity and launch binding verified, direct mapped-image proof unavailable |

Fixed tests check target pointer position, click-selected focus between two
test windows, inserted text, scrolling, release of keys/buttons/modifiers,
source generation changes, scale/portrait axes, resize invalidation, denied and
partial grants, owner/session loss, v6 serial requirements, and transport errors
after an observed down event. Shared service tests prove that the Portal path
changes target text while leaving the global injector uncreated. These targets
are private D-Bus fixtures, not the live compositor or permission database.

Transferred deltas were fully hash verified and only changed-file mtimes were
refreshed before HO5 builds. Existing targets were reused. Raw local/remote logs,
operation IDs and process evidence remain in the private parent handoff; they
are not committed as public runtime or device configuration.

## Four-platform physical matrix

See [the directed matrix](2026-10-10-INPUT-STAGE-MATRIX.json): twelve cross-platform
directions plus one user-observed distinct-Mac record, making thirteen records.
That Mac observation still has unspecified directed device roles; it is not two
verified Mac directions. Fix and record both roles separately for bidirectional
Mac acceptance. User observations, native OS facts, isolated installation
fixtures and controlled physical acceptance
are labeled separately.

The user reported Mac-to-Mac video working and mouse/keyboard unresponsive.
Viewer control role and video focus remain part of the physical check; this
observation alone does not establish a root cause. The parent reports 24/24
isolated Mac installation/lifecycle checks, including health failure and rollback;
the daily applications were not replaced by that fixture result.

HO5 had an actual active Wayland RemotePlay window and portal services, but the
OS denied reading its mapped executable. Read-only capability properties report
RemoteDesktop v2, available device mask 7, and ScreenCast v5; these properties
do not grant any session permissions. There is no current presented-frame
or target-input receipt. Historical FFmpeg/placeholder sidecar startup logs do
not prove a current frame. The fresh inventory found the registered Windows and
Android agents offline; installed versions, USB authorization and directed
physical tests remain unknown. Audio, files, direct/relay paths and long-duration
physical acceptance remain untested in this stage. The denied legacy system
service stop was not retried. The existing current user service was normally
upgraded under continuing maintenance authorization, preserving its profile and
launch configuration; OS permissions were not changed.

The [fixed HO5 delivery](2026-10-10-HO5-LINUX-DELIVERY.md) records the actual
compiled candidate, source binding, successful current maintenance upgrade and
verified rollback copies. Old live counters were unavailable; no definite active
use was observed in the preflight, and the maintenance authorization covers one
rollback-protected switch with brief connection reconstruction. Signed Mac input
build 1082 has been handed to its native upgrade task; the earlier 24/24 fixture
result belongs to build 1081.

Next physical acceptance must use the upgraded current executable, normal
local control consent, the viewer's control role and focus, and an observable
target. Record presentation separately from input effects, then repeat source
switch/resize/disconnect release and window isolation checks. Mass deployment
and release publication remain gated on their own signed receipts and acceptance.

API references: [RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html),
[ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html).
