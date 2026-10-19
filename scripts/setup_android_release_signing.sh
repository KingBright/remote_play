#!/usr/bin/env bash
set -euo pipefail
# Existing identity only. Credential copying and password entry remain user actions.
case "${1:---check}" in
  --check|--prepare) MODE="${1:---check}" ;;
  *) echo 'Usage: setup_android_release_signing.sh [--check|--prepare]' >&2; exit 2 ;;
esac
[[ "$#" -le 1 ]] || exit 2
exec python3 -B - "$MODE" <<'PY'
import json
import os
from pathlib import Path
import stat
import subprocess
import sys

prepare = sys.argv[1] == '--prepare'
directory = Path.home() / 'Library/Application Support/RemotePlay/Signing'
if prepare and not directory.exists():
    parent = directory.parent
    if parent.resolve(strict=True) != parent or parent.stat().st_uid != os.getuid():
        raise SystemExit('Existing RemotePlay support directory is not canonical and owned')
    directory.mkdir(mode=0o700)
directory_ok = False
if directory.exists() or directory.is_symlink():
    info = directory.lstat()
    directory_ok = (directory.resolve(strict=True) == directory and stat.S_ISDIR(info.st_mode)
                    and info.st_uid == os.getuid() and stat.S_IMODE(info.st_mode) == 0o700)
    if not directory_ok:
        raise SystemExit('Signing directory must be canonical, owned and mode 0700; nothing changed')
keystore = directory / 'remoteplay-release.p12'
keystore_ok = False
if keystore.exists() or keystore.is_symlink():
    info = keystore.lstat()
    keystore_ok = (keystore.resolve(strict=True) == keystore and stat.S_ISREG(info.st_mode)
                   and info.st_uid == os.getuid() and info.st_nlink == 1
                   and stat.S_IMODE(info.st_mode) == 0o600)
    if not keystore_ok:
        raise SystemExit('Existing p12 must be canonical, owned, single-link and mode 0600; nothing changed')
account = __import__('pwd').getpwuid(os.getuid()).pw_name
result = subprocess.run(['/usr/bin/security', 'find-generic-password', '-a', account,
                         '-s', 'com.remoteplay.android.release.signing'],
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
if result.returncode not in (0, 44):
    raise SystemExit('Keychain metadata lookup failed; no credential was requested or changed')
print(json.dumps(dict(signing_directory=str(directory), directory_ready=directory_ok,
                      keystore_present=keystore_ok, keychain_item_present=result.returncode == 0,
                      account=account, key_alias='remoteplay', credentials_read=False,
                      state='metadata-ready-signer-unverified' if keystore_ok and result.returncode == 0
                      else 'user-local-import-required')))
print('See docs/testing/ANDROID-PRIVATE-PROFILE.md for manual import of the existing identity.')
raise SystemExit(0 if prepare or (directory_ok and keystore_ok and result.returncode == 0) else 2)
PY
