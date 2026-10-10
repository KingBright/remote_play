# macOS remote input diagnosis and platform error propagation

The reported Mac-to-Mac video-without-input issue remains an actual-device
acceptance failure. This review establishes current installed identity, finds
one definite error-reporting defect and records what still needs measurement.
It does not count received packets, simulated GPUI events or permission database
rows as target application effects.

## Current devices and identities

At diagnosis time MacBook runs signed `2.0.0-alpha.7`, build `20260930.7`, with
binary SHA-256
`27f4e4f6f7e9cbbbfbf2125f45293b946a5c76bfab1734410b3cd9c9fb5771d0`.
The independently hashed native observer binds its live mapped executable to
that unchanged signed App. MacStudio runs signed `2.0.0-alpha.8`, build
`20261004.80`, with binary SHA-256
`bfb517967a3171c9ce1b3e6814e8a0167470b7a7cc4a795a9ab7e2dec2e67f5b`.
Its launchd PID, canonical executable text mapping and on-disk inode agree.
Native codesign integrity, the exact pinned certificate requirement and its
stable designated requirement pass on both machines.

Both existing launch agents still run the canonical executable without an
explicit background-service argument and with headless disabled. Neither
installed App is the joint observed-build `20261010.1081` candidate. These
current observations do not identify the exact stream/session used when the
user first noticed the failure. Direction, selected source and enabled-control
state remain unknown pending the user's clarification or actual GUI evidence.

MacBook and MacStudio are online through the existing Remote Hosts connection.
The initial Studio read was rejected under the original no-remote-command task
scope. The same command and idempotency key succeeded once the updated explicit
user authorization was supplied. Subsequent bounded reads inspected identities,
launcher, relevant stored permission metadata and error categories only. No
device permission, App, launcher, remote file or other device state was changed.
Windows cube remains offline; its current runtime/input status is not verified.

## Input chain and concrete defect

The original GUI starts each connection in view-only mode. Input requires an
enabled control session, responsive authenticated peer, current source revision
and newly submitted frame, an input-capable source, and appropriate focus. Its
GPUI root captures key down/up; the video surface focuses on click and routes
move/button/scroll events. Existing coordinate mapping accounts for contain and
cover geometry; session/overlay/source transitions release tracked keys,
modifiers and buttons. Host admission checks the negotiated subscription and
source revision before selecting the platform injector.

The macOS desktop injector previously continued creating an event source when
input-posting preflight failed. `CGEvent::post` has no success return, so its
trait implementation could report `Ok` despite an unavailable permission.
The macOS-only `host/src/input_injector.rs` now rejects missing permission before
construction and checks preflight again before each injected event, including
after revocation. It also applies the same check to window construction. The
existing host/session `INPUT_UNAVAILABLE` path reports the concrete host error,
releases held input and relocks control while retaining video. There is no
permission-request API in this fix, no TCC edit, replacement signature, automatic
consent or unconditional capability enablement.

This change corrects hidden failure reporting; it is not proof that missing
permission caused the observed user session. The patch is local source only and
is absent from the independently tested joint `1081` product. The authoritative
producer must consume it in a new observed, pinned-signature candidate before
installed behavior can change.

Only `host/src/input_injector.rs` changes executable behavior. Public protocol,
Session, GUI, host admission and Linux Portal interfaces are unchanged. The
current macOS source catalog intentionally exposes secondary displays with
`supports_input=false`; the desktop injector maps the main display. Enabling
secondary-display control requires preserving the selected display geometry in
the shared host target abstraction and coordinated admission/injection changes.
Do not flip that capability flag alone or change the shared files concurrently
with the Linux Portal owner.

## Permission evidence and boundaries

MacBook's system TCC contains allow records for Accessibility, PostEvent and
ScreenCapture under `com.remoteplay.unified`. Their stored code requirement is
the same bundle identifier and exact pinned leaf certificate as the current App.
This is stored metadata, not a live preflight result from its running host.
An additional Security-framework static validity check returns
`CSSMERR_TP_NOT_TRUSTED` both with the stored requirement and without any
requirement. That baseline failure does not prove a TCC requirement mismatch or
a missing checkbox. No trust-store or permission repair was performed.

