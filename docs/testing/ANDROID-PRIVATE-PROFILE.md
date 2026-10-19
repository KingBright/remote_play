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
