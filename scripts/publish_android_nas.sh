#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "$0")/.." && pwd)"
APK="${1:-$ROOT/android/app/build/outputs/apk/debug/app-debug.apk}"
NAS_HOST="${REMOTEPLAY_NAS_HOST:-root@hackerlife.fun}"
NAS_PORT="${REMOTEPLAY_NAS_PORT:-222}"
REMOTE_DIR="${REMOTEPLAY_NAS_DOWNLOAD_DIR:-/opt/remoteplay/downloads}"
PUBLIC_BASE_URL="${REMOTEPLAY_DOWNLOAD_BASE_URL:-https://relay.hackerlife.fun:8443/download}"
CHANNEL="${REMOTEPLAY_RELEASE_CHANNEL:-debug}"
RELEASE_NOTES="${REMOTEPLAY_RELEASE_NOTES:-RemotePlay Android release}"
INDEX_TEMPLATE="$ROOT/deploy/nas/download/index.html"

SSH=(ssh -p "$NAS_PORT" -o BatchMode=yes -o ConnectTimeout=10)
SCP=(scp -O -P "$NAS_PORT" -o BatchMode=yes -o ConnectTimeout=10)

fail() { printf 'Error: %s\n' "$*" >&2; exit 1; }
require() { command -v "$1" >/dev/null 2>&1 || fail "missing command: $1"; }

for tool in ssh scp curl python3 shasum tar; do require "$tool"; done
[ -f "$APK" ] || fail "APK not found: $APK"
[ -f "$INDEX_TEMPLATE" ] || fail "landing page template not found: $INDEX_TEMPLATE"

