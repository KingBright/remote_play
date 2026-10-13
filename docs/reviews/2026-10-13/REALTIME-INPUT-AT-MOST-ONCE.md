# Realtime input: consume intent before native execution

Remote input now requires a host-issued binding to the current connection,
subscription, source revision and random 128-bit target epoch. The host checks
its actual target and Portal capture generation, not only the subscription ID.
Connection/source changes discard held sender intent; it is never replayed.

Input is stamped before entering the async control queue. The receiver consumes
strictly increasing sequence numbers before native calls or awaits. Duplicates,
out-of-order sequences, events older than 250 ms, future timestamps and expired
leases are discarded. Timestamp estimates come from a correlated binding reply,
biased older by network delay. Native dispatch rechecks the deadline after old
owner cleanup and target validation; Portal also checks before the RPC call.
Unknown native outcomes never restore a consumed sequence or trigger a retry.

Every owned key/button press has its own sequence identity. A 100 ms held-state
snapshot only releases known missing presses and never synthesizes missing
presses. After 300 ms without a matching fresh held-state snapshot, the host
invalidates the binding and cleans only its current owner. Packet age is deducted
from this budget; ordinary motion, unrelated new presses and binding/connection
heartbeats do not extend existing holds. The host checks every 50 ms, giving a
nominal stop bound of 350 ms from the last accepted state creation time when the
executor is scheduled promptly. Executor stalls, native cleanup and OS delivery
can add delay; this is not a hard real-time OS guarantee. A new binding starts
empty, so a delayed snapshot cannot restart an expired hold. Old releases cannot affect a newer target epoch;
releases use this same protected input lane, with a correlated receipt and no
automatic retry. Key-up cannot introduce modifiers, alternate key encodings
share a press identity, and unsupported touch input is dropped.

Portal records downs before RPCs and consumes ups before RPCs. A known expiry
before dispatch is distinguished from a missing reply. On an unknown Portal
transport result the host revokes that exact compositor lease. Mac and Windows
record accepted transitions, discard duplicate downs/unowned ups,
and do not track a down that was definitely never posted. Windows cleanup sends
button-up only for owned buttons. Rejected Windows calls stop the current input
operation: a rejected modifier cannot turn a shortcut into a bare printable key.
Failed cleanup blocks further native input until this injector's owned state is
cleared. Linux uinput cleanup drains its own device's
recorded keys/buttons, and unknown key/button codes no longer become guessed
printable keys or left clicks. A destroyed Mac window never redirects releases
to another application through a global fallback.

The existing session version and enum discriminants remain unchanged; new
variants are appended. Old peers can retain existing media capabilities, but
unsequenced input and releases cannot enable control on the new host. The new
viewer requires input binding readiness and has no legacy input fallback. Both
ends must be updated before physical control acceptance. Legacy ClientSession
input paths have not gained protected input negotiation and fail closed against
this host; independent Remote Hosts callers are outside this protocol lane.

## Main-checkout safety follow-up (2026-10-13)

- Old-owner Release, Held snapshots, key/button-up and modifier reductions update
  only the old sender's logical state and sequence; they cannot take ownership or
  release the current owner's input. Fresh deliberate presses, movement, scrolling
  and newly added modifiers retain normal takeover behavior.
- The receiving lease rejects an explicit key-down whose modifier intent differs
  from accepted modifier state, before adding a held key. Lost/reordered Ctrl-down
  therefore drops Ctrl+C instead of typing c. Legacy KeyDown has no explicit mask
  and keeps its compatibility semantics; key-up never introduces modifiers.
- The pre-existing uinput modifier omission is fixed: modifiers produce actual
  EV_KEY transitions before characters. Each step uses the same deadline; failure
  or expiry prevents the character and cleanup covers only accepted presses.
- Mac selected-window clicks carry that same deadline through WindowServer/AX
  reads to the AXFrontmost write, preventing expired requests from focusing an app.

Validation used `/Users/jinliang/remote_play` on main and its existing Cargo target.
Eight recorded failures first reproduced the owner/protocol/uinput defects. Final
focused checks passed 65 distinct tests: protocol 44, host 18, sender sealing 3.
Host cases use production dispatch/timer bodies with recording injectors; macOS
focus tests record callbacks and uinput tests record writes without native input.
Hold tests cover refresh, dropped snapshots, duplicate/old-epoch/stale traffic,
delayed receipt/polling, drag timeout despite motion and grant requests, and no
automatic restart from old held state. Current-owner release and new takeover pass.

This protocol currently has no supported continuous joystick/velocity control;
touch input is rejected. Discrete clicks, text/key presses, scroll and relative
motion are never replayed by held snapshots. macOS/Windows global native input and
compositor-managed Portal input cannot strictly distinguish overlapping physical
user key state; native queues cannot be retracted and OS delivery latency is not
bounded by the protocol. uinput cleanup targets this injector's own virtual device.
No permission expansion, physical input, deployment or push occurred in this follow-up.

## Earlier checkpoint verification

The new implementation was tested on the managed MacBook checkout's main branch
using its existing owned Rust target, profiles, lock and 8 GiB budget. The primary
repository's existing 104 dirty files remain outside this change. The integrated
base is b83d64d0f939877ecf23df9ebec9eb10d509938e, retaining all 66 migration commits.
The final source manifest and logs are retained in task-4/realtime-input-safety-20261013.

- protocol: 40 passed; remote_core: 301 passed; host: 171 passed, 2 ignored.
- desktop: 120 passed, 2 ignored, including the real loopback protected-wire test.
- Explicit GPUI synthetic event recorder: 1 passed; no native input or screen capture.
- Actual Windows injector source compiled against a recording SendInput API on
  MacBook: 3 ownership/failure-path tests passed, included in the host count.
  This checks source control flow; it does not check Windows ABI or native execution.
- Private D-Bus target: lost key-up reply is observed exactly once, including cleanup.
- Host production ingress: replay, stale events, wrong connection/revision/target,
  target epoch replacement and late old release are rejected before injection.
- The desktop recorder now supports the in-process paired key without mutating
  global pairing state or logging secrets, removing the prior test-order failure.

## Acceptance limits and pending work

These tests verify software admission and bounded native call attempts. Mac HID
and Windows SendInput operate on global OS input state: they cannot atomically
prove ownership of a simultaneously held physical user key, freeze foreground
focus, or retract an already queued OS event. Window IDs/PIDs alone are not an
atomic lifetime lock on the target process. This change therefore does not claim
strict physical-input isolation or a guarantee about when an accepted OS event
is delivered. Microsoft documents both the accepted-event count and interference
from already-held keys in [SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput). Those limitations require a stronger target-scoped OS primitive,
and physical acceptance remains pending.

The new Windows and Linux uinput code has not been natively built or executed in
this run. Linux Portal tests use a private bus on MacBook; they are not desktop
consent receipts. No installed product was replaced. Windows cube is offline;
At the Gateway observation on 2026-10-13 04:28:10 UTC, MacStudio was offline and
in Remote Hosts maintenance draining (reported agent 0.10.37; upgrade state
preflight with target 0.10.35). This is not a product-upgrade success receipt. The old Studio transfer
9532b631-9b71-47bf-b7d5-5ad572cf1fe5 is still paused and was not replayed. HO5's
previous b83 candidate remains immutable; safe idle cannot be established from
its current product because it lacks authoritative live-session counters.

GitHub main push was rejected before process creation by automatic approval
review: the current trusted approval did not name the exact private repository
and full 66-commit source scope. No alternate push route was used. The repository
and scope clarification is pending; local branches and backups remain retained.
