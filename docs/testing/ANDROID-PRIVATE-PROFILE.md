# Android private profile startup regression

The nubia NX741J (Android 16, serial `324662166861`) running code12 reported
`profile-io:app-private:os=13`. The identity writer used a hard link to publish
new private files. Android app-data SELinux policy rejects that operation.

Code13 uses `renameat2(RENAME_NOREPLACE)` for Android first publication, as the
file receiver already does. Publication remains atomic and refuses an existing
destination. Ownership, ordinary-file, single-link and private-mode checks stay
in effect; startup never repairs permissions or discards an orphan secret.
JNI initialization loads the identity and installs its session secret before
creating the bridge client, whose constructor starts discovery on that profile.

Run from the primary Studio checkout on `main`, using its existing lifecycle
binding and Cargo target:

```sh
python3 -B scripts/test_stream_settings_model.py --mode mesh-profile
python3 -B scripts/test_stream_settings_model.py --mode bridge-startup
./scripts/test_release.sh
./scripts/build_android_native.sh
```

The mesh regressions include an existing identity, simultaneous first publishers,
private file modes, owner mismatch, links, orphan secrets and legacy identity
preservation. Native macOS execution covers the platform-neutral publication
contract. Android compilation and the following physical test remain separate
evidence for Android's syscall and SELinux behavior.

The fixed signed output is produced by
`scripts/build_android_release.sh --upgrade-candidate`. This entry requires the
original Android release key and its existing Keychain item on the build machine.
If either is unavailable, report signed packaging as blocked; do not create a
replacement key, copy private keys through tools, build a second source checkout
or bypass the producer. Preserve the original signing material on its authorized
machine. The final APK must pass `verify_release_apk.py` with the original signer
and retain the formal current-output transaction and verification receipt.

For local preparation without any signing credential access, use
`scripts/build_android_release.sh --upgrade-candidate --unsigned-only`.
It retains the original producer lock, public reservation and Gradle gate.
After content/JNI/version/alignment checks it emits `UNSIGNED_APK_READY` and
`unsigned-preparation.json` beside `app-release-unsigned.apk` in the existing
public-owned packaging workspace. Its explicit state is `unsigned-awaiting-signing`;
it has no signer and cannot pass the signed current-output evidence gate.
The producer holds that stage for a bounded 30-minute review window. Expiry or
cancellation uses the original abort/cleanup path and preserves the existing signed
current output. This mode has no transfer or signing endpoint and creates no key.
Cross-device proxy signing is cancelled. Both Macs must keep independent local
build/sign/delivery capability using the same existing Android signing identity.

To retain exactly one prepared APK without rebuilding, the owner starts
`release_artifact_lifecycle.py save-unsigned` with the prepared SHA-256, original
producer PID and observed source commit. The coordinator validates and buffers
the existing bytes, then waits (at most five minutes) for the original task to
finish through its normal cancel/abort path. It does not stop that task or alter
its locks. After the old transaction finishes, the normal producer/preflight,
admission and `release-publish --validation-kind unsigned-dev` save the same bytes
and verified receipt in the established default pair. This is explicitly
`unsigned-awaiting-local-key-import`, never a signing receipt. The original
public authority owns atomic replacement and temporary cleanup; no APK generation,
source copy or extra persistent artifact is created.


Studio local signing setup (the existing MacBook identity remains in place):

1. Run `./scripts/setup_android_release_signing.sh --prepare` in Studio's primary
   checkout. This creates only the private Signing directory and checks Keychain
   item metadata without requesting a password. Default `--check` is read-only.
   This setup entry never generates a signing key.
2. Personally copy the original encrypted
   `~/Library/Application Support/RemotePlay/Signing/remoteplay-release.p12`
   from MacBook to the same path on Studio. Keep the MacBook original.
   On Studio run
   `chmod 600 "$HOME/Library/Application Support/RemotePlay/Signing/remoteplay-release.p12"`.
   Do not attach the file or its password to chat or a tool request.
3. In Studio Keychain Access, create a password item in the existing login keychain
   with name/service `com.remoteplay.android.release.signing`, account `jinliang`,
   and the original p12 password. If needed, personally reveal that existing item
   in MacBook Keychain Access using its normal authentication.
   Keep alias `remoteplay`; do not generate a key or replace an existing item.
