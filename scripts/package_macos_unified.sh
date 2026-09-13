#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

APP_NAME="${REMOTE_PLAY_APP_NAME:-RemotePlay Unified}"
BUNDLE_ID="${REMOTE_PLAY_BUNDLE_ID:-com.remoteplay.unified}"
VERSION="${REMOTE_PLAY_VERSION:-0.1.0}"

TARGET_DIR="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
PACKAGE_DIR="$ROOT_DIR/target/package/macos"
APP_DIR="$PACKAGE_DIR/${APP_NAME}.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
ZIP_PATH="$PACKAGE_DIR/${APP_NAME}-macos-arm64.zip"

mkdir -p "$PACKAGE_DIR"

echo "Building remote_play release binary..."
cargo build --release -p remote_play_app --bin remote_play

echo "Preparing app bundle at $APP_DIR"
rm -rf "$APP_DIR" "$ZIP_PATH"
if [[ "$APP_NAME" != "RemotePlay" ]]; then
    rm -rf "$PACKAGE_DIR/RemotePlay.app" "$PACKAGE_DIR/RemotePlay-macos-arm64.zip"
fi
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"
cp "$TARGET_DIR/release/remote_play" "$MACOS_DIR/remote_play"

chmod +x "$MACOS_DIR/remote_play"

xattr -cr "$APP_DIR" || true

cat > "$CONTENTS_DIR/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>remote_play</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>RemotePlay uses the microphone when talkback is enabled.</string>
    <key>NSScreenCaptureUsageDescription</key>
    <string>RemotePlay captures the screen when this Mac is being controlled.</string>
</dict>
</plist>
PLIST

if security find-identity -v -p codesigning | grep -q "RemotePlay Local"; then
    echo "Signing with RemotePlay Local identity..."
    codesign --force --deep --sign "RemotePlay Local" --identifier "$BUNDLE_ID" "$APP_DIR"
else
    echo "Signing ad-hoc..."
    codesign --force --deep -s - "$APP_DIR"
fi
codesign --verify --deep --strict "$APP_DIR"
xattr -cr "$APP_DIR" || true

echo "Creating zip package..."
(cd "$PACKAGE_DIR" && COPYFILE_DISABLE=1 zip -qry "$(basename "$ZIP_PATH")" "$(basename "$APP_DIR")")
shasum -a 256 "$ZIP_PATH"
echo "$ZIP_PATH"
