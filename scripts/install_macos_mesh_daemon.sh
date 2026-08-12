#!/usr/bin/env bash
set -euo pipefail

trap 'echo "RemotePlay mesh daemon installation failed near line ${LINENO}." >&2' ERR

LABEL="${REMOTE_PLAY_MESH_DAEMON_LABEL:-com.remoteplay.mesh}"
PLIST_PATH="/Library/LaunchDaemons/${LABEL}.plist"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

resolve_app_dir() {
    if [[ -n "${REMOTE_PLAY_APP_DIR:-}" ]]; then
        (cd "$REMOTE_PLAY_APP_DIR" && pwd)
        return
    fi

    if [[ -x "$script_dir/../../MacOS/remote_play" ]]; then
        (cd "$script_dir/../../.." && pwd)
        return
    fi

    if [[ -x "$script_dir/../target/package/macos/RemotePlay Unified.app/Contents/MacOS/remote_play" ]]; then
        (cd "$script_dir/../target/package/macos/RemotePlay Unified.app" && pwd)
        return
    fi

    if [[ -x "$script_dir/../target/package/macos/RemotePlay.app/Contents/MacOS/remote_play" ]]; then
        (cd "$script_dir/../target/package/macos/RemotePlay.app" && pwd)
        return
    fi

    echo "RemotePlay app could not be located. Set REMOTE_PLAY_APP_DIR=/path/to/RemotePlay Unified.app." >&2
    exit 1
}

APP_DIR="$(resolve_app_dir)"
REMOTE_PLAY_BIN="$APP_DIR/Contents/MacOS/remote_play"
EASYTIER_BIN="$APP_DIR/Contents/Resources/bin/easytier-core"

if [[ ! -x "$REMOTE_PLAY_BIN" ]]; then
    echo "remote_play binary is missing or not executable: $REMOTE_PLAY_BIN" >&2
    exit 1
fi
if [[ ! -x "$EASYTIER_BIN" ]]; then
    echo "easytier-core is missing or not executable: $EASYTIER_BIN" >&2
    exit 1
fi

if [[ "${EUID}" -ne 0 ]]; then
    exec sudo /usr/bin/env \
        REMOTE_PLAY_APP_DIR="$APP_DIR" \
        REMOTE_PLAY_MESH_DAEMON_LABEL="$LABEL" \
        REMOTE_PLAY_DISPLAY_NAME="${REMOTE_PLAY_DISPLAY_NAME:-RemotePlay}" \
        "$0" "$@"
fi

target_user="${REMOTE_PLAY_TARGET_USER:-${SUDO_USER:-}}"
if [[ -z "$target_user" || "$target_user" == "root" ]]; then
    target_user="$(stat -f '%Su' /dev/console)"
fi
if [[ -z "$target_user" || "$target_user" == "root" ]]; then
    echo "Could not determine the RemotePlay desktop user." >&2
    exit 1
fi

target_group="$(id -gn "$target_user")"
target_home="$(dscl . -read "/Users/$target_user" NFSHomeDirectory | awk '{print $2}')"
if [[ -z "$target_home" || ! -d "$target_home" ]]; then
    echo "Could not determine a valid home directory for $target_user." >&2
    exit 1
fi

mesh_dir="${REMOTE_PLAY_MESH_DIR:-$target_home/Library/Application Support/RemotePlay/Mesh}"
display_name="${REMOTE_PLAY_DISPLAY_NAME:-$(scutil --get ComputerName 2>/dev/null || hostname)}"
tmp_plist="$(mktemp "${TMPDIR:-/tmp}/remoteplay-mesh.XXXXXX.plist")"
trap 'rm -f "$tmp_plist"' EXIT

install -d -o "$target_user" -g "$target_group" -m 700 "$mesh_dir"

HOME="$target_home" \
REMOTE_PLAY_DISPLAY_NAME="$display_name" \
REMOTE_PLAY_MESH_DIR="$mesh_dir" \
"$REMOTE_PLAY_BIN" --mesh-ensure-config >/dev/null

chown -R "$target_user:$target_group" "$mesh_dir"
chmod 700 "$mesh_dir"
if [[ -f "$mesh_dir/mesh.conf" ]]; then
    chmod 600 "$mesh_dir/mesh.conf"
fi
if [[ -f "$mesh_dir/mesh.secret" ]]; then
    chmod 600 "$mesh_dir/mesh.secret"
fi

HOME="$target_home" \
REMOTE_PLAY_DISPLAY_NAME="$display_name" \
REMOTE_PLAY_MESH_DIR="$mesh_dir" \
REMOTE_PLAY_EASYTIER_BIN="$EASYTIER_BIN" \
"$REMOTE_PLAY_BIN" --mesh-launchd-plist "$LABEL" > "$tmp_plist"

install -o root -g wheel -m 600 "$tmp_plist" "$PLIST_PATH"

launchctl bootout "system/$LABEL" >/dev/null 2>&1 || true
launchctl bootstrap system "$PLIST_PATH"
launchctl enable "system/$LABEL" >/dev/null 2>&1 || true
launchctl kickstart -k "system/$LABEL" >/dev/null 2>&1 || true

echo "RemotePlay mesh daemon installed: $PLIST_PATH"
echo "Mesh config: $mesh_dir"