4. Run `./scripts/setup_android_release_signing.sh --check`. This confirms only
   local file permissions and item presence. The signed build verifies the actual
   pinned certificate:
   `215aedabf17f40a454ad381d9027772eef9446346cae3db031cac2a5ae51c3a5`.
5. To sign the retained Studio APK without rebuilding, use
   `./scripts/build_android_release.sh --upgrade-candidate --sign-prepared`.
   It reads Studio's existing Keychain through the original signing path, stages
   the registered unsigned current in the original reservation, and invokes local
   SDK apksigner with password environment references. It then applies the
   unchanged signed APK verifier and normal current publication/cleanup.
   Failure preserves the unsigned current. The signed receipt retains the
   original observed build commit and unsigned SHA-256.

MacBook can continue its independent local build/sign/delivery with its existing
key. No cross-machine signing endpoint, private-key transfer API, new credential,
wireless ADB or helper-service authorization is part of this workflow.
The available Remote Hosts tools can prepare non-secret configuration and inspect
status; they do not expose a dedicated Studio credential-entry/takeover surface.
Credential copying and Keychain password entry therefore remain local user steps.

For physical acceptance, use the existing MacBook USB ADB and the exact nubia
serial. Verify the installed version, signer, first-install time and notification
permission before `adb -s 324662166861 install -r APK`. Recheck them after the
upgrade. Launch `.MainActivity`, confirm native initialization succeeds, and
confirm the independent workspace service survives leaving/reopening the Activity.
Do not clear data, uninstall, change security grants or use a different phone as
acceptance for this regression. Record startup and service results separately
from connection, decoded video, input and long-duration acceptance.

## 2026-10-19 MacBook build and nubia upgrade acceptance

The existing MacBook primary checkout `/Users/jinliang/weavelo` was fast-forwarded
from `55681cae510b2cbefd43d68c9ebc0154dea5a557` to
`3081d7ca45b1f9ccf089c6892b5907bf04f3827e`. Both local lifecycle metadata files
retained their exact bytes and permissions as the upstream change made them
machine-local ignored files. The original release binding and shared
`~/rust-target` cache were preserved. No second checkout, replacement signing
identity or remote signing path was used.

One invocation of the formal `--upgrade-candidate` producer rebuilt
`remote_core` and `remote_client_bridge`, then completed Gradle release assembly,
lint and unit tests. The fixed release regressions ran 605 tests: 597 passed
and 8 were conditionally skipped. Original publication reported `current_committed=true`
and `cleanup_status=completed`; the observed build input hashes were unchanged.

| Verified release field | Value |
| --- | --- |
| Package | `fun.hackerlife.weavelo` |
| Version | `2.0.0-alpha.9`, code `13` |
| APK bytes | `16169895` |
| APK SHA-256 | `5f2be85802eb28102284f5df70225edab9151c02eef904a3680be2b0da70aca2` |
| Existing signing certificate SHA-256 | `215aedabf17f40a454ad381d9027772eef9446346cae3db031cac2a5ae51c3a5` |
| Native library SHA-256 | `68f6f475a01bb1f23183cfdc485b284bc42353c724f3a500a9bb8d4847783124` |
| Native gates | ARM64 only, 27 JNI entry points, 16 KiB alignment, non-debuggable |

USB identity was rechecked as nubia NX741J, Android 16 / SDK 36, serial
`324662166861`. The installed code12 APK matched the approved baseline
`d027be0643e9d3872b2f404be35219b32b510fdbe4ebc2397e6b6f058ad331de`;
there were no active install sessions. A single `adb install -r` upgraded it to
code13. The installed APK hash matched the new release. First-install time,
credential/device-encrypted data-directory inodes and runtime permissions
remained unchanged: notifications granted, camera and microphone ungranted.
No uninstall, data clear, permission grant or credential change was performed.

Physical checks passed:

- First cold launch completed and the former `profile-io:app-private:os=13`
  and `Native unavailable` diagnostics disappeared.
- Settings reported `Loaded (.so ready)`. Existing decoder, audio engine and
  gesture-system labels were retained.
- A second cold launch loaded the same persisted network identifier. Its
  SHA-256 was `a3bd51b6989c27e33125f3e4977f0c06aa1391f9c4784a68976d3536eac65043`.
- After 20 seconds on the Home screen, returning to the Activity retained the
  same app process and network identifier with no profile startup error.

