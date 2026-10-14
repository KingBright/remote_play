#!/usr/bin/env bash
# Publish verified immutable runtime packages; never start a client or host.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAGE="${1:?Usage: publish_desktop_nas.sh ARTIFACT_DIRECTORY [--macos-only] [--dry-run|--verify-only]}"
shift
MODE=apply
SCOPE=all
VERIFY_OPTIONS=()
for option in "$@"; do
  case "$option" in
    --macos-only) [[ "$SCOPE" == all ]] || exit 2; SCOPE=macos; VERIFY_OPTIONS=(--macos-only) ;;
    apply|--dry-run|--verify-only) [[ "$MODE" == apply ]] || exit 2; MODE="$option" ;;
    *) echo "Unknown publication option: $option" >&2; exit 2 ;;
  esac
done
# The exact macOS signature and source identity are checked before networking.
python3 "$ROOT/scripts/macos_release_guard.py" verify-stage "$STAGE" "${VERIFY_OPTIONS[@]}"
python3 - "$STAGE" "$SCOPE" <<'PREFLIGHT'
from pathlib import Path
import json,sys
stage=Path(sys.argv[1]);scope=sys.argv[2]
doc=json.loads((stage/'desktop-releases.json').read_text())
entries=doc['releases']
assert len({e['file'] for e in entries})==len(entries),'duplicate runtime filenames'
selected=[e for e in entries if 'macos' in e['file'].lower()] if scope=='macos' else entries
assert len(selected)==(1 if scope=='macos' else 3),'unexpected runtime package selection'
for entry in entries:
 name=entry['file'];assert Path(name).name==name and name.endswith(('.zip','.tar.gz','.tgz')),'runtime archives only; no source checkout'
metadata=['desktop-releases.json',doc.get('acceptance_file','acceptance-2.0.0-alpha.2.html'),'index.html']
if scope=='macos':
 metadata+=['desktop-releases.before.json','index.before.html']
 before=json.loads((stage/'desktop-releases.before.json').read_text())
 assert [e for e in entries if 'macos' not in e['file'].lower()]==[e for e in before['releases'] if 'macos' not in e['file'].lower()],'other platform entries changed'
 assert len([e for e in before['releases'] if 'macos' in e['file'].lower()])==1,'missing original macOS release'
 assert selected[0].get('build_identity_expectations'),'macOS update requires verified source identity pins'
 assert doc['acceptance_file']!=before['acceptance_file'],'preserve the previous acceptance page'
 assert {k:v for k,v in doc.items() if k not in ('releases','published_at','acceptance_file')}=={k:v for k,v in before.items() if k not in ('releases','published_at','acceptance_file')},'unrelated publication metadata changed'
 assert (stage/'index.html').read_bytes()==(stage/'index.before.html').read_bytes().replace(before['acceptance_file'].encode(),doc['acceptance_file'].encode()),'index must preserve unrelated content'
for name in metadata:
 assert Path(name).name==name and (stage/name).is_file() and not (stage/name).is_symlink(),'missing/linked publication metadata'
PREFLIGHT

NAS_HOST="${REMOTEPLAY_NAS_HOST:-root@hackerlife.fun}"
NAS_PORT="${REMOTEPLAY_NAS_PORT:-222}"
REMOTE_DIR="${REMOTEPLAY_NAS_DOWNLOAD_DIR:-/opt/remoteplay/downloads}"
BASE="${REMOTEPLAY_DOWNLOAD_BASE_URL:-https://relay.hackerlife.fun:8443/download}"
if [[ "$MODE" == --dry-run ]]; then
  python3 - "$STAGE" "$SCOPE" <<'PLAN'
import hashlib,json,sys
from pathlib import Path
stage=Path(sys.argv[1]);scope=sys.argv[2];doc=json.loads((stage/'desktop-releases.json').read_text())
selected=[e for e in doc['releases'] if scope=='all' or 'macos' in e['file'].lower()]
print(json.dumps({'step':'publish','mode':'dry-run','scope':scope,'NAS_role':'runtime-artifact-receiver-only','NAS_compilation':False,'releases':[{k:e[k] for k in ['file','version','sha256','size_bytes']} for e in selected],'preserved_releases':[e['file'] for e in doc['releases'] if e not in selected],'base_manifest_sha256':hashlib.sha256((stage/'desktop-releases.before.json').read_bytes()).hexdigest() if scope=='macos' else None}))
PLAN
  exit 0
fi

