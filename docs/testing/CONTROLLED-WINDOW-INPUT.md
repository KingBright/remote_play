# Controlled window input diagnostic

The guarded example uses the existing RemotePlay V2 SDK. It is a diagnostic client, not a replacement app or a release package. It keeps the installed host responsible for capture, permission checks and input injection.

`window_scoped_acceptance_guarded` selects only an exact Window ID, fixture PID and fresh 32-character hexadecimal fixture nonce. It requires the actual host's matching `Subscribed.supports_input` and four newly decoded window frames. It then sends one center click and the letter `a`, without audio, clipboard, shortcuts or file requests. Two post-input frames are required. The JSON report does **not** claim target effects: those require the fixture's independent counters.

Every success, error, operation timeout and SIGINT/SIGTERM cancellation runs bounded cleanup outside the cancelled operation. Cleanup retries the same owned `ReleaseInput`, `Unsubscribe` and `Close`; it requires matching `InputReleased`, `Unsubscribed` and the exact `Closed` reason `all capture and connection workers stopped`. A wrong ID, missing ACK or timeout-style reason fails cleanup. Late `INPUT_UNAVAILABLE` errors fail the operation even if cleanup succeeds. Do not force-kill the client while its cleanup is running.

## Prepare the target

Compile `scripts/acceptance/GuardedWindowControlFixture.swift` with `xcrun swiftc`, AppKit and CoreGraphics. Only after the bounded live test is authorized, launch it with `WEAVELO_WINDOW_FIXTURE_DIR` pointing at a new private test directory and `WEAVELO_WINDOW_FIXTURE_NONCE` set to a fresh 32-character hexadecimal identifier.

The fixture has two own windows and exits after 180 seconds. Read its `state.json` for the actual PID and A/B Window IDs. It reports mouse/key down/up counts, normalized click point and event source PID only for its own windows. A source PID of zero is unknown, not proof of a particular injector. It does not observe other apps. Its `command.json` supports an explicit `quit` action; use that to close only this fixture after the client has cleaned up.

## Run the diagnostic client

Build/test the example through the repository's established artifact contract:

```text
cargo test --offline --locked -p remote_play_app --example window_scoped_acceptance_guarded --features gpui-native-video
cargo build --offline --locked -p remote_play_app --example window_scoped_acceptance_guarded --features gpui-native-video
```

```text
window_scoped_acceptance_guarded input-window VIEWER_LOCAL_ROUTE FIXTURE_PID WINDOW_ID FIXTURE_NONCE NEW_ABSOLUTE_OUTPUT_JSON
```

Routes are local to their viewer machine. In the verified 2026-10-12 snapshot, Studio's `127.0.0.1:63696` routes to MacBook, while MacBook's `127.0.0.1:58778` routes to Studio. A stale route or failed connection is a failure; do not replay an unknown session. Never subscribe the desktop or choose another window as a fallback.

Before accepting the result, compare this fixture's before/after mouse/key counts, both release counts, expected normalized point and unchanged B counters. Record the client's enqueue result, actual host readiness/errors, post-input window video and strict cleanup separately. The public host interface does not expose independent per-event receive/inject counters; do not invent them. This SDK midpoint test does not by itself validate the human GUI's focus/control collection.

Transfers or live execution needing additional approval must stop at their exact blocked action. Do not change TCC, signing identities, installed apps, profiles or device grants to make the test pass. The original broader `window_scoped_acceptance.rs` remains preserved; use this guarded variant for the bounded input test.