Studio's user and system TCC databases cannot be opened by the current ordinary
Remote Hosts session. Its last bounded log segment contains no input, injection
or permission failure messages. Missing messages and unreadable permission
records remain unknown. The foreground GUI event path is separate from global
Input Monitoring; absence of a ListenEvent record is not itself an injection
diagnosis. The relevant injector currently checks PostEvent; window focus also
uses Accessibility.

If the actual signed host's live preflight reports denial, the minimal user
action is to verify that host's canonical RemotePlay App and enable/add that App
in System Settings > Privacy & Security > Accessibility. Confirm the exact
identity before changing any setting. No user permission change is requested
solely on the stored records or trust-check error above.

## Regression evidence and remaining actual-device acceptance

The host library passed 119 tests with one existing ignored case, including
permission-denial propagation, window target safety, pointer bounds and drag
event selection. The application input filter passed 22 tests; its existing
ignored full event-routing regression was then run separately and passed. That
regression uses a temporary loopback protocol recorder and GPUI TestAppContext,
with all real session-key environment variables removed. It observed production
mouse down/up and key down/up handlers and local-overlay isolation, with no
actual host, system capture or OS input injection. Those results are not a
physical input acceptance receipt.

The unchanged release suite passed 263 tests: 256 passed, seven existing skips.
The joint candidate's independent real install/upgrade/rollback acceptance is
documented in [INSTALLER-BUILD-IDENTITY.md](../../testing/INSTALLER-BUILD-IDENTITY.md).

Still required in each Mac direction: the actual viewer/source/control state,
event collection and source-bound transmission, target receipt and live
permission status, then visible effects in a disposable nonsensitive target
window for move, click, drag, scroll, key down/up, modifier combinations and
release. Repeat contain/cover and differing resolution/scaling cases, and
distinguish actual effects from acknowledgement. Existing manual CUA approval
is still pending; no alternative injection/UI route was used to bypass it.
Studio permission state and actual target effect remain unverified. Production
installation, remote deployment, login/reboot and publication did not run.

## Continued visible-state and selected-display preparation

The original session's current input eligibility and reason now reach the GUI
as explicit snapshot fields. A persistent small notice remains visible when
input is blocked, even while the floating toolbar is hidden. It distinguishes
source capability, view-only mode, local overlays and missing keyboard focus,
and preserves the actual host permission error. The control button says
`Enable control`; the permissions drawer disables its input toggle for a source
that cannot accept input. Nothing automatically unlocks a session. Messages
describe the session's current state rather than blaming the user's actions.

The macOS module adds
`MacInputInjector::for_display(id: u32) -> Result<Self, Box<dyn Error + Send + Sync>>`.
It keeps the selected display ID, validates its live active/online status and
finite logical rectangle, and maps normalized coordinates to that display's
global origin. Relative movement is bounded to its rectangle. Pointer press and
scroll require a confirmed point for that target, rather than using an unrelated
local pointer position; absolute and touch coordinates update that point.
Missing or disconnected displays return an error rather than using the main
display. The existing main-display constructor uses the same implementation.

This is a prepared platform interface, not enabled secondary-display support.
Shared host integration remains with the parent task because the Linux owner
is editing the same target dispatcher. Exact required sites in
`host/src/service.rs`: `InputTarget`, `input_target_for`,
`LazyInputInjector::inject_target` and its existing target-release path. Bind a
display ID at source admission and call the new constructor only on macOS.
The macOS branch of `host/src/capture_sources.rs` must retain its conservative
capability until this binding and its regression checks are integrated. No wire
protocol change is required by this prepared interface.

Six selected-platform input tests pass, including negative/above/right origins,
portrait logical rectangles, invalid/offline display refusal and permission
denial. The continued application input filter passes 23 tests. These remain
code regressions; there is still no actual target-effect receipt for two Macs.

The user subsequently authorized a routine same-permission MacBook upgrade
after a new final artifact includes lifecycle, immutable identity and the latest
input changes. Preparation and artifact verification may proceed independently.
The old installation has no read-only active-session diagnostic endpoint, so a
safe switch point is being confirmed before any stop. An unanswered activity
question is not permission to interrupt an unknown live media/input session.
Studio and HO5 installation, OS/TCC changes, public Git push and manual CUA
remain outside that upgrade step.
