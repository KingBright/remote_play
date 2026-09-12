#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/remote-play-smoke.XXXXXX")"
HOST_LOG="$TMP_DIR/host.log"
CLIENT_LOG="$TMP_DIR/headless-client.log"
SOURCE_FILE="$TMP_DIR/smoke-source.bin"
CLIENT_RECEIVE_DIR="$TMP_DIR/client-received"
HOST_PID=""

cleanup() {
    if [[ -n "$HOST_PID" ]] && kill -0 "$HOST_PID" 2>/dev/null; then
        kill "$HOST_PID" 2>/dev/null || true
        wait "$HOST_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

dd if=/dev/urandom of="$SOURCE_FILE" bs=1024 count=64 status=none

echo "Smoke temp dir: $TMP_DIR"
echo "Host log:       $HOST_LOG"
echo "Client log:     $CLIENT_LOG"

# Build before timing readiness. cargo run can spend the entire startup budget
# compiling or waiting for another cargo process, without ever launching a host.
BUILD_LOG="$TMP_DIR/build.log"
echo "Build log:      $BUILD_LOG"
cargo build -p remote_play_app --bin remote_play >"$BUILD_LOG" 2>&1
cargo build -p remote_core --example headless_smoke_client >>"$BUILD_LOG" 2>&1
SMOKE_TARGET_DIR="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"

REMOTE_PLAY_DATA_PLANE_MEDIA=1 \
REMOTE_PLAY_FILE_TRANSFER=1 \
REMOTE_PLAY_HOST_SEND_FILE="$SOURCE_FILE" \
REMOTE_PLAY_HEADLESS=1 \
REMOTE_PLAY_MESH=0 \
REMOTE_PLAY_DISCOVERY=0 \
REMOTE_PLAY_CLIENT_RECEIVER=0 \
"$SMOKE_TARGET_DIR/debug/remote_play" >"$HOST_LOG" 2>&1 &
HOST_PID="$!"

for _ in {1..80}; do
    if grep -q "Listening for ControlMessages on " "$HOST_LOG"; then
        break
    fi
    if ! kill -0 "$HOST_PID" 2>/dev/null; then
        echo "Host exited before becoming ready."
        tail -80 "$HOST_LOG" || true
        exit 1
    fi
    sleep 0.25
done

if ! grep -q "Listening for ControlMessages on " "$HOST_LOG"; then
    echo "Host did not become ready in time."
    tail -80 "$HOST_LOG" || true
    exit 1
fi

REMOTE_PLAY_EXPECT_DATA_PLANE_MEDIA=1 \
REMOTE_PLAY_EXPECT_AUDIO="${REMOTE_PLAY_EXPECT_AUDIO:-0}" \
REMOTE_PLAY_EXPECT_FILE_NAME="$(basename "$SOURCE_FILE")" \
REMOTE_PLAY_SMOKE_RECEIVE_DIR="$CLIENT_RECEIVE_DIR" \
REMOTE_PLAY_SMOKE_SECONDS="${REMOTE_PLAY_SMOKE_SECONDS:-8}" \
"$SMOKE_TARGET_DIR/debug/examples/headless_smoke_client" >"$CLIENT_LOG" 2>&1

echo "Smoke passed."
tail -40 "$CLIENT_LOG"