The phone had zero linked peers and no existing remote session or workspace.
Connection recovery, an active workspace foreground-service lifecycle, actual
decoded/displayed video, remote input, application file transfer and long-duration
operation remain unverified. Static telemetry labels are not proof of those
paths. Octendra retained discovery-only scope; its device credentials,
accessibility permission and shell activation were not changed.


## 2026-10-19 MacBook code14 QR acceptance

The authorized source was pinned to
`b72416c432105652648c6724cbcf20a7bbc291f7`, including the toolchain and invitation
QR fixes. The original MacBook primary `main` checkout was fast-forwarded to that
exact commit. Only Android `versionCode` was incremented to 14 in build commit
`bce8031379f788607818c01330f516f7a4045e84`. No later Studio implementation was
pulled into this build. The original keychain/signing identity, shared
`/Users/jinliang/rust-target`, source directory and release authority were reused.

The repository-pinned Rust/Cargo 1.99.1 and Android targets were installed through
the existing rustup setup. The existing HotSpot JDK 17 and Android NDK
27.0.12077973 were used. The first fixed-suite attempt timed out waiting for the
initial toolchain installation; the complete suite passed after installation.

The observe-only adoption check then identified an older `Cargo.lock` digest in
the original machine-local binding. Owner reassessment compared that registered
lock from `705a1f029d1e1deedf28df51852896bf9fc552d4` with the pinned source:
third-party package versions, sources and checksums were unchanged. The delta was
the existing application rename/version and client references to already-locked
`cc` and `tempfile`. The original helper's dry-run differed only in that digest.
Only the digest was refreshed, from
`4b080a8b8d834a1e05bd8bef77f24132390b83583d3fbbb4a576441366a833f9` to
`c7fc4e9101e2f80a8388ef43998cbb43f992ab7520809d80a5588e746fb3b9a3`.
The authority inventory, canonical shared target, coverage, budgets, file mode and
all other binding fields were retained; no additional ownership or cleanup scope
was claimed. The original adoption check then passed. The source remained clean
through the guarded tests and formal build.

| Check | Observed result |
| --- | --- |
| Required release-script suite | 606 run: 598 passed, 8 conditional skips |
| Synthetic fixture generator | Passed; profile/network independent, tracked fixture unchanged |
| Rust QR codec | 7 passed |
| Actual pairing controller integration | 7 passed |
| Android JVM suite | 50 passed, including 6 QR tests; no failures or skips |
| Release assembly and lint | Passed through the original formal producer |
| Guarded Rust process groups and source checks | Owned groups absent, source bytes unchanged |

One invocation of `./scripts/build_android_release.sh --upgrade-candidate`
rebuilt the ARM64 native library and published the signed default upgrade output.
Original publication reported `current_committed=true` and
`cleanup_status=completed`. Independent verification of the actual published APK
passed before installation. Read-only inspection warnings for unrelated,
root-owned global LaunchDaemon configurations did not change their permissions.

| Verified release field | Value |
| --- | --- |
| Build source | `bce8031379f788607818c01330f516f7a4045e84` |
| Package | `fun.hackerlife.weavelo` |
| Version | `2.0.0-alpha.9`, code `14` |
| APK bytes | `16686025` |
| APK SHA-256 | `88e5f9f588c689cf2eceb48353608727074719f441eff17436028023a49be931` |
| Existing signing certificate SHA-256 | `215aedabf17f40a454ad381d9027772eef9446346cae3db031cac2a5ae51c3a5` |
| Native library SHA-256 | `269383845dd59f799df6c9896b5e4e855b8a336407e16eaedad4aaa693ea534d` |
| Native gates | ARM64 only, 27 JNI entry points, 16 KiB alignment, non-debuggable |
| Default output | `android/app/build/outputs/apk/release/app-upgrade.apk` with its original verification receipt |

USB nubia NX741J, Android 16 / SDK 36, serial `324662166861` was rechecked.
The code13 installed APK matched the approved baseline, and no install session
was active. A single `adb install -r` at `2026-10-19T17:50:09Z` returned Success.
The installed code14 APK hash matched the new release. First-install time remained
`2026-10-19 10:04:33`; the device-reported update time became
`2026-10-20 01:50:09`. Credential/device-encrypted data-directory inodes and
runtime permission grants were preserved: notifications granted, camera and
microphone ungranted. No uninstall, data clear or permission grant was used.

