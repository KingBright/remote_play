#!/usr/bin/env bash
# Production-only packaging. Missing pinned identity is an error, never ad-hoc fallback.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -z "${REMOTEPLAY_RELEASE_LOCK_FD:-}" ]]; then
  exec python3 -B "$ROOT_DIR/scripts/release_artifact_lifecycle.py" run macos "$@"
fi
python3 -B "$ROOT_DIR/scripts/release_artifact_lifecycle.py" check-lock macos
cd "$ROOT_DIR"
[[ "${REMOTE_PLAY_APP_NAME:-RemotePlay}" == "RemotePlay" ]] || { echo "Release app identity cannot be overridden" >&2; exit 1; }
[[ "${REMOTE_PLAY_BUNDLE_ID:-com.remoteplay.unified}" == "com.remoteplay.unified" ]] || { echo "Release bundle ID cannot be overridden" >&2; exit 1; }
# Check before a long build and before touching any previous package.
python3 "$ROOT_DIR/scripts/macos_release_guard.py" preflight
: "${REMOTE_PLAY_BUILD_RECEIPT:?Run the existing build_identity_gate.py build first}"
: "${REMOTE_PLAY_BUILD_RECEIPT_SHA256:?Pin the observed build receipt}"
: "${REMOTE_PLAY_EXPECTED_COMMIT:?Pin the reviewed source commit}"
: "${REMOTE_PLAY_EXPECTED_IDENTITY_SHA256:?Pin the compiled build identity}"
: "${REMOTE_PLAY_PREBUILT_BINARY:?Use the binary produced by build_identity_gate.py build in the existing shared cache}"
: "${REMOTE_PLAY_PREBUILT_SHA256:?Pin the exact observed compiler artifact}"
VERSION="${REMOTE_PLAY_VERSION:-$(cat "$ROOT_DIR/VERSION")}"
SUFFIX="$(python3 -c 'import re,sys; m=re.search(r"alpha\.(\d+)$",sys.argv[1]); print(m[1] if m else 0)' "$VERSION")"
BUILD_NUMBER="${REMOTE_PLAY_BUILD_NUMBER:-$(date -u +%Y%m%d).$SUFFIX}"
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps --locked | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
python3 - "$VERSION" <<'CHECK_VERSION'
from pathlib import Path
import sys,tomllib
version=tomllib.loads(Path('app/Cargo.toml').read_text())['package']['version']
if sys.argv[1] != version or Path('VERSION').read_text().strip() != version:
    raise SystemExit('Release tag, VERSION, and compiled crate version must agree')
CHECK_VERSION
# The existing observed-build entry owns identity inputs and incremental Cargo.
# Packaging verifies that exact artifact again before signing and sealing it.
BINARY="$(python3 "$ROOT_DIR/scripts/verify_prebuilt_binary.py" \
  --binary "$REMOTE_PLAY_PREBUILT_BINARY" --sha256 "$REMOTE_PLAY_PREBUILT_SHA256")"
python3 "$ROOT_DIR/scripts/verify_desktop_gui.py" --binary "$BINARY" --platform macos --version "$VERSION"
python3 "$ROOT_DIR/scripts/macos_release_guard.py" package \
  --binary "$BINARY" --output "$TARGET_DIR/package/macos" \
  --version "$VERSION" --build "$BUILD_NUMBER" \
  --build-receipt "$REMOTE_PLAY_BUILD_RECEIPT" \
  --build-receipt-sha256 "$REMOTE_PLAY_BUILD_RECEIPT_SHA256" \
  --expected-commit "$REMOTE_PLAY_EXPECTED_COMMIT" \
  --expected-identity-sha256 "$REMOTE_PLAY_EXPECTED_IDENTITY_SHA256"
