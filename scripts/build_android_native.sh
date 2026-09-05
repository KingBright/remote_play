#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="$ROOT/android/app/src/main/jniLibs"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required" >&2
  exit 1
fi

detect_ndk() {
  if [[ -n "${ANDROID_NDK_HOME:-}" && -d "${ANDROID_NDK_HOME}" ]]; then
    printf '%s\n' "$ANDROID_NDK_HOME"
    return
  fi
  local sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
  if [[ -d "$sdk/ndk" ]]; then
    ls -d "$sdk/ndk"/* 2>/dev/null | sort -V | tail -1
  fi
}

NDK="$(detect_ndk || true)"
if [[ -z "${NDK}" ]]; then
  echo "Android NDK not found. Set ANDROID_NDK_HOME." >&2
  exit 1
fi
export ANDROID_NDK_HOME="$NDK"

HOST_TAG="$(ls "$NDK/toolchains/llvm/prebuilt" | head -1)"
TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin"
API=26

export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$TOOLCHAIN/aarch64-linux-android${API}-clang"
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$TOOLCHAIN/x86_64-linux-android${API}-clang"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
export CC_x86_64_linux_android="$CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER"

mkdir -p "$OUT_DIR/arm64-v8a" "$OUT_DIR/x86_64"

export AR_aarch64_linux_android="${AR_aarch64_linux_android:-$TOOLCHAIN/llvm-ar}"
export AR_x86_64_linux_android="${AR_x86_64_linux_android:-$TOOLCHAIN/llvm-ar}"

if command -v cargo-ndk >/dev/null 2>&1; then
  cargo ndk -t arm64-v8a -t x86_64 -o "$OUT_DIR" --platform "$API" \
    build -p remote_client_bridge --features android --release
else
  echo "Using NDK $NDK ($HOST_TAG)"
  cargo build -p remote_client_bridge --features android --release --target aarch64-linux-android
  if rustup target list --installed | grep -qx 'x86_64-linux-android'; then
    cargo build -p remote_client_bridge --features android --release --target x86_64-linux-android
  else
    echo "Skipping x86_64-linux-android (rust-std not installed)"
  fi
fi

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  CARGO_TARGET_DIR="$(cargo metadata --format-version 1 --no-deps --offline 2>/dev/null \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' 2>/dev/null \
    || true)"
fi
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
if [[ -f "$CARGO_TARGET_DIR/aarch64-linux-android/release/libremote_play_android.so" ]]; then
  cp "$CARGO_TARGET_DIR/aarch64-linux-android/release/libremote_play_android.so" \
    "$OUT_DIR/arm64-v8a/libremote_play_android.so"
fi
if [[ -f "$CARGO_TARGET_DIR/x86_64-linux-android/release/libremote_play_android.so" ]]; then
  cp "$CARGO_TARGET_DIR/x86_64-linux-android/release/libremote_play_android.so" \
    "$OUT_DIR/x86_64/libremote_play_android.so"
fi

echo "Native libraries staged under $OUT_DIR"
ls -l "$OUT_DIR"/* 2>/dev/null || true
