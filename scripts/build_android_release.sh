#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "$0")/.." && pwd)"
export WEAVELO_ANDROID_UPGRADE_CANDIDATE=0
UNSIGNED_ONLY=0
SIGN_PREPARED=0
for option in "$@"; do
    case "$option" in
        --upgrade-candidate) [[ "$WEAVELO_ANDROID_UPGRADE_CANDIDATE" == 0 ]] || exit 2; export WEAVELO_ANDROID_UPGRADE_CANDIDATE=1 ;;
        --unsigned-only) [[ "$UNSIGNED_ONLY" == 0 ]] || exit 2; UNSIGNED_ONLY=1 ;;
        --sign-prepared) [[ "$SIGN_PREPARED" == 0 ]] || exit 2; SIGN_PREPARED=1 ;;
        *) echo 'Only --upgrade-candidate, --unsigned-only and --sign-prepared are supported' >&2; exit 2 ;;
    esac
done
[[ "$UNSIGNED_ONLY" == 0 || "$SIGN_PREPARED" == 0 ]] || { echo 'Unsigned preparation and local signing are separate modes' >&2; exit 2; }
if [[ "$WEAVELO_ANDROID_UPGRADE_CANDIDATE" == 1 ]]; then
    APK_NAME=app-upgrade.apk
    APK_WORKSPACE_NAME=.weavelo-android-upgrade-package.pending
else
    APK_NAME=app-release.apk
    APK_WORKSPACE_NAME=.weavelo-android-package.pending
fi
if [[ -z "${WEAVELO_RELEASE_LOCK_FD:-}" ]]; then
    exec python3 -B "$ROOT/scripts/release_artifact_lifecycle.py" run android-release "$@"
fi
python3 -B "$ROOT/scripts/release_artifact_lifecycle.py" check-lock android-release
# Existing signing material: location/service/alias are intentionally not recreated.
SIGN_DIR="${WEAVELO_ANDROID_SIGNING_DIR:-$HOME/Library/Application Support/RemotePlay/Signing}"
KEYSTORE="${WEAVELO_ANDROID_KEYSTORE:-$SIGN_DIR/remoteplay-release.p12}"
ALIAS="${WEAVELO_ANDROID_KEY_ALIAS:-remoteplay}"
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
[ -d "$SDK/build-tools" ] || { echo "Android SDK not found: $SDK" >&2; exit 1; }
export ANDROID_HOME="$SDK" ANDROID_SDK_ROOT="$SDK"
if [[ "$UNSIGNED_ONLY" == 1 ]]; then
    # Local preparation only: never read signing credentials or publish current.
    unset WEAVELO_ANDROID_KEYSTORE WEAVELO_ANDROID_KEYSTORE_PASSWORD WEAVELO_ANDROID_KEY_ALIAS WEAVELO_ANDROID_KEY_PASSWORD
else
    [ -f "$KEYSTORE" ] || { echo "Release keystore missing: $KEYSTORE" >&2; exit 1; }
    PASSWORD="$(security find-generic-password -a "$ACCOUNT" -s "$SERVICE" -w)" || { echo 'Release signing password missing from macOS Keychain' >&2; exit 1; }
    export WEAVELO_ANDROID_KEYSTORE="$KEYSTORE" WEAVELO_ANDROID_KEYSTORE_PASSWORD="$PASSWORD" WEAVELO_ANDROID_KEY_ALIAS="$ALIAS" WEAVELO_ANDROID_KEY_PASSWORD="$PASSWORD"
    trap 'unset PASSWORD WEAVELO_ANDROID_KEYSTORE_PASSWORD WEAVELO_ANDROID_KEY_PASSWORD' EXIT
fi
cd "$ROOT"
APK_WORKSPACE="$ROOT/android/app/build/outputs/apk/release/$APK_WORKSPACE_NAME"
APKSIGNER="$(find "$SDK/build-tools" -maxdepth 2 -type f -name apksigner | sort -V | tail -1)"
[ -x "$APKSIGNER" ] || { echo "apksigner not found under $SDK/build-tools" >&2; exit 1; }
if [[ "$SIGN_PREPARED" == 1 ]]; then
    python3 -B "$ROOT/scripts/release_artifact_lifecycle.py" stage-unsigned-current
    "$APKSIGNER" sign --ks "$KEYSTORE" --ks-key-alias "$ALIAS" --ks-pass env:WEAVELO_ANDROID_KEYSTORE_PASSWORD --key-pass env:WEAVELO_ANDROID_KEY_PASSWORD --out "$APK_WORKSPACE/release/app-release.apk" "$APK_WORKSPACE/release/app-release-unsigned.apk"
else
    ./scripts/build_android_native.sh
    cd android
    ./gradlew --no-daemon --no-configuration-cache --max-workers=2 -Pandroid.injected.apk.location="$APK_WORKSPACE" -Pkotlin.compiler.execution.strategy=in-process -Dorg.gradle.jvmargs='-Xmx2g -XX:MaxMetaspaceSize=1g' :app:assembleRelease :app:lintRelease :app:testDebugUnitTest
fi
if [[ "$UNSIGNED_ONLY" == 1 ]]; then
    exec python3 -B "$ROOT/scripts/release_artifact_lifecycle.py" hold-unsigned
fi
APK="$APK_WORKSPACE/release/app-release.apk"
[ -f "$APK" ] || { echo "Signed release APK not found: $APK" >&2; exit 1; }
"$APKSIGNER" verify --verbose --print-certs "$APK"
# Calls the unchanged verify_release_apk.verify gate and writes the exact reserved receipt.
python3 -B "$ROOT/scripts/release_artifact_lifecycle.py" publish-android
APK="$ROOT/android/app/build/outputs/apk/release/$APK_NAME"
echo "Release APK: $APK"
echo "Size: $(stat -f %z "$APK")"
echo "SHA-256: $(shasum -a 256 "$APK" | awk '{print $1}')"