ARCHIVE="$(mktemp "${TMPDIR:-/tmp}/rp-desktop.XXXXXX.tar")"
VERIFY="$(mktemp "${TMPDIR:-/tmp}/rp-download.XXXXXX")"
trap 'rm -f "$ARCHIVE" "$VERIFY"' EXIT
python3 - "$STAGE" "$ARCHIVE" "$SCOPE" <<'PY'
from pathlib import Path
import hashlib,json,sys,tarfile
stage=Path(sys.argv[1]);doc=json.loads((stage/'desktop-releases.json').read_text());scope=sys.argv[3]
entries=[e for e in doc['releases'] if scope=='all' or 'macos' in e['file'].lower()]
with tarfile.open(sys.argv[2],'w') as archive:
 for entry in entries:
  name=entry['file'];assert Path(name).name==name
  p=stage/name;assert p.is_file() and not p.is_symlink()
  assert p.stat().st_size==entry['size_bytes'] and hashlib.sha256(p.read_bytes()).hexdigest()==entry['sha256']
  archive.add(p,arcname=name,recursive=False)
 names=['desktop-releases.json',doc.get('acceptance_file','acceptance-2.0.0-alpha.2.html'),'index.html']
 if scope=='macos':names+=['desktop-releases.before.json','index.before.html']
 for name in names:
  assert Path(name).name==name
  archive.add(stage/name,arcname=name,recursive=False)
PY
REMOTE_ARCHIVE="/tmp/remoteplay-desktop-$(basename "$ARCHIVE")"
if [[ "$MODE" != --verify-only ]]; then
scp -O -P "$NAS_PORT" -o BatchMode=yes -o ConnectTimeout=10 "$ARCHIVE" "$NAS_HOST:$REMOTE_ARCHIVE"
ssh -p "$NAS_PORT" -o BatchMode=yes -o ConnectTimeout=10 "$NAS_HOST" "python3 - '$REMOTE_ARCHIVE' '$REMOTE_DIR' '$SCOPE'" <<'PY'
from pathlib import Path
import fcntl,hashlib,json,os,shutil,sys,tarfile,tempfile
archive=Path(sys.argv[1]);root=Path(sys.argv[2]);scope=sys.argv[3] if len(sys.argv)>3 else 'all'
assert scope in ('all','macos')
assert not root.is_symlink(),'linked publication root'
if scope=='macos':assert root.is_dir(),'existing publication required'
else:root.mkdir(parents=True,exist_ok=True)
def digest(path):
 h=hashlib.sha256()
 with path.open('rb') as stream:
  for block in iter(lambda:stream.read(1024*1024),b''):h.update(block)
 return h.hexdigest()
def regular(path):
 assert path.is_file() and not path.is_symlink(),'missing/linked publication file'
def exact(path,entry):
 regular(path)
 assert digest(path)==entry['sha256'],'runtime archive checksum mismatch'
 if 'size_bytes' in entry:assert path.stat().st_size==entry['size_bytes'],'runtime archive size mismatch'
def install(source,target):
 assert not target.is_symlink(),'linked publication destination'
 if target.exists() and target.read_bytes()==source.read_bytes():return False
 fd,part=tempfile.mkstemp(prefix='.'+target.name+'.',dir=root)
 try:
  with os.fdopen(fd,'wb') as out,source.open('rb') as incoming:shutil.copyfileobj(incoming,out)
  os.chmod(part,0o644);os.replace(part,target)
 finally:
  Path(part).unlink(missing_ok=True)
 return True
