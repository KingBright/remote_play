# Session recovery: independent-review P2 repairs

The independent review at `independent-remoteplay-session-035713Z` rejected the
previous candidate for duplicate Unsubscribe success after failure and blocking
ScreenCaptureKit stop during Drop. This change repairs both before formal build.

Unsubscribe keeps the first result per subscription ID, including pending/unknown
and failure. Duplicates replay that result; they cannot turn a prior error into
Unsubscribed. Capture and retiring audio/clipboard joins remain owned. Completed
JoinHandles are consumed once; join panics survive pruning and force Close error.

ScreenCaptureKit stop transfers the stream and output handlers to its native
callback context. No synchronous completion wait runs in video/system-audio Drop.
Only a successful native callback destroys the owner and increments completed
stops/decrements native streams. A missing callback retains the owner and pending
count; a failed stop retains the owner and nonzero error/stream count until process
exit. Two-second observation deadlines produce unknown/error, never reclaimed
resources or Closed. Failed/unknown connection receipts retain admission slots
while resources remain live, including across key rotation. Other runtime tasks
and existing healthy sessions can continue. Pause stops the owned capture stream;
resume recreates it only after confirmed completion, preserving the selected source
and media format without overlapping an unconfirmed old owner.

The known-ID IPC scalar receipt includes `native_stops_pending`. Acceptance requires
stable Closed samples with no workers, streams, pending stops or stop errors, plus
actual encoded-frame and completed-stop evidence. Unknown is inspectable and never
passes Closed acceptance.

Validation on MacBook: host library 153 passed, 1 ignored; release Python suite
271 tests, 7 skipped. Added fixtures cover duplicate native-stop failure, audio
cleanup pending, capture join panic, pruning panic, withheld native completion,
disconnect and other-session/timer progress. Two fixtures directly exercise the
production owned-callback bridge with synthetic owners and withheld/error callbacks.
They do not start ScreenCaptureKit or change device/privacy permissions.

The two original 1085 diagnostics still exited 1. This is no claim that real OS
stop never returns or that their authentication timeout has a proven cause. No new
actual capture, remote stream, GUI/input/audio acceptance or installation occurred.

The latest user instruction explicitly requires the unchanged formal observed-build
flow with a fresh target outside source. That formal build instruction takes
precedence over this checkout's warm managed-Cargo target rule only for the formal
candidate. Warm MacBook tests continue using the managed helper. Any Studio formal
build must have an isolated exact-source checkout, a fresh external target AND a
fresh actual intermediate build directory, no shared target, no gate modifications,
an independently pinned source/config identity, adequate admission/reserve, and the
existing fixed signed-package flow on MacBook. No signing keys or credentials move.

The prior reviewed candidate and its handoff remain immutable historical evidence.
Formal build/receipt/package results are recorded separately outside this source.
