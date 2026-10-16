#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [[ -z "${WEAVELO_RELEASE_LOCK_FD:-}" ]]; then
  exec python3 -B "$ROOT/scripts/release_artifact_lifecycle.py" run android-native "$@"
fi
python3 -B "$ROOT/scripts/release_artifact_lifecycle.py" check-lock android-native
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
export CXX_aarch64_linux_android="$TOOLCHAIN/aarch64-linux-android${API}-clang++"
export CXX_x86_64_linux_android="$TOOLCHAIN/x86_64-linux-android${API}-clang++"
export CMAKE_TOOLCHAIN_FILE_aarch64_linux_android="$ROOT/scripts/cmake/android-arm64.cmake"
export CMAKE_TOOLCHAIN_FILE_x86_64_linux_android="$ROOT/scripts/cmake/android-x86_64.cmake"

# Only stage ABIs built successfully in this invocation. Never package a stale sibling ABI.
TARGETS="${WEAVELO_ANDROID_TARGETS:-aarch64-linux-android}"
export AR_aarch64_linux_android="$TOOLCHAIN/llvm-ar"
export AR_x86_64_linux_android="$TOOLCHAIN/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="${CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS:-} -C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
export CARGO_TARGET_X86_64_LINUX_ANDROID_RUSTFLAGS="${CARGO_TARGET_X86_64_LINUX_ANDROID_RUSTFLAGS:-} -C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
cd "$ROOT"
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps --locked | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/weavelo-native.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
for target in $TARGETS; do
  case "$target" in
    aarch64-linux-android) abi=arm64-v8a ;;
    x86_64-linux-android) abi=x86_64 ;;
    *) echo "Unsupported Android target: $target" >&2; exit 1 ;;
  esac
  rustup target list --installed | grep -qx "$target" || { echo "Missing Rust target: $target" >&2; exit 1; }
  cargo build -p remote_client_bridge --features android --release --locked --target "$target" -j "${CARGO_BUILD_JOBS:-2}"
  lib="$TARGET_DIR/$target/release/libweavelo_android.so"
  test -s "$lib"
  mkdir -p "$STAGE/$abi"
  cp "$lib" "$STAGE/$abi/libweavelo_android.so"
done
mkdir -p "$OUT_DIR"
# Remove only this product's generated libraries, not third-party JNI files.
find "$OUT_DIR" -name libweavelo_android.so -type f -delete
cp -R "$STAGE"/. "$OUT_DIR"/
echo "Native ABI build complete: $TARGETS"
find "$OUT_DIR" -name libweavelo_android.so -exec shasum -a 256 {} \;
