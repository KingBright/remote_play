# Coordinated synthetic HEVC diagnostics

Build the existing guarded example from the primary main checkout:

```sh
python3 -B scripts/test_stream_settings_model.py --mode input-loss-tests
python3 -B scripts/test_stream_settings_model.py --mode input-loss-build
```

The current macOS diagnostic example supports:

```text
window_scoped_acceptance_guarded inspect-hevc LOOPBACK_TARGET NONCE NEW_OUTPUT_JSON --coordinated-synthetic-screen
window_scoped_acceptance_guarded inspect-hevc-input LOOPBACK_TARGET NONCE NEW_OUTPUT_JSON --coordinated-synthetic-screen
window_scoped_acceptance_guarded replay-hevc SYNTHETIC_BOUNDARY_JSON NEW_OUTPUT_JSON --skip-initial-access-unit
```

Network modes reuse the existing paired profile and media-only V2 connection.
They never create credentials, request OS permissions or enable file reception.
The operator must first establish and independently verify an opaque synthetic
fixture covering the physical capture region for a bounded coordinated slot.
Never use either mode while a private desktop could be exposed. Windows currently
advertises MainDisplay; these modes do not weaken the existing exact-window mode.

The tee records production decoder ingress after reassembly/jitter, with unchanged
payloads. It retains at most 96 metadata rows and 32 access units / 4 MiB of raw
synthetic data. JSON, HEVC and PGM evidence are private, newly created files. A
successful Subscribe or decoded counter is not visual verification: inspect the
actual decoded nonce and changing marker independently. Setup is bounded to 12s,
video observation to 12s, and each cleanup acknowledgement to 5s.

Input mode writes video-ready.json, then waits at most 45s for click-approved.json.
It sends only a normalized center move and one left-button down/up. It writes
clicked.json, then waits at most 45s for text-approved.json. The coordinator must
independently confirm native mouse events and exact fixture foreground before
allowing keys. Missing, malformed, non-private, mismatched or expired approvals
fail closed. The separate text is abc plus the first four hexadecimal nonce
characters, sent as physical down/up pairs with no modifiers or clipboard.

Each approval is a private same-user JSON file at the output's extension path:
fixture_nonce, phase (click/text), source=MainDisplay, source_revision=0,
coverage_verified=true, visual_verified=true, positive fixture_pid/fixture_window,
and expires_local_unix_ms (no more than 20s ahead). Text additionally requires
foreground_is_fixture=true and native_click_verified=true. These are evidence
confirmations by the coordinator, not replacements for human authorization.

Creating OUTPUT.stop cancels a waiting stage. Cleanup releases owned input,
unsubscribes and closes the exact connection; input sent requires a release ACK.
Native target effects and held-state release still require independent evidence.
Authentication/open elapsed time uses the local monotonic clock and is neither
RTT nor one-way latency.

Offline replay uses only an existing synthetic sample, no network or input.
Run both comparison cases in the same native VideoToolbox environment; a
sandbox session-creation failure must not be mistaken for a codec regression.
