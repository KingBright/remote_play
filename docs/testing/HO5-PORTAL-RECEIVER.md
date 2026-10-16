# Coordinated HO5 Portal receiver

The installed Linux alpha.8 already has a local Portal/PipeWire share path.
`MainDisplay` alone selects the legacy FFmpeg path; on KDE Wayland the observed
XWayland root was black. Do not repeat that path as video-content acceptance.
The old short desktop probe cannot wait for a local share or switch sources.

The existing guarded SDK example now has an explicit `wait-shared-display` mode.
It keeps one connection and subscription 256 alive through local selection,
switches to exactly one Portal `Display` with its high bit set and the title
`Shared display`, checks the exact SwitchSource ACK, resets the decoder, resumes,
and requires four fresh decoded frames. It never launches or approves a picker.

## Offline preparation

Use the primary main checkout and the existing validation entry and shared cache:

```
python3 -B scripts/test_stream_settings_model.py --mode input-loss-tests
python3 -B scripts/test_stream_settings_model.py --mode input-loss-build
```

Record the actual helper SHA before a live run. Preparation tests do not grant
capture, local Portal consent or a new desktop time slot.

## One coordinated slot, at most three minutes

1. Obtain a fresh short HO5 slot. Start an owned opaque full-screen synthetic
   fixture on its only monitor, with a fresh 32-hex nonce. Verify its actual
   geometry, focus and deadline before starting this receiver. The receiver's
   bootstrap subscription can briefly start the legacy capture before its pause
   ACK: **never start it over a private or uncoordinated desktop**.
2. Resolve HO5's current existing paired route from the MacBook backend; do not
   reuse an old port or infer a route from another device. Within ten seconds of
   starting the fixture, launch:

   ```
   window_scoped_acceptance_guarded wait-shared-display LOOPBACK_TARGET FIXTURE_NONCE NEW_ABSOLUTE_OUTPUT_JSON --coordinated-synthetic-screen
   ```

   The maximum operation is 100 seconds, plus 12 seconds connection setup and
   up to 24 seconds for cleanup. The fixture must outlive those budgets. No
   receiver GUI opens on MacBook. The paired profile must already exist.
3. Wait for `OUTPUT.waiting.json`: connection ID, subscription 256 and paused
   bootstrap ACK. On HO5 use **Devices → Share this computer → Refresh viewers**.
   Identify this connection/stream and **Display + control**. If several viewer
   rows cannot be distinguished, stop rather than guess a recipient. Choose only the
   synthetic monitor and confirm the normal KDE keyboard/pointer request. Keep
   the same connection; an ended/reconnected viewer invalidates the share target.
4. The helper polls the source catalog, switches the same subscription to the
   newly shared display, and writes `OUTPUT.shared.json` plus `OUTPUT.pgm` (the
   decoded NV12 luma plane on Mac). Inspect the PGM with a trusted viewer or
   losslessly convert it to PNG. It must show the fixture's nonce and changing
   frame counter. A decoder count or this readiness receipt is not visual proof.
5. For video-only completion, create `OUTPUT.stop`. For the safe input suite,
   first independently recheck that the same fixture is focused and covers the
   monitor, and that the decoded image is that fixture. Only then write a private
   same-user `OUTPUT.input-approved.json` containing:

   ```json
   {"fixture_nonce":"THE_SAME_NONCE","source":{"Display":2147483657},"source_revision":900231,"visual_verified":true}
   ```

   Copy the actual source from `OUTPUT.shared.json`; the number above is only a
   format example. The helper refuses mismatched source/nonce/revision or unsafe
   approval-file permissions. It sends the existing 16-event drag, wheel, abc,
   Shift+A and modifier-release suite, without clipboard or system shortcuts.
   It always leaves `target_effect_verified=false`; verify actual target events,
   text, held keys/buttons and focus independently.
6. Require release/unsubscribe/closed ACKs in the final receipt after input.
   For a video-only run the SDK may have no input lease; a missing ReleaseInput
   ACK remains explicitly false, while confirmed unsubscribe/close releases the
   owned subscription. This is not held-input-release acceptance. On HO5 press
   **Stop** for this local share (cancel an unfinished chooser), close the owned
   fixture and verify it exited. Observe closed host capture/stream counters.
   Cancellation, missing sources and timeouts use the same owned cleanup. Do not
   reconnect, guess another display or repeat the fallback on failure.

The mode does not prove sustained video, physical display latency, reverse input,
disconnect/reconnect input release or compositor effects. Record each separately.