GRADLE="$ROOT/android/app/build.gradle.kts"
VERSION="$(sed -n 's/^[[:space:]]*versionName = "\([^"]*\)"/\1/p' "$GRADLE" | head -1)"
VERSION_CODE="$(sed -n 's/^[[:space:]]*versionCode = \([0-9][0-9]*\)/\1/p' "$GRADLE" | head -1)"
MIN_SDK="$(sed -n 's/^[[:space:]]*minSdk = \([0-9][0-9]*\)/\1/p' "$GRADLE" | head -1)"
[ -n "$VERSION" ] || fail "versionName missing from $GRADLE"
[ -n "$VERSION_CODE" ] || fail "versionCode missing from $GRADLE"
[ -n "$MIN_SDK" ] || fail "minSdk missing from $GRADLE"

COMMIT="$(git -C "$ROOT" rev-parse --short=7 HEAD)"
PUBLISHED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
APK_NAME="RemotePlay-Android-${VERSION}-${COMMIT}-${CHANNEL}.apk"
SHA256="$(shasum -a 256 "$APK" | awk '{print $1}')"
SIZE_BYTES="$(stat -f %z "$APK" 2>/dev/null || stat -c %s "$APK")"
PUBLIC_BASE_URL="${PUBLIC_BASE_URL%/}"
REMOTE_ARCHIVE="/tmp/remoteplay-download-${COMMIT}.$$.tgz"

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/remoteplay-publish.XXXXXX")"
VERIFY_APK="$(mktemp "${TMPDIR:-/tmp}/remoteplay-verify.XXXXXX.apk")"
cleanup() { rm -rf "$STAGE" "$VERIFY_APK"; }
trap cleanup EXIT

cp "$APK" "$STAGE/$APK_NAME"
cp "$INDEX_TEMPLATE" "$STAGE/index.html"

export VERSION VERSION_CODE MIN_SDK COMMIT PUBLISHED_AT APK_NAME SHA256 SIZE_BYTES CHANNEL RELEASE_NOTES
python3 - "$STAGE" <<'PY'
import json, os, sys
from pathlib import Path
stage = Path(sys.argv[1])
entry = {
    "version": os.environ["VERSION"],
    "version_code": int(os.environ["VERSION_CODE"]),
    "commit": os.environ["COMMIT"],
    "channel": os.environ["CHANNEL"],
    "published_at": os.environ["PUBLISHED_AT"],
    "file": os.environ["APK_NAME"],
    "size_bytes": int(os.environ["SIZE_BYTES"]),
    "sha256": os.environ["SHA256"],
}
release = {
    "product": "RemotePlay",
    "platform": "android",
    **entry,
    "min_sdk": int(os.environ["MIN_SDK"]),
    "url": f'/download/{entry["file"]}',
    "latest_url": "/download/RemotePlay-Android-latest.apk",
    "notes": [os.environ["RELEASE_NOTES"]],
}
(stage / "release.json").write_text(json.dumps(release, ensure_ascii=False, indent=2) + "\n")
(stage / "entry.json").write_text(json.dumps(entry, ensure_ascii=False, indent=2) + "\n")
PY

tar -C "$STAGE" -czf "$STAGE/bundle.tgz" "$APK_NAME" index.html release.json entry.json

printf 'Publishing RemotePlay Android %s (%s)\n' "$VERSION" "$COMMIT"
"${SCP[@]}" "$STAGE/bundle.tgz" "$NAS_HOST:$REMOTE_ARCHIVE"
"${SSH[@]}" "$NAS_HOST" "bash -s -- '$REMOTE_ARCHIVE' '$REMOTE_DIR' '$APK_NAME' '$SHA256'" <<'REMOTE'
set -euo pipefail
archive="$1"
root="$2"
apk_name="$3"
expected_sha="$4"
tmp="$(mktemp -d /tmp/remoteplay-release.XXXXXX)"
cleanup() { rm -rf "$tmp" "$archive"; }
trap cleanup EXIT
mkdir -p "$root"
tar -xzf "$archive" -C "$tmp"
printf '%s  %s\n' "$expected_sha" "$tmp/$apk_name" | sha256sum -c -

# Versioned files are immutable. Existing same-name content must match exactly.
if [ -e "$root/$apk_name" ]; then
    printf '%s  %s\n' "$expected_sha" "$root/$apk_name" | sha256sum -c -
else
    install -m 0644 "$tmp/$apk_name" "$root/$apk_name.new"
    mv -f "$root/$apk_name.new" "$root/$apk_name"
fi

# Merge the historical release index before switching latest metadata.
python3 - "$root" "$tmp/entry.json" <<'PY'
import json, os, sys
from pathlib import Path
root = Path(sys.argv[1])
entry = json.loads(Path(sys.argv[2]).read_text())
versions = root / "versions.json"
try:
    doc = json.loads(versions.read_text()) if versions.exists() else {}
except Exception:
    doc = {}
releases = doc.get("releases", []) if isinstance(doc.get("releases", []), list) else []
releases = [r for r in releases if r.get("file") != entry["file"] and r.get("commit") != entry["commit"]]
releases.insert(0, entry)
new_doc = {
    "product": "RemotePlay",
    "platform": "android",
    "latest": entry["version"],
    "latest_commit": entry["commit"],
    "releases": releases,
}
tmp = root / "versions.json.new"
tmp.write_text(json.dumps(new_doc, ensure_ascii=False, indent=2) + "\n")
os.chmod(tmp, 0o644)
os.replace(tmp, versions)
PY

for file in index.html release.json; do
    install -m 0644 "$tmp/$file" "$root/$file.new"
    mv -f "$root/$file.new" "$root/$file"
done

ln -sfn "$apk_name" "$root/.RemotePlay-Android-latest.apk.new"
mv -f "$root/.RemotePlay-Android-latest.apk.new" "$root/RemotePlay-Android-latest.apk"
REMOTE

printf 'Verifying public landing page and metadata\n'
curl -fsS "$PUBLIC_BASE_URL/" | grep -F 'RemotePlay / Android' >/dev/null
PUBLIC_RELEASE="$(curl -fsS "$PUBLIC_BASE_URL/release.json")"
printf '%s' "$PUBLIC_RELEASE" | python3 -c 'import json,sys,os; d=json.load(sys.stdin); assert d["sha256"]==os.environ["SHA256"]; assert d["file"]==os.environ["APK_NAME"]'
curl -fsS "$PUBLIC_BASE_URL/$APK_NAME" -o "$VERIFY_APK"
ACTUAL_SHA="$(shasum -a 256 "$VERIFY_APK" | awk '{print $1}')"
[ "$ACTUAL_SHA" = "$SHA256" ] || fail "public APK checksum mismatch: $ACTUAL_SHA"
printf 'Published: %s/\n' "$PUBLIC_BASE_URL"
printf 'APK: %s/%s\n' "$PUBLIC_BASE_URL" "$APK_NAME"
printf 'SHA-256: %s\n' "$SHA256"
