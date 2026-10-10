# Linux Portal input stage — 2026-10-10

The Linux source now has a consented RemoteDesktop input path, wired into the
existing original GPUI/Ely application and the common host subscription service.
This is a tested source checkpoint. It is not a deployment receipt or a claim
that physical remote control has passed.

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

The macOS permission-error source patch from
`bd7dfa21bab7a088b6858a894bc39e8b578c36c0` is included. Its diagnostic document was
not copied. Additional macOS display-geometry adaptation is separately owned
and must use this common dispatch/lifecycle boundary when its API is delivered.
No network protocol, persistent identity, dependency version or signing policy
has changed.

## Validation and evidence boundary

| Check | Result | Meaning |
| --- | --- | --- |
| Local macOS Host regression | 133 passed, 1 ignored, 0 failed | Includes private-bus Linux adapter and shared dispatch, plus macOS permission error tests |
| Local original GPUI product | `cargo check -p remote_play_app --locked` passed | Compilation, no product window started |
| Local Ely share controls | 3 passed, 0 ignored, 0 failed | GPUI test-context clicks, selection cancellation and disabled repetition |
| HO5 Host regression | 92 passed, 2 ignored, 0 failed | Native Linux compilation and fixed target effects; no desktop consent fabricated |
| HO5 original GPUI/native video | Native configuration check passed | Compilation separate from actual presentation |
| HO5 Ely share controls | 3 passed, 0 ignored, 0 failed | Existing target reused; no native visual acceptance |
| Release scripts | 241 total: 234 passed, 7 skipped, 0 failures/errors | `python3 -m unittest discover -s scripts/tests -v` |
| Earlier joint Mac regression | 267 total: 260 passed, 7 skipped, 0 failures/errors | Earlier joint identity/lifecycle scope; not this platform input matrix |

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
directions plus the user-observed distinct-Mac case. User observations, native
OS facts, isolated installation fixtures and controlled physical acceptance
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
not prove a current frame. Windows/Android require fresh inventory and directed
tests. Audio, files, direct/relay paths and long-duration physical acceptance
remain untested in this stage. The denied legacy system-service stop was not
retried; production services, OS permissions and launch configuration were not
changed by this input work.

Next physical acceptance must use an identity-matched staged executable, normal
local control consent, the viewer's control role and focus, and an observable
target. Record presentation separately from input effects, then repeat source
switch/resize/disconnect release and window isolation checks. Mass deployment
and release publication remain gated on their own signed receipts and acceptance.

API references: [RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html),
[ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html).
