# Invitation QR pairing

The original GPUI product already generates invitations on macOS, Windows and
Linux. Open the device drawer, select **Pairing**, and show the QR in **Weavelo
Device Group**. On Android, select **Scan to join network**, allow camera access,
and scan the code. A decoded result uses the same asynchronous join operation as
**Enter invitation**. The existing behavior joins immediately after decoding;
there is no additional credential exchange by an agent.

The manual input dialog accepts the original invitation or its QR text. Desktop
**Join from Clipboard** also accepts either form. Android offers **Enter
invitation** and **Cancel** on the scanner, including when camera permission is
denied or no rear camera is available. System back cancels scanning. Invalid
invitations display the existing join error without replacing membership; start
another scan to retry. Active sessions, file reception and screen sharing still
block changing networks through the existing Android join guard.

## Preserved contract

- The QR contains the existing `weavelo://join?port=...&code=...` envelope and RPM2
  invitation. Generation uses the existing invitation checksum validator and
  canonical formatter. Receiving uses the existing `prepare_join`/save/reload
  path; device identity and display name are retained.
- The existing invitation has no independent TTL. QR adds no expiration claim,
  authentication mechanism, credential store, deep-link autojoin, or network
  permission bypass. The port remains existing envelope metadata.
- The original path-form QR and bare/manual codes remain accepted. Empty,
  oversized, lookalike-route, duplicate-code/port and invalid-port envelopes
  fail without echoing credential text. Parsed payload debug output is redacted.
- GPUI uses a full four-module white quiet zone with unshrunk black/white cells.
  Android uses the already-installed CameraX and ZXing libraries; no dependency
  or toolkit replacement is involved.
- Decode runs on a serial worker and copies cropped luminance using row/pixel
  strides without changing the source buffer position. Each image closes on all
  paths. The scanner unbinds only its own use cases on disposal and fences late
  provider/result callbacks and repeated frames.

Android currently consumes invitations; it has no invitation-generation UI.
Desktop has QR generation and text/clipboard joining, but no camera/image-import
scanner. The web viewer has no device-group invitation workflow. This change
does not add those separate capabilities.

## Synthetic regression entrypoints

All test invitations are synthetic and unrelated to installed device profiles.
The fixture generator reads no profile and starts no networking. Run from the
single primary `main` checkout with the existing toolchain/cache contract:

```sh
python3 -B scripts/test_stream_settings_model.py --mode pairing-fixture
python3 -B scripts/test_stream_settings_model.py --mode pairing-qr
python3 -B scripts/test_stream_settings_model.py --mode pairing-join
```

The generator overwrites only `remote_core/tests/fixtures/pairing-qr.txt` with a
fixed synthetic invitation and the actual Rust QR modules. Android unit tests
read that same fixture and decode it in four orientations and with padded,
cropped, strided camera planes. They also exercise blank/non-QR frames,
truncated planes, duplicate-frame contention and cancelled callbacks.
The `pairing-join` integration target directly imports the production
`client/src/mesh_pairing.rs` module, avoiding a full GPUI/media rebuild without
substituting the controller or its storage implementation.

```sh
cd android
./gradlew --offline --no-daemon --max-workers=1 :app:testDebugUnitTest
```

Native camera permission dialogs, camera release/reopen, optical scanning at
different display scales, real device membership and native GPUI presentation
still require physical acceptance. Unit tests and Kotlin compilation do not
establish those outcomes. Real pairing is performed by the owner scanning in the
ordinary app; never export a real invitation into test fixtures, logs or reports.

Formal packages continue through the existing signing and lifecycle scripts;
these source tests do not publish or install an APK or desktop application.

## Mac Studio validation, 2026-10-19

Source baseline: `84d27a3a264651aac6d5ae3630959650ab37a77b`, primary `main`.
Rust/Cargo 1.99.1; existing `~/rust-target` and Gradle caches, no second checkout.

| Check | Observed result |
| --- | --- |
| Rust QR codec and shared fixture | 7 passed |
| Actual pairing controller integration | 7 passed |
| Existing mesh identity, checksum and private-storage tests | 22 passed |
| Android JVM suite | 50 passed, including 6 QR tests; no failures/skips |
| Android debug Kotlin and test compilation | Passed |
| Android debug lint | Passed after resolving already-declared test dependencies |
| Required release-script suite | 605 run: 597 passed, 8 existing skips |
| Final guarded-entry regression after adding the lightweight target | 29 passed |
| GPUI source syntax and Git whitespace check | Passed; not a native build/render test |

Rust runs confirmed unchanged source bytes during execution and clean exit of
their owned process groups. Small local receipts/logs are in the ignored
`.rust-storage/validation-logs/`; Android XML results are in
`android/app/build/test-results/testDebugUnitTest/`.

Not performed: full desktop/native Android release builds, Windows/Linux builds,
real camera/permission/back/reopen interactions, optical scanning, native GUI
visual acceptance, real device pairing, signing, publishing or installation.
The existing Studio `app-upgrade.apk` receipt still describes the earlier
unsigned alpha.9/code 13 candidate, so it is not evidence of this change.