Physical checks passed:

- Cold launches completed in 169 ms and 137 ms. Settings showed
  `Loaded (.so ready)`; the second process had no fatal-exception,
  `profile-io:app-private:os=13` or `Native unavailable` log markers.
- The persisted network digest remained
  `a3bd51b6989c27e33125f3e4977f0c06aa1391f9c4784a68976d3536eac65043`.
- After 20 seconds on Home, resuming retained the same process and network
  identity; Activity startup reported HOT / 44 ms.
- Scan entry displayed the system camera request. Cancelling that request with
  Back, without granting permission, showed the manual-input fallback and Cancel.
  Cancel returned to Devices with unchanged membership and no invitation scanned.

Camera opening, release/reopen, optical QR scanning, actual membership changes and
real device pairing remain unverified. Real invitations are scanned by the owner
in the ordinary application; no real invitation entered tests, logs or reports.
Desktop Computer Use approval was still unconfirmed and was not bypassed. The
phone still had no linked peer or existing remote workspace. Real video/input,
application file transfer, active workspace-service recovery and long-duration
operation remain unverified. Synthetic test invitations were never joined through
the installed profile. This acceptance does not cover later reverse-QR UI work.


## 2026-10-19 code14 foreground-camera follow-up

The owner explicitly requested invitation QR scanning and authorized its expected
foreground camera consent through Android's standard prompt. The installed
code14 package was retained; this follow-up did not build or install another APK.
Before acting, USB identity, code14, the Devices page and ungranted camera access
were checked. No other camera request or pairing operation had run in this task.

The standard `com.android.permissioncontroller` prompt named Weavelo. Its
`permission_allow_foreground_only_button` (allow only while using the app) was
selected. Android reported `Uid mode: CAMERA: foreground`; CAMERA became granted,
while RECORD_AUDIO remained ungranted and POST_NOTIFICATIONS remained granted.
No `pm grant`, app-op mutation, background grant or platform-consent bypass was
used. No camera image, real invitation, clipboard or credential was read.

Physical observations on nubia serial `324662166861`:

| Action | Observed camera-service result |
| --- | --- |
| Open scanner after standard foreground consent | Back camera 0 owned by Weavelo PID 16980 |
| Tap scanner Cancel | Weavelo removed from Active Camera Clients; Devices page restored |
| Open scanner again | Camera 0 reacquired, scanner visible, no new permission prompt |
| Press system Back | Camera client removed; camera app-op no longer running |

The same app process remained alive, no fatal-exception marker appeared, and the
persisted network digest remained
`a3bd51b6989c27e33125f3e4977f0c06aa1391f9c4784a68976d3536eac65043`.
The final observed UI was Devices with no active camera client. Foreground camera
permission remains available for the owner's normal scanner use.

The focused `QrFrameDecoderTest` suite was rerun offline: 6 passed, no failures,
errors or skips. Synthetic frames verify four orientations of the shared
Rust-generated QR, row padding, pixel stride, crop and buffer-position retention.
Truncated buffers, invalid plane dimensions/stride, blank frames and a non-QR
barcode return no result. Concurrent frames claim one result; cancelled and late
callbacks cannot complete the scan. No synthetic invitation was joined through
the installed profile. These tests establish malformed-frame handling in the
production decoder, not injection of malformed buffers into the physical camera.

Camera opening, Cancel release, reopen and Back release are now physically
verified. Optical recognition of a synthetic displayed card and real invitation
pairing still require the phone to be aimed at the intended target. A separate
non-sensitive invalid test card contains only
`WEAVELO-C14-CAMERA-TEST-NOT-AN-INVITATION`; it is not an RPM2 invitation and cannot
identify a real network. Real pairing remains an owner-performed scan in the
ordinary app. Desktop Computer Use approval remains unconfirmed and was not
bypassed. Video/input, file transfer and long-duration session acceptance remain
unverified.


## 2026-10-19 MacBook code15 reverse-QR acceptance

The original MacBook primary `main` checkout was fast-forwarded from `43c0075`
to the explicitly accepted source `1807d509032458fb86dfda0bad589a7e2e5ca94f`,
including reverse-QR feature `899bf5d`. Only Android `versionCode` was incremented
to 15 in build commit `cf31c391d2e93c146d5e13dd89d537634bd200c1`.
No additional checkout or later unverified implementation was used.

