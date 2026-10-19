# Invitation QR pairing

The original GPUI product generates invitations on macOS, Windows and Linux.
Open the device drawer, select **Pairing**, and show the QR in **Weavelo Device
Group**. Android also generates an invitation: select **Show invitation QR**.
This explicitly reads the existing private group through the native bridge; it
does not create another group or include secrets in periodic telemetry.

On Android, select **Scan to join network**, allow camera access, and scan the
other device's QR. A decoded result uses the same asynchronous join operation as
**Enter invitation**. To join a desktop to an Android group, save the phone's QR
with **Save PNG**, transfer the image yourself, then select **Scan QR Image…**
in desktop **Pairing**. The desktop recognizes PNG, JPEG and WebP images locally
and delegates the validated invitation to the existing join operation. Both
paths join immediately after decoding; no agent transfers real credentials.

The manual input dialog accepts the original invitation or its QR text. Desktop
**Join from Clipboard** also accepts either form. Android offers **Enter
invitation** and **Cancel** on the scanner, including when camera permission is
denied or no rear camera is available. System back cancels scanning. Invalid
invitations display the existing join error without replacing membership; start
another scan to retry. Active sessions, file reception and screen sharing still
block changing networks through the existing Android join guard.

Desktop image selection can be cancelled, and an in-flight decode can be
cancelled from the pairing card. Late results and duplicate submissions cannot
join. An invalid or unreadable image leaves membership unchanged; an image with
different valid invitations must be cropped to one. The original active-session
and file-transfer guards run again before joining. If the group changes while
the file picker is open, a new scan is required.

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
- GPUI and Android use a full four-module white quiet zone with unshrunk
  black/white cells. Android reuses CameraX and ZXing. Desktop image recognition
  uses pinned `rqrr` and the existing `image` version under the `pairing-image`
  feature; these decoder dependencies are not enabled in the Android bridge.
  No UI toolkit is replaced.
- Decode runs on a serial worker and copies cropped luminance using row/pixel
  strides without changing the source buffer position. Each image closes on all
  paths. The scanner unbinds only its own use cases on disposal and fences late
  provider/result callbacks and repeated frames.

Desktop supports QR image import, not live camera scanning. Clipboard text
joining remains a separate fallback and is not described as QR recognition.
The web viewer has no device-group invitation workflow.

## Synthetic regression entrypoints

All test invitations are synthetic and unrelated to installed device profiles.
The fixture generator reads no profile and starts no networking. Run from the
single primary `main` checkout with the existing toolchain/cache contract:

```sh
python3 -B scripts/test_stream_settings_model.py --mode pairing-fixture
python3 -B scripts/test_stream_settings_model.py --mode pairing-qr
python3 -B scripts/test_stream_settings_model.py --mode pairing-image
python3 -B scripts/test_stream_settings_model.py --mode pairing-join
python3 -B scripts/test_stream_settings_model.py --mode bridge-invitation
```

The generator overwrites only `remote_core/tests/fixtures/pairing-qr.txt` with a
fixed synthetic invitation and the actual Rust QR modules. Android unit tests
read that same fixture and decode it in four orientations and with padded,
cropped, strided camera planes. They also exercise blank/non-QR frames,
truncated planes, duplicate-frame contention and cancelled callbacks.
The `pairing-join` integration target directly imports the production
`client/src/mesh_pairing.rs` module, avoiding a full GPUI/media rebuild without
substituting the controller or its storage implementation. It also rasterizes
the actual Android-generated matrix, reads the PNG through the desktop file
decoder, and joins through that controller while retaining device identity.
The bridge export tests verify read-only behavior and private-store rejection.

```sh
cd android
./gradlew --offline --no-daemon --max-workers=1 :app:testDebugUnitTest
```

`InvitationQrTest` compares the production Android encoder against
`remote_core/tests/fixtures/android-pairing-qr.txt`, a synthetic fixture including
its white margin. To regenerate it after an intentional encoder change, run the
same test with `WEAVELO_UPDATE_QR_FIXTURE=1`; the generator verifies the fixed
synthetic source fixture and never reads installed device configuration.

Native camera permission dialogs, camera release/reopen, optical scanning at
different display scales, Android save/copy actions, native desktop file-picker
interactions, real device membership and native GPUI presentation
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

## Reverse-flow validation

Feature baseline: `b72416c432105652648c6724cbcf20a7bbc291f7`, primary `main`.
Before committing, Book's code 14 and acceptance changes through `12f858e` were
fast-forwarded; all 20 local feature files were retained byte-for-byte.
The subsequent documentation-only acceptance commit `43c0075` was also retained
before pushing the feature; it changed none of the validated feature source.
This follow-up adds Android invitation display/save and desktop QR image import.
It does not change app versions, the Rust toolchain, signing or installed apps.

| Check | Observed result |
| --- | --- |
| Desktop image decoding, invalid/large images, ambiguity and cancellation | 4 passed |
| Actual controller, including Android-generated PNG import and repeat join | 8 passed |
| Native invitation export with Android JNI enabled on macOS | 3 passed; read-only and private-store protections retained |
| Android JVM suite | 53 passed, no failures/errors/skips |
| Android debug Kotlin/test compilation and lint | Passed |
| Required release-script suite | 606 run: 598 passed, 8 existing skips |
| Guarded-entry regression | 29 passed |
| macOS GPUI product compile check | Incomplete: the existing 1,800-second guard ended dependency compilation with exit 124; no compiler error was reported before the deadline |

The Rust test receipts report unchanged source bytes and exited owned process
groups. Evidence is in `.rust-storage/validation-logs/pairing-reverse-*.log`
and the corresponding private runner receipts. Android results remain in the
default `android/app/build/test-results/testDebugUnitTest/` directory.
The desktop compile receipt is `platform-macos-check-9kgimdm2.jsonl`: its owned
process group exited, post-observation passed and source bytes stayed unchanged.
The product UI was not reached, so this is not a successful desktop build. The
remaining check needs a later coordinated run using the same retained cache;
no second Rust build was started after the coordinated run reached its deadline.

No APK or desktop package was produced or installed by this follow-up. Book's
separately accepted code 14 uses build source `bce8031` and acceptance commit
`12f858e`; it does not include these reverse-flow changes. Native Android
save/copy/camera interaction for this follow-up, desktop picker/rendering,
Windows/Linux compilation and real pairing remain untested. The owner must
perform real pairing; test and report artifacts contain only synthetic invites.
