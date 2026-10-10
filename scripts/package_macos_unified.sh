#!/usr/bin/env bash
# Production-only packaging. Missing pinned identity is an error, never ad-hoc fallback.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
[[ "${REMOTE_PLAY_APP_NAME:-RemotePlay}" == "RemotePlay" ]] || { echo "Release app identity cannot be overridden" >&2; exit 1; }
[[ "${REMOTE_PLAY_BUNDLE_ID:-com.remoteplay.unified}" == "com.remoteplay.unified" ]] || { echo "Release bundle ID cannot be overridden" >&2; exit 1; }
# Check before a long build and before touching any previous package.
python3 "$ROOT_DIR/scripts/macos_release_guard.py" preflight
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
BINARY="$TARGET_DIR/release/remote_play"
if [[ -n "${REMOTE_PLAY_PREBUILT_BINARY:-}" ]]; then
  # Remote builders return an artifact, never a private signing key. Retain all
  # product/version/signing gates and require the exact verified build digest.
  BINARY="$(python3 "$ROOT_DIR/scripts/verify_prebuilt_binary.py" \
    --binary "$REMOTE_PLAY_PREBUILT_BINARY" --sha256 "${REMOTE_PLAY_PREBUILT_SHA256:-}")"
else
  cargo build --release -p remote_play_app --bin remote_play --features gpui-restoration --locked -j "${CARGO_BUILD_JOBS:-2}"
fi
python3 "$ROOT_DIR/scripts/verify_desktop_gui.py" --binary "$BINARY" --platform macos --version "$VERSION"
python3 "$ROOT_DIR/scripts/macos_release_guard.py" package \
  --binary "$BINARY" --output "$TARGET_DIR/package/macos" \
  --version "$VERSION" --build "$BUILD_NUMBER"
