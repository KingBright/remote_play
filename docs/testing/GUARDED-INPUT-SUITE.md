# Guarded synthetic-window input suite

Extend the existing `window_scoped_acceptance_guarded` helper; do not use a second
input injector or substitute Remote Hosts input for RemotePlay input. The helper
uses the existing paired SDK route, exact PID/window/nonce catalog binding,
input-readiness checks and owned cleanup. These diagnostic changes do not alter
production input authorization, permissions, capture defaults or installation.

## Build and run

From the primary main checkout, use the existing shared target:

```
python3 -B scripts/test_stream_settings_model.py --mode input-loss-tests
python3 -B scripts/test_stream_settings_model.py --mode input-loss-build
```

Compile `scripts/acceptance/GuardedWindowControlFixture.swift` using the local
approved Xcode toolchain, to the existing owned test-artifact location. Resolve
Xcode license/SDK setup through the device owner; do not accept licenses or
switch toolchains to bypass a refusal. A failed build leaves the old helper
unusable as evidence for the new modes; bind each run to the actual helper hash.

All modes take the same arguments:

```
window_scoped_acceptance_guarded MODE LOOPBACK_TARGET FIXTURE_PID WINDOW_ID FIXTURE_NONCE NEW_ABSOLUTE_OUTPUT_JSON
```

- `input-window` retains the original center click and `a` down/up sequence.
- `input-suite` sends an interior left-button drag, wheel movement, physical
  `abc` keys, and Shift+A with modifier release. It never sends Command, Control,
  Option, clipboard text, commands or arbitrary shortcuts.
- `hold-loss` retains the existing bounded held-input test. After independent
  native held proof, terminate only the owned sender and verify remote release.
- `hold-focus` arms the same harmless held state. After native held proof, write
  a fresh `{id, action: "activate_B"}` command to the owned fixture command file,
  moving focus from owned A to owned B. Observe native releases and no new
  presses in B; preserve the sender's closing/cleanup receipt. This mode does
  not require process loss. Never change focus to user documents or other apps.

The fixture exits automatically after 180 seconds. It retains only bounded
native-event/text evidence: source PID, actual target window, monotonic time,
key down/up, modifier flags, pointer movement/drag and scroll. No microphone,
private application inspection or system input monitor is installed. The sender
remains bounded by its original operation and cleanup budgets.

## Native effect verification

Save fixture state immediately before the suite and after cleanup, retaining
its full bounded event prefix. Then run:

```
python3 scripts/acceptance/inspect_window_input.py --before BEFORE.json --after AFTER.json --sender SDK_RECEIPT.json --backend-pid VERIFIED_REMOTEPLAY_PID
```

For focus release, use the held native baseline as BEFORE and append
`--mode focus-release`. The focus verifier requires the same process/window/nonce,
A-to-B key-window transition, target key/mouse up events attributed to the
verified backend, empty held sets and no new down events in B.

The verifier rejects enqueue-only success, a replaced process/window/nonce,
missing event-prefix evidence, wrong source PID/window, missing drag displacement
or wheel effect, incorrect text/key pairing, missing Shift release or held input.
Sender enqueue, host readiness/cleanup ACKs and target native effects are distinct
claims; no per-event host injection ACK is invented. A rendering submission is
not visual display acceptance. Physical display/acoustic latency remains separate.

Run both directions using each sender's existing loopback route and each target's
own freshly verified backend PID. No local-only result proves reverse input.
Current physical key codes assume the macOS ABC keyboard layout; a different
layout must be reported as such, not remapped silently or called text acceptance.

## 2026-10-16 checkpoint

The existing installed alpha.8/build 20261015.2 passed Studio-to-MacBook visible
synthetic-window display and pause/resume. MacBook-to-Studio reached 24 captured
and encoded frames, 9 decoded frames, nonzero audio, and then phase-0 timeout
(required 15 decoded frames). Source selection and subscribe ACKs matched the
exact fixture. The available diagnostics have no per-connection sent-frame or
packet timeline; these totals do not prove transport loss or a decoder defect.
Source-window geometry also differed between initial and final fixture state;
there is no timestamped geometry history to establish causality. Keep the failure
and add stage observations before changing product behavior or test thresholds.

The new suite has not passed native execution: local Swift/C entrypoints report
an unaccepted Xcode license; the focused Rust build also fails generating
CoreMedia bindings with the previously scoped SDK path. Do not deploy, call the
new modes verified, or use stale binaries until the owner resolves toolchain
setup and the fixed build/tests succeed. Existing basic click/a and sender-loss
release evidence is independent of these unexecuted new cases.