try:
 with tempfile.TemporaryDirectory(prefix='remoteplay-desktop-') as temp:
  temp=Path(temp)
  with tarfile.open(archive) as tar:
   names=set()
   for item in tar.getmembers():
    assert item.isfile() and Path(item.name).name==item.name and item.name not in names,'unexpected archive path/type'
    names.add(item.name)
    with tar.extractfile(item) as incoming,(temp/item.name).open('xb') as out:shutil.copyfileobj(incoming,out)
  doc=json.loads((temp/'desktop-releases.json').read_text())
  entries=doc['releases'];selected=[e for e in entries if scope=='all' or 'macos' in e['file'].lower()]
  assert len({e['file'] for e in entries})==len(entries),'duplicate runtime filenames'
  for entry in entries:assert Path(entry['file']).name==entry['file'] and entry['file'].endswith(('.zip','.tar.gz','.tgz'))
  acceptance=doc.get('acceptance_file','acceptance-2.0.0-alpha.2.html')
  assert Path(acceptance).name==acceptance
  metadata=[acceptance,'index.html','desktop-releases.json']
  allowed={e['file'] for e in selected}|set(metadata)
  if scope=='macos':allowed|={'desktop-releases.before.json','index.before.html'}
  assert names==allowed,'unexpected or missing publication files'
  lock=root/'.desktop-publish.lock'
  assert not lock.is_symlink(),'linked publication lock'
  with lock.open('a') as held:
   fcntl.flock(held,fcntl.LOCK_EX)
   if scope=='macos':
    before_raw=(temp/'desktop-releases.before.json').read_bytes();before=json.loads(before_raw)
    assert len(selected)==1 and len([e for e in before['releases'] if 'macos' in e['file'].lower()])==1
    assert selected[0].get('build_identity_expectations'),'missing verified macOS identity pins'
    preserved=[e for e in before['releases'] if 'macos' not in e['file'].lower()]
    assert [e for e in entries if 'macos' not in e['file'].lower()]==preserved,'other platform entries changed'
    assert {k:v for k,v in doc.items() if k not in ('releases','published_at','acceptance_file')}=={k:v for k,v in before.items() if k not in ('releases','published_at','acceptance_file')},'unrelated publication metadata changed'
    assert acceptance!=before['acceptance_file'],'preserve previous acceptance page'
    regular(root/'desktop-releases.json');regular(root/'index.html')
    assert (root/'desktop-releases.json').read_bytes()==before_raw,'publication changed since read; stop and inspect'
    index_before=(temp/'index.before.html').read_bytes()
    assert (root/'index.html').read_bytes()==index_before,'index changed since read; stop and inspect'
    assert (temp/'index.html').read_bytes()==index_before.replace(before['acceptance_file'].encode(),acceptance.encode()),'unrelated index content changed'
    # Preserve and verify all previous runtime packages, including the rollback Mac.
    for entry in before['releases']:
     assert Path(entry['file']).name==entry['file']
     exact(root/entry['file'],entry)
    regular(root/before['acceptance_file'])
    if (root/acceptance).exists():
     regular(root/acceptance)
     assert (root/acceptance).read_bytes()==(temp/acceptance).read_bytes(),'immutable acceptance conflict'
   # Validate every incoming archive and immutable collision before any content write.
   for entry in selected:
    name=entry['file'];exact(temp/name,entry);target=root/name
    assert not target.is_symlink(),'linked runtime destination'
    if target.exists():assert digest(target)==entry['sha256'],'immutable artifact conflict'
   for name in metadata:
    regular(temp/name);assert not (root/name).is_symlink(),'linked metadata destination'
   for entry in selected:
    name=entry['file'];changed=install(temp/name,root/name)
    print('INSTALLED' if changed else 'UNCHANGED',name,entry['sha256'],flush=True)
   previous={name:(root/name).read_bytes() if (root/name).exists() else None for name in metadata}
   changed=[]
   try:
    for name in metadata:
     if install(temp/name,root/name):changed.append(name);print('INSTALLED',name,flush=True)
     else:print('UNCHANGED',name,flush=True)
   except BaseException:
    for name in reversed(changed):
     if previous[name] is None:(root/name).unlink()
     else:
      saved=temp/('restore-'+name);saved.write_bytes(previous[name]);install(saved,root/name)
    raise
finally:
 archive.unlink(missing_ok=True)
PY
fi
# Verify both the updated Mac and all preserved platform bytes over the original URL.
python3 - "$STAGE" <<'PY' > "$STAGE/desktop-sha256.txt"
import json,sys
from pathlib import Path
for entry in json.loads((Path(sys.argv[1])/'desktop-releases.json').read_text())['releases']:
 print(entry['sha256'],entry['file'])
PY
while read -r expected name; do
  curl -fLsS --connect-timeout 15 --max-time 300 "$BASE/$name" -o "$VERIFY"
  actual="$(shasum -a 256 "$VERIFY" | awk '{print $1}')"
  test "$actual" = "$expected" || { echo "Public checksum mismatch: $name" >&2; exit 1; }
  printf 'PUBLIC_VERIFIED %s/%s %s\n' "$BASE" "$name" "$actual"
done < "$STAGE/desktop-sha256.txt"
curl -fLsS --connect-timeout 15 --max-time 30 "$BASE/desktop-releases.json" > "$STAGE/public-desktop-releases.json"
cmp "$STAGE/desktop-releases.json" "$STAGE/public-desktop-releases.json"
