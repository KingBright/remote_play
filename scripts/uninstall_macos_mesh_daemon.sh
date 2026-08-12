#!/usr/bin/env bash
set -euo pipefail

LABEL="${REMOTE_PLAY_MESH_DAEMON_LABEL:-com.remoteplay.mesh}"
PLIST_PATH="/Library/LaunchDaemons/${LABEL}.plist"

if [[ "${EUID}" -ne 0 ]]; then
    exec sudo /usr/bin/env REMOTE_PLAY_MESH_DAEMON_LABEL="$LABEL" "$0" "$@"
fi

launchctl bootout "system/$LABEL" >/dev/null 2>&1 || true
rm -f "$PLIST_PATH"

echo "RemotePlay mesh daemon removed: $PLIST_PATH"
