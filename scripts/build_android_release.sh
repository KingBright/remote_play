#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "$0")/.." && pwd)"
if [[ -z "${REMOTEPLAY_RELEASE_LOCK_FD:-}" ]]; then
    exec python3 -B "$ROOT/scripts/release_artifact_lifecycle.py" run android-release "$@"
fi
python3 -B "$ROOT/scripts/release_artifact_lifecycle.py" check-lock android-release
SIGN_DIR="${REMOTEPLAY_ANDROID_SIGNING_DIR:-$HOME/Library/Application Support/RemotePlay/Signing}"
KEYSTORE="${REMOTEPLAY_ANDROID_KEYSTORE:-$SIGN_DIR/remoteplay-release.p12}"
ALIAS="${REMOTEPLAY_ANDROID_KEY_ALIAS:-remoteplay}"
SERVICE="com.remoteplay.android.release.signing"
ACCOUNT="${USER:-remoteplay}"
SDK="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOME/Library/Android/sdk}}"
if [[ -z "${JAVA_HOME:-}" || ! -x "$JAVA_HOME/bin/java" ]] || "$JAVA_HOME/bin/java" -version 2>&1 | grep -qiE 'OpenJ9|Semeru'; then
    HOTSPOT17="/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home"
    if [[ -x "$HOTSPOT17/bin/java" ]]; then
        export JAVA_HOME="$HOTSPOT17"
        export PATH="$JAVA_HOME/bin:$PATH"
    fi
fi
[ -x "${JAVA_HOME:-}/bin/java" ] || { echo "HotSpot JDK 17 is required for Android release builds" >&2; exit 1; }
[ -f "$KEYSTORE" ] || { echo "Release keystore missing: $KEYSTORE" >&2; exit 1; }
[ -d "$SDK/build-tools" ] || { echo "Android SDK not found: $SDK" >&2; exit 1; }
PASSWORD="$(security find-generic-password -a "$ACCOUNT" -s "$SERVICE" -w)" || { echo 'Release signing password missing from macOS Keychain' >&2; exit 1; }
export ANDROID_HOME="$SDK" ANDROID_SDK_ROOT="$SDK"
export REMOTEPLAY_ANDROID_KEYSTORE="$KEYSTORE" REMOTEPLAY_ANDROID_KEYSTORE_PASSWORD="$PASSWORD" REMOTEPLAY_ANDROID_KEY_ALIAS="$ALIAS" REMOTEPLAY_ANDROID_KEY_PASSWORD="$PASSWORD"
trap 'unset PASSWORD REMOTEPLAY_ANDROID_KEYSTORE_PASSWORD REMOTEPLAY_ANDROID_KEY_PASSWORD' EXIT
cd "$ROOT"
./scripts/build_android_native.sh
cd android
APK_WORKSPACE="$ROOT/android/app/build/outputs/apk/release/.remoteplay-android-package.pending"
./gradlew --no-daemon --no-configuration-cache --max-workers=2 -Pandroid.injected.apk.location="$APK_WORKSPACE" -Pkotlin.compiler.execution.strategy=in-process -Dorg.gradle.jvmargs='-Xmx2g -XX:MaxMetaspaceSize=1g' :app:assembleRelease :app:lintRelease :app:testDebugUnitTest
APK="$APK_WORKSPACE/release/app-release.apk"
[ -f "$APK" ] || { echo "Signed release APK not found: $APK" >&2; exit 1; }
APKSIGNER="$(find "$SDK/build-tools" -maxdepth 2 -type f -name apksigner | sort -V | tail -1)"
[ -x "$APKSIGNER" ] || { echo "apksigner not found under $SDK/build-tools" >&2; exit 1; }
"$APKSIGNER" verify --verbose --print-certs "$APK"
# Calls the unchanged verify_release_apk.verify gate and writes the exact reserved receipt.
python3 -B "$ROOT/scripts/release_artifact_lifecycle.py" publish-android
APK="$ROOT/android/app/build/outputs/apk/release/app-release.apk"
echo "Release APK: $APK"
echo "Size: $(stat -f %z "$APK")"
echo "SHA-256: $(shasum -a 256 "$APK" | awk '{print $1}')"
