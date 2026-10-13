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
presses. After 750 ms without valid state, the host invalidates the binding and
cleans only its current owner. Old releases cannot affect a newer target epoch;
releases use this same protected input lane, with a correlated receipt and no
automatic retry. Key-up cannot introduce modifiers, alternate key encodings
share a press identity, and unsupported touch input is dropped.

Portal records downs before RPCs and consumes ups before RPCs. A known expiry
before dispatch is distinguished from a missing reply. On an unknown Portal
transport result the host revokes that exact compositor lease. Mac and Windows
record transitions before native calls, discard duplicate downs/unowned ups,
and do not track a down that was definitely never posted. Windows cleanup sends
button-up only for owned buttons. Linux uinput cleanup drains its own device's
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

## Verified source

The new implementation was tested on the managed MacBook checkout's main branch
using its existing owned Rust target, profiles, lock and 8 GiB budget. The primary
repository's existing 104 dirty files remain outside this change. The integrated
base is b83d64d0f939877ecf23df9ebec9eb10d509938e, retaining all 66 migration commits.
The final source manifest and logs are retained in task-4/realtime-input-safety-20261013.

- protocol: 40 passed; remote_core: 301 passed; host: 168 passed, 2 ignored.
- desktop: 120 passed, 2 ignored, including the real loopback protected-wire test.
- Explicit GPUI synthetic event recorder: 1 passed; no native input or screen capture.
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
is delivered. Those limitations require a stronger target-scoped OS primitive,
and physical acceptance remains pending.

The new Windows and Linux uinput code has not been natively built or executed in
this run. Linux Portal tests use a private bus on MacBook; they are not desktop
consent receipts. No installed product was replaced. Windows cube is offline;
MacStudio is in Remote Hosts maintenance draining. The old Studio transfer
9532b631-9b71-47bf-b7d5-5ad572cf1fe5 is still paused and was not replayed. HO5's
previous b83 candidate remains immutable; safe idle cannot be established from
its current product because it lacks authoritative live-session counters.

GitHub main push was rejected before process creation by automatic approval
review: the current trusted approval did not name the exact private repository
and full 66-commit source scope. No alternate push route was used. The repository
and scope clarification is pending; local branches and backups remain retained.