The original release lock was free, no delivery transaction or cleanup lock was
pending, and no build process was active before starting. The existing Rust
1.99.1, HotSpot JDK 17, Android SDK/NDK, signing keychain identity and shared
`/Users/jinliang/rust-target` were reused. The accepted lock-file delta adds six
checksummed crates.io packages for optional desktop QR image recognition; the
Android bridge does not enable that feature. Original-helper reassessment
refreshed only `Cargo.lock` in the machine-local observe-only binding to
`2ccf450879b377afc608d74790fb48bdcbb0568ce6844c6ae81e996b1c469fc3`.
The original authority inventory, canonical cache, budgets, coverage, configuration
and file modes were retained. New dependencies were already in the existing cache.

| Check | Observed result |
| --- | --- |
| Required release-script suite | 607 run: 599 passed, 8 conditional skips |
| Synthetic QR fixture generation | Passed; tracked source remained unchanged |
| Rust QR codec | 7 passed |
| Desktop QR image recognition | 4 passed |
| Actual controller / Android-generated PNG interoperability | 8 passed |
| Read-only invitation export and private-store rejection | 3 passed |
| Android JVM suite | 53 passed, no failures/errors/skips |
| New Android invitation encoder tests | 3 passed: scanner roundtrip, four-module white quiet zone, exact desktop fixture match |
| Formal release assembly and lint | Passed |

Each guarded Rust run reported unchanged source bytes, successful post-observation
and absence of its owned process group. One invocation of the original formal
`--upgrade-candidate` producer rebuilt the native bridge, verified the actual APK,
published the default output and completed temporary-stage cleanup. Independent
verification of the published file also passed before installation.

| Verified release field | Value |
| --- | --- |
| Build source | `cf31c391d2e93c146d5e13dd89d537634bd200c1` |
| Package | `fun.hackerlife.weavelo` |
| Version | `2.0.0-alpha.9`, code `15` |
| APK bytes | `16709412` |
| APK SHA-256 | `c2af1bf851c716a81f88077172318fb52b362e3ceacaed63d7eddbd442703cdf` |
| Existing signing certificate SHA-256 | `215aedabf17f40a454ad381d9027772eef9446346cae3db031cac2a5ae51c3a5` |
| Native library SHA-256 | `259992101f3767b6496a5c6b2ae927837ed92fa0a0921bd05552e8ba8c2b2612` |
| Native gates | ARM64 only, 28 current JNI entry points, 16 KiB alignment, non-debuggable |
| Default output | `android/app/build/outputs/apk/release/app-upgrade.apk` and its original verification receipt |

USB nubia NX741J serial `324662166861` was online. Its installed code14 APK
matched the accepted hash, no install session was active, and the scanner did not
own the camera. A single `adb install -r` at `2026-10-19T18:33:57Z` returned
Success. Installed code15 and its APK hash matched the verified file. First-install
time remained `2026-10-19 10:04:33`; the device-reported update time became
`2026-10-20 02:33:57`. Both private-data directory inodes and all observed runtime
grants were retained: camera and notifications granted, microphone ungranted.
There was no uninstall, data clear, credential change or new permission grant.

The first code15 cold launch completed in 180 ms without the former profile error
or Native unavailable state. **Show invitation QR** opened **Invite a device**,
completed native loading and exposed the generic QR canvas with positive bounds
`[166,730][1050,1614]`. Save PNG, Copy code and Close were enabled. Close returned
to Devices; a second opening loaded correctly, and system Back also dismissed it.
The same app process remained alive, with no fatal-exception marker or active
camera client at the end. The persisted network digest remained
`a3bd51b6989c27e33125f3e4977f0c06aa1391f9c4784a68976d3536eac65043`.

Physical invitation checks used only whitelisted interface metadata and the
network digest. No screenshot, QR pixels, invitation string or clipboard was
captured. Save PNG and Copy code were not pressed; no real invitation file or
network join was produced. Generation, quiet-zone and cross-platform decoding
were tested with synthetic fixtures through the production encoder/controller.

Real optical invitation scanning, actual pairing, real Save PNG/Copy code and
desktop picker/rendering remain unverified here. The owner performs real pairing
and any credential-bearing image transfer in the ordinary apps. This Android
acceptance does not establish desktop package deployment, video/input, file
transfer or long-duration session behavior. Earlier pending optical-test fixtures
and unresolved recovery materials remain protected.


