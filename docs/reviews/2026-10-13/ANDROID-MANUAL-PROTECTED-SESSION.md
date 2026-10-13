# Android manual single-viewport protected session — 2026-10-13

The manual address/device connection now uses the same `WorkspaceConnection`
transport and `InputState` as the Android workspace. The Kotlin/JNI entrypoints,
viewport controls and encoded video/audio queues remain in place. There is no
fallback to unbound legacy input.

## Behavior

- Open/Subscribe, rate configuration, pause/resume and Close use versioned Session
  control. Existing pairing authentication, source revisions, enqueue-time input
  binding, at-most-once sequences, host-clock age limits and held-state expiry
  are reused. Only matching held snapshots renew existing presses.
- Keyboard and touch entrypoints require an input-capable subscription, a usable
  first keyframe and a current binding. Ignored pre-connection input cannot
  accumulate local modifiers or gestures. Pause and reconnection discard local
  held state; paused subscriptions do not request or accept input grants.
- Explicit key chords with stale modifier ownership are rejected before changing
  held state. Temporary Shift for uppercase/symbol virtual keys is sent as an
  explicit modifier transition. A dropped modifier cannot become a bare character.
- Close is retried while the receiver remains alive, with a bounded local wait.
  Offline/lost cleanup still relies on the host's existing held-state deadline;
  this is not a guarantee about hard OS scheduling latency.
- Pause acknowledgements, clock synchronization and host telemetry reach the
  existing stats object. Resume requires another keyframe. The shared connection
  uses a sparse five-second Ping keepalive. The manual path starts no file
  receive worker and does not opt into clipboard reception.

## Local validation

The final local logs record 35/35 Android-feature bridge tests and 19/19
shared workspace tests passing. The release-script suite recorded 295 tests,
with 288 passed and seven conditional native-signing skips. Android Gradle
validation succeeded, reusing the existing 30 passing JVM results and reporting
zero lint errors/five existing warnings. See task-local
`android-manual-session-20261013` for logs and the final receipt. Checks cover real bridge keyboard/touch entrypoints against loopback recording
peers, dropped key-up, held snapshots that cannot synthesize a missing down,
duplicate packets, binding expiry, pause/cancel tails, reconnection, first-Close
loss, media keyframe recovery and source-switch isolation. No test injects OS input.

Android checks use the existing Gradle wrapper and installed JDK 17/SDK in offline
mode: compileDebugKotlin, testDebugUnitTest, lintDebug and
compileDebugAndroidTestKotlin. Kotlin sources are unchanged, so cached tasks are
reported as up-to-date rather than newly executed device tests.

## APK and physical acceptance boundary

The existing release entry is `scripts/build_android_release.sh`, which invokes
`build_android_native.sh`, existing release keystore/Keychain signing, Gradle
release checks and `verify_release_apk.py`. The existing keystore, HotSpot JDK 17
and Android SDK are present. No signing password was read; Keychain access and a
successful signed rebuild remain unverified. No credentials were created and no
permissions changed. No new APK was produced, uploaded or installed. Mac-host
Rust tests do not establish the Android NDK/ARM64 ABI, MediaCodec rendering,
physical keyboard/touch effects or long-duration device acceptance.

## Push boundary

This is a local main commit. The previously completed eight-commit push ends at
`b98a37f`; it does not authorize disclosure of subsequent commits. The pending
history also includes preserved dated deployment/incident records containing
local paths, device/network names, process IDs, internal addresses, public
endpoints and signing/artifact fingerprints. These are operational disclosures,
even though the scan found no actual private key or credential material. The
full commit/path inventory and scan are retained in the task-local receipt.
No push or remote deployment was performed for this change.
