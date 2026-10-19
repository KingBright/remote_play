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

For physical acceptance, use the existing MacBook USB ADB and the exact nubia
serial. Verify the installed version, signer, first-install time and notification
permission before `adb -s 324662166861 install -r APK`. Recheck them after the
upgrade. Launch `.MainActivity`, confirm native initialization succeeds, and
confirm the independent workspace service survives leaving/reopening the Activity.
Do not clear data, uninstall, change security grants or use a different phone as
acceptance for this regression. Record startup and service results separately
from connection, decoded video, input and long-duration acceptance.
