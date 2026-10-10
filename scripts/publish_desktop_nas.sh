#!/usr/bin/env bash
# Publish verified immutable desktop/host packages; never start a client or host.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAGE="${1:?Usage: publish_desktop_nas.sh ARTIFACT_DIRECTORY}"
# Signature verification is required even when metadata claims the package is signed.
# It runs before the first scp/ssh and checks the exact bytes to be published.
python3 "$ROOT/scripts/macos_release_guard.py" verify-stage "$STAGE"
NAS_HOST="${REMOTEPLAY_NAS_HOST:-root@hackerlife.fun}"
NAS_PORT="${REMOTEPLAY_NAS_PORT:-222}"
REMOTE_DIR="${REMOTEPLAY_NAS_DOWNLOAD_DIR:-/opt/remoteplay/downloads}"
BASE="${REMOTEPLAY_DOWNLOAD_BASE_URL:-https://relay.hackerlife.fun:8443/download}"
ARCHIVE="$(mktemp "${TMPDIR:-/tmp}/rp-desktop.XXXXXX.tar")"
VERIFY="$(mktemp "${TMPDIR:-/tmp}/rp-download.XXXXXX")"
trap 'rm -f "$ARCHIVE" "$VERIFY"' EXIT
python3 - "$STAGE" "$ARCHIVE" <<'PY'
from pathlib import Path
import hashlib,json,sys,tarfile
stage=Path(sys.argv[1]); doc=json.loads((stage/'desktop-releases.json').read_text())
entries=doc['releases']; assert len(entries)==3
with tarfile.open(sys.argv[2],'w') as archive:
 for entry in entries:
  name=entry['file']; assert Path(name).name==name
  p=stage/name
  assert p.stat().st_size==entry['size_bytes']
  assert hashlib.sha256(p.read_bytes()).hexdigest()==entry['sha256']
  archive.add(p,arcname=name,recursive=False)
 for name in ['desktop-releases.json',doc.get('acceptance_file', 'acceptance-2.0.0-alpha.2.html'),'index.html']:
  assert Path(name).name == name
  archive.add(stage/name,arcname=name,recursive=False)
PY
REMOTE_ARCHIVE="/tmp/remoteplay-desktop-20260928-$$.tar"
scp -O -P "$NAS_PORT" -o BatchMode=yes -o ConnectTimeout=10 "$ARCHIVE" "$NAS_HOST:$REMOTE_ARCHIVE"
ssh -p "$NAS_PORT" -o BatchMode=yes -o ConnectTimeout=10 "$NAS_HOST" "python3 - '$REMOTE_ARCHIVE' '$REMOTE_DIR'" <<'PY'
from pathlib import Path
import hashlib,json,os,shutil,sys,tarfile,tempfile
archive=Path(sys.argv[1]);root=Path(sys.argv[2]);root.mkdir(parents=True,exist_ok=True)
try:
 with tempfile.TemporaryDirectory(prefix='remoteplay-desktop-') as temp:
  temp=Path(temp)
  with tarfile.open(archive) as tar:
   for item in tar.getmembers():
    assert item.isfile() and Path(item.name).name==item.name,'unexpected archive path/type'
    with tar.extractfile(item) as incoming,(temp/item.name).open('wb') as out:shutil.copyfileobj(incoming,out)
  doc=json.loads((temp/'desktop-releases.json').read_text())
  for entry in doc['releases']:
   name=entry['file'];assert Path(name).name==name
   source=temp/name;assert hashlib.sha256(source.read_bytes()).hexdigest()==entry['sha256']
   target=root/name
   if target.exists():
    assert hashlib.sha256(target.read_bytes()).hexdigest()==entry['sha256'],'immutable artifact conflict'
   else:
    part=root/(name+'.new');shutil.copyfile(source,part);os.chmod(part,0o644);os.replace(part,target)
   print('INSTALLED',name,entry['sha256'],flush=True)
  for name in ['desktop-releases.json',doc.get('acceptance_file', 'acceptance-2.0.0-alpha.2.html'),'index.html']:
   assert Path(name).name == name
   part=root/(name+'.new');shutil.copyfile(temp/name,part);os.chmod(part,0o644);os.replace(part,root/name)
finally:
 archive.unlink(missing_ok=True)
PY
python3 - "$STAGE" <<'PY' > "$STAGE/desktop-sha256.txt"
import json,sys
from pathlib import Path
for p in json.loads((Path(sys.argv[1])/'desktop-releases.json').read_text())['releases']:
 print(p['sha256'],p['file'])
PY
while read -r expected name; do
  curl -fLsS --retry 2 --retry-connrefused --retry-delay 1 --connect-timeout 15 --max-time 300 "$BASE/$name" -o "$VERIFY"
  actual="$(shasum -a 256 "$VERIFY" | awk '{print $1}')"
  test "$actual" = "$expected" || { echo "Public checksum mismatch: $name" >&2; exit 1; }
  printf 'PUBLIC_VERIFIED %s/%s %s\n' "$BASE" "$name" "$actual"
done < "$STAGE/desktop-sha256.txt"
curl -fLsS "$BASE/desktop-releases.json" > "$STAGE/public-desktop-releases.json"
cmp "$STAGE/desktop-releases.json" "$STAGE/public-desktop-releases.json"