## 2026-10-20 code16 discovery repair: implementation checks

The owner authorized the existing Book primary checkout and a signature-compatible
in-place phone upgrade after restoring the phone network. The owner had already
joined the phone to Book's group. The persisted group name digest matched, but
code15's device list and Book's supported background route count remained empty.
A persisted identity alone does not establish which identity a running worker uses.

A fresh-process regression reproduced a real initialization defect: the locked
bridge dependency graph enables both rustls providers, while JNI does not execute
the desktop entrypoint's provider bootstrap. Before the fix, the synthetic WSS
bind followed by TLS client construction panicked with an ambiguous provider;
after the fix it passed. This proves the code defect, not a recovered historical
phone panic. WSS binding now reuses the existing shared provider initializer and
preserves any process-selected provider, certificates and authentication policy.

The Android bridge now starts P2P DNS independently, bounds startup, subscribes to
cancellation before spawning, observes child completion/panic and aborts/joins its
owned workers on reload. A runtime generation records the identity actually loaded
and acknowledges the corresponding reload request. Relay observations distinguish
connection attempts, successful local registration writes, received Pongs and data.
Registration writes are not server acknowledgments and Pongs are not peer discovery.

A new read-only JNI observation exposes only fixed states/counters and SHA-256 of
public network/device identifiers. It never reads invitations, secrets, routing
capabilities or endpoint URLs. Android parses an explicit whitelist before logging
state transitions; unknown strings and malformed hashes are discarded. Settings
shows connection progress and the empty state retains the existing pairing.

Pre-package checks on this source: relay/routed-relay 21 passed; bridge network
lifecycle 5 passed; release scripts 607 run with 8 conditional skips; Android JVM
56 passed, compileReleaseKotlin and lintRelease passed. The focused native runs used
the original lifecycle authority/binding, locked offline dependencies and canonical
shared target; both confirmed unchanged sources and no remaining owned process group.
Physical discovery and recovery acceptance is recorded separately after deployment.


### Physical code16 observation and final code17 follow-up

The one code16 producer and independent APK verification passed: original signer,
ARM64, 29 JNI symbols, 16 KiB alignment, non-debuggable, 17,333,046 bytes, APK SHA-256
`e4a9fed569bd4745fcc358134a498309725c90b6270efb9ddd372b5df4f3a439`, native SHA-256
`022b34a632f4a8a44ab1a557a00ba072273c0b628f40b3eefe0bbd585863a5e2`.
A single install-r completed at 2026-10-20T05:56:03Z. Installed bytes matched;
first-install time, CE inode 132535, DE inode 132546 and observed grants were retained.
Cold launch took 175 ms. The new Settings UI showed runtime group digest prefix
`e2d45152dc05`, generation 1, reload 0/0 and P2P running. Both relays were connecting
with registration writes/Pongs 0/0; phone devices and Book background routes were 0.

The existing global Android log level was S, so application logs were unavailable;
that setting was preserved. PTY UI hierarchy observations emitted only fixed status
labels, counters, coordinates and public hash prefixes. No hierarchy, invitation or
QR image was retained. At 06:00Z the phone had default network 136 with Wi-Fi/VPN;
shell HTTPS requests to the relay and example.com timed out. At 06:01Z, forced
current public IPv4 addresses removed phone DNS from the test: both TCP connections
completed, but TLS returned error 35 with no completed handshake. These shell probes
are independent transport evidence, not application-UID registration proof. They do
not identify VPN as the cause. Real mutual discovery has not passed in this state.

Final review preserves independent LAN/P2P when local relay binding is unavailable,
keeps the most recent relay retry reason visible until a connection succeeds, and
adds connection-attempt counts and a public local-device hash prefix to Settings.
The follow-up uses code17 so the published code16 artifact is never relabeled.
The owner has been asked to restore usable phone HTTPS while retaining pairing and
keeping Weavelo foreground. Physical mutual discovery and outage recovery remain
pending that condition; no VPN/security configuration was changed by the agent.


### Final code17 package and physical upgrade

Build input was the clean primary-main commit
`b004c30be7a362a35aa6b4478ed86e2f1265026b`. The original formal upgrade producer
completed native build, release assembly, lint, all 56 JVM tests, APK verification,
atomic publication and temporary-stage cleanup. Independent verification read the
published bytes without replacing the producer's receipt. The final native guarded
relay suite passed 22 tests, including retained retry-reason and reconnect evidence.

| Field | Verified value |
| --- | --- |
| Version | 2.0.0-alpha.9, code17 |
| APK size | 17,330,301 bytes |
| APK SHA-256 | `d5be69bd7435e8dbc37d46d556cbfdd997fd66a21f3dda93ca0f060062fe644f` |
| Native SHA-256 | `607daed7b12e63df4d3fd52f09c89f405ea04255910a6b081352bdbded87a04b` |
| Original signer SHA-256 | `215aedabf17f40a454ad381d9027772eef9446346cae3db031cac2a5ae51c3a5` |
| Native gates | ARM64 only, 29 JNI entry points, 16 KiB alignment, non-debuggable |
| Current output | `android/app/build/outputs/apk/release/app-upgrade.apk` |

After online/installed-code16 hash checks, exactly one code17 install-r at
2026-10-20T06:05:12Z returned Success. Installed APK bytes matched. First install
remained 2026-10-19 10:04:33; CE/DE inodes remained 132535/132546. Camera and
notifications remained granted; microphone remained ungranted. Cold launch was
159 ms. No uninstall, data reset, permission change, credential export or re-pair
was performed.

The real Settings UI showed runtime network `e2d45152dc05`, local node
`cf2fe17cf319` (distinct from Book `152f52b34b89`), generation 1, reload 0/0,
P2P running, routes/devices 0/0. Both relays had attempted 2 connections, last
reason closed, registration/Pongs 0/0. Book's supported background status still
reported 0 routes. A fresh credential-free Book WSS upgrade to the same relay
returned HTTP101 with verified TLS; the probe intentionally timed out after 4s
without sending registration data. Existing established TCP sockets alone were
never counted as registration, heartbeat or mutual visibility.

Remaining acceptance blocker: actual phone relay transport has not reached its
first successful registration/Pong, and real mutual visibility/outage recovery are
pending usable phone network. Owner assistance was requested to restore that
condition and retain the current group. No VPN, security or global-log setting was
changed. Desktop GUI Computer Use consent is still unconfirmed; supported CLI
status was used without reading raw Snapshot or collecting invitation data.
Cross-group desktop membership changes remain deferred until Book/phone work.


## Application transport diagnostic validation

The application reports its own default-network observation separately from
shell observations. Existing network-state permission is reused; the code does
not bind/request a network or change VPN, proxy, security or pairing settings.
The production relay path distinguishes DNS, TCP, TLS, WebSocket upgrade,
registration sends and received Pongs. It retains a bounded handshake/retry loop
and the existing provider, trust-root and authentication policies.

Independent same-UID platform TLS/WS and HTTPS controls help locate the first
failing boundary. Strict certificate/hostname and standard WS response checks
remain enabled. The local diagnostic record uses fixed error categories,
counters, numeric status codes and bounded identifier/address hashes. Raw error
messages, response headers, endpoints, credentials and pairing capabilities are
excluded. Detailed per-device network observations remain owner-controlled local
evidence and are not repository publication material.

Two direct Cargo edges reference already-locked TLS libraries. No dependency
version or checksum changed. Original lifecycle reassessment retained authority,
shared-cache targets, budgets and coverage while refreshing only the lock digest.
Focused relay regression: 25 passed, 0 failed. Android JVM: 59 passed,
0 failures/errors/skips. Release Kotlin compilation and lint passed. Required
release-script regression: 607 total, 599 passed, 0 failed, 8 conditional skips.
Synthetic tests cover silent TLS versus silent WS, HTTP rejection/redaction,
standard WS response validation and safe application/native evidence separation.

The original formal producer and independent verifier accepted the current signed
ARM64 candidate, code19, with 29 JNI exports, 16 KiB alignment and non-debuggable
status. APK SHA-256:
`73bf7b95d6b4948ba2b31acf834a04dd6aca2d23ad9f4506348ad19c601e13ff`.
Physical mutual discovery and outage recovery remain incomplete; private evidence
and any required owner action are retained locally. A successful build, TLS
attempt or TCP connection is not counted as successful peer discovery.
The history consolidation changes documentation and synthetic test identifiers
only; the installed candidate's production source bytes are unchanged.
