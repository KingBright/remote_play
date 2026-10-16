#!/usr/bin/env python3
"""Install the verified Weavelo desktop entry and hicolor assets as one user transaction.

No root, service control, package manager, shared icon-cache rewrite or permissions repair.
Plans are read-only. Apply/recover require the exact reviewed plan hash.
"""
from __future__ import annotations
import argparse
import ctypes
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
import pwd
from pathlib import Path
import stat
import sys
import tempfile

import build_identity_gate as gate

APP_ID = 'fun.hackerlife.weavelo'
SIZES = (16, 32, 48, 64, 128, 256, 512)


def require(ok, message):
    if not ok: raise ValueError(message)


def sha(raw): return hashlib.sha256(raw).hexdigest()
def plan_hash(plan): return sha(gate.canonical(plan))


def safe(path):
    require(path.is_absolute() and '..' not in path.parts, 'Unsafe desktop installation path')
    for p in [path, *path.parents]:
        require(not p.is_symlink(), 'Linked desktop installation path')
        if p.exists() and (p == path or p.is_relative_to(Path.home())):
            require(p.lstat().st_uid == os.getuid(), 'Desktop path is not user-owned')


def state(path):
    safe(path)
    if not path.exists(): return None
    m = path.lstat()
    require(stat.S_ISREG(m.st_mode) and m.st_nlink == 1 and m.st_size <= 4 * 1024**2
            and not stat.S_IMODE(m.st_mode) & 0o7000, 'Unsafe desktop file')
    return {'sha256': gate.hash_file(path), 'mode': stat.S_IMODE(m.st_mode)}


def atomic_write(path, raw, mode, expected):
    require(state(path) == expected, 'Desktop destination drift; nothing overwritten')
    path.parent.mkdir(parents=True, exist_ok=True)
    safe(path.parent)
    fd, temp = tempfile.mkstemp(prefix='.weavelo-desktop-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as out:
            os.fchmod(out.fileno(), mode); out.write(raw); out.flush(); os.fsync(out.fileno())
        require(state(path) == expected, 'Desktop file changed before atomic replacement')
        os.replace(temp, path)
        sync(path.parent)
    finally:
        if os.path.exists(temp): os.unlink(temp)


def sync(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try: os.fsync(fd)
    finally: os.close(fd)


def exclusive_rename(source, destination):
    """Native no-replace rename; no check-then-replace fallback."""
    require(source.parent.stat().st_dev == destination.parent.stat().st_dev, 'Desktop transaction must use one filesystem')
    libc = ctypes.CDLL(None, use_errno=True)
    if sys.platform == 'darwin':
        fn = libc.renamex_np; fn.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        result = fn(os.fsencode(source), os.fsencode(destination), 0x4)
    elif sys.platform.startswith('linux') and hasattr(libc, 'renameat2'):
        fn = libc.renameat2; fn.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        result = fn(-100, os.fsencode(source), -100, os.fsencode(destination), 1)
    else: raise ValueError('Exclusive desktop rename unavailable; no fallback')
    if result: raise OSError(ctypes.get_errno(), 'Exclusive desktop rename failed', str(destination))
    sync(source.parent); sync(destination.parent)


def identity(path):
    safe(path)
    if not path.exists(): return None
    before = path.lstat(); value = state(path); after = path.lstat()
    require((before.st_dev, before.st_ino, before.st_mtime_ns, before.st_ctime_ns, before.st_size)
            == (after.st_dev, after.st_ino, after.st_mtime_ns, after.st_ctime_ns, after.st_size), 'Unstable desktop inode')
    return dict(value, device=after.st_dev, inode=after.st_ino)


class DesktopTransaction:
    def __init__(self, home, data_home=None):
        self.home = home
        self.data_home = data_home if data_home is not None else home / '.local/share'
        require(self.data_home.is_relative_to(home), 'User desktop data must remain under the authorized home')
        safe(self.data_home)
        self.work = home / '.local/state/weavelo-desktop-install'
        self.journal = self.work / 'journal.json'

    def targets(self):
        paths = {f'icon-{s}.png': self.data_home / f'icons/hicolor/{s}x{s}/apps/{APP_ID}.png' for s in SIZES}
        paths['tile.svg'] = self.data_home / f'icons/hicolor/scalable/apps/{APP_ID}.svg'
        paths['desktop'] = self.data_home / f'applications/{APP_ID}.desktop'
        return paths

    def payloads(self, assets, executable):
        require(executable.is_absolute() and not any(ord(c) < 32 or c in '\\"`$%' for c in str(executable)),
                'Desktop executable contains unsupported control/quoting characters')
        desktop = f'[Desktop Entry]\nType=Application\nName=Weavelo\nExec="{executable}"\nIcon={APP_ID}\nStartupWMClass={APP_ID}\nTerminal=false\nCategories=Network;RemoteAccess;\n'
        return {n: desktop.encode() if n == 'desktop' else gate.regular(assets / n).read_bytes() for n in self.targets()}

    def plan(self, assets, executable, provenance):
        safe(self.home); safe(self.work)
        require(not self.journal.exists(), 'An original desktop transaction exists; observe/recover it')
        payloads = self.payloads(assets, executable)
        require(gate.hash_file(executable) == gate.sha(provenance['binary_sha256']), 'Verified executable changed')
        entries = []
        for name, path in self.targets().items():
            before = state(path); desired = sha(payloads[name])
            require(before is None or before['sha256'] == desired,
                    'Existing new-identity desktop file differs; first install never replaces it')
            entries.append({'name':name, 'path':str(path), 'before':before,
                'action':'create' if before is None else 'preserve',
                'after':{'sha256':desired,'mode':0o644} if before is None else before})
        return {'schema':2,'kind':'weavelo-user-desktop-first-install','home':str(self.home),
                'executable':str(executable),'data_home':str(self.data_home),
                'executable_sha256':provenance['binary_sha256'],'provenance':provenance,'entries':entries,
                'security_settings_modified':False,'services_modified':False,'applied':False}

    @contextmanager
    def lock(self):
        safe(self.work); self.work.mkdir(parents=True,exist_ok=True,mode=0o700)
        require(stat.S_IMODE(self.work.stat().st_mode)==0o700,'Desktop transaction directory is not private')
        lock=self.work/'lock';safe(lock)
        fd=os.open(lock,os.O_CREAT|os.O_RDWR|os.O_NOFOLLOW,0o600)
        try:
            m=os.fstat(fd)
            require(m.st_uid==os.getuid() and m.st_nlink==1 and stat.S_ISREG(m.st_mode),'Unsafe desktop transaction lock')
            fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);yield
        finally:os.close(fd)

    def save(self, document):
        # Only the private, locked journal uses replace. Public assets never do.
        atomic_write(self.journal,json.dumps(document,sort_keys=True,indent=2).encode()+b'\n',0o600,state(self.journal))

    def validate(self, document, expected):
        plan=document['plan']
        require(document.get('schema')==2 and plan_hash(plan)==gate.sha(expected)==document['plan_sha256']
                and plan['schema']==2 and plan['kind']=='weavelo-user-desktop-first-install'
                and plan['home']==str(self.home),'Original desktop transaction differs; legacy journals cannot be reinterpreted')
        require([(e['name'],e['path']) for e in plan['entries']]==[(n,str(p)) for n,p in self.targets().items()]
                and plan['data_home']==str(self.data_home),'Desktop destinations differ')
        for key,record in document['owned'].items():
            i=int(key);require(str(i)==key and 0<=i<len(plan['entries']),'Invalid publication index')
            e=plan['entries'][i]
            require(e['action']=='create' and record['staging']==str(self.work/'payloads'/key)
                    and record['retired']==str(self.work/'retired'/key)
                    and record['identity']['sha256']==e['after']['sha256']
                    and record['identity']['mode']==e['after']['mode'],'Invalid publication ownership receipt')
        return plan

    def apply(self, assets, executable, provenance, expected):
        with self.lock():
            plan=self.plan(assets,executable,provenance)
            require(plan_hash(plan)==gate.sha(expected),'Reviewed desktop plan drift')
            payloads=self.payloads(assets,executable)
            for name in ['backup','payloads','retired']:
                directory=self.work/name;safe(directory);require(not directory.exists(),'Unknown original transaction files')
                directory.mkdir(mode=0o700)
            document={'schema':2,'plan':plan,'plan_sha256':expected,'phase':'preparing','owned':{}}
            self.save(document)
            try:
                for i,e in enumerate(plan['entries']):
                    path=Path(e['path']);require(state(path)==e['before'],'Destination changed before preparation')
                    if e['action']=='preserve':
                        raw=path.read_bytes();require(sha(raw)==e['before']['sha256'],'Unstable preserved preimage')
                        atomic_write(self.work/'backup'/str(i),raw,0o600,None)
                        continue
                    path.parent.mkdir(parents=True,exist_ok=True);safe(path.parent)
                    require(path.parent.stat().st_dev==self.work.stat().st_dev,'Desktop targets must share the private transaction filesystem')
                    stage=self.work/'payloads'/str(i)
                    require(sha(payloads[e['name']])==e['after']['sha256'],'Source asset drift')
                    atomic_write(stage,payloads[e['name']],0o644,None)
                    document['owned'][str(i)]={'staging':str(stage),'retired':str(self.work/'retired'/str(i)),
                                              'identity':identity(stage)}
                    self.save(document)  # Durable inode intent BEFORE publication.
                require(gate.hash_file(executable)==plan['executable_sha256'],'Executable drift before publication')
                document['phase']='installing';self.save(document)
                for i,e in enumerate(plan['entries']):
                    if e['action']=='preserve':continue
                    record=document['owned'][str(i)]
                    require(identity(Path(record['staging']))==record['identity'],'Owned staged inode changed')
                    exclusive_rename(Path(record['staging']),Path(e['path']))
                    require(identity(Path(e['path']))==record['identity'],'Published inode differs')
                for i,e in enumerate(plan['entries']):
                    actual=identity(Path(e['path']))
                    require((actual==document['owned'][str(i)]['identity'] if e['action']=='create' else state(Path(e['path']))==e['before']),
                            'Desktop publication verification failed')
                require(gate.hash_file(executable)==plan['executable_sha256'],'Executable drift after publication')
                document['phase']='completed';self.save(document)
                return {'applied':True,'plan_sha256':expected,'installed_files':len(plan['entries']),
                        'created_files':len(document['owned']),'journal':str(self.journal),
                        'services_modified':False,'system_icon_display':'not_evaluated'}
            except Exception:
                self.rollback(document,expected)
                raise

    def rollback(self, document, expected):
        plan=self.validate(document,expected);conflicts=[]
        for i,e in reversed(list(enumerate(plan['entries']))):
            # No ownership receipt, no mutation, even if bytes match the candidate.
            record=document['owned'].get(str(i))
            if record is None:continue
            path=Path(e['path']);retired=Path(record['retired']);wanted=record['identity']
            current=identity(path);saved=identity(retired)
            if saved is not None:
                if saved!=wanted:
                    if current is None:
                        exclusive_rename(retired,path)
                    conflicts.append(e['path']);continue
                if current is not None:conflicts.append(e['path'])
                continue
            if current is None:continue
            if current!=wanted:
                conflicts.append(e['path']);continue
            exclusive_rename(path,retired)  # Captures races without deleting any file.
            if identity(retired)!=wanted:
                if not path.exists():
                    try:exclusive_rename(retired,path)
                    except FileExistsError:pass
                conflicts.append(e['path'])
        document['external_changes_preserved']=conflicts
        document['phase']='recovery_blocked_external_changes' if conflicts else 'rolled_back'
        self.save(document)
        require(not conflicts,'Desktop file changed outside original transaction; external files preserved, inspect original journal')
        return {'rolled_back':True,'journal':str(self.journal),'retired_owned_assets_retained':True}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--data-home', type=Path, help='Explicit user-owned XDG_DATA_HOME below the authorized home')
    p.add_argument('--assets', type=Path)
    p.add_argument('--assets-manifest-sha256')
    p.add_argument('--app', type=Path)
    p.add_argument('--release-manifest', type=Path)
    p.add_argument('--manifest-sha256')
    p.add_argument('--commit')
    p.add_argument('--identity-sha256')
    p.add_argument('--apply', action='store_true')
    p.add_argument('--recover', action='store_true')
    p.add_argument('--rollback', action='store_true')
    p.add_argument('--plan-sha256')
    a = p.parse_args()
    require(sys.platform.startswith('linux') and os.getuid() != 0 and os.getuid() == os.geteuid(), 'Native ordinary-user Linux only')
    require(sum([a.apply, a.recover, a.rollback]) <= 1, 'Apply/recover/rollback are mutually exclusive')
    home = Path.home().resolve(strict=True)
    require(home == Path(pwd.getpwuid(os.getuid()).pw_dir).resolve(strict=True), 'HOME differs from current OS user')
    data_home = a.data_home or Path(os.environ.get('XDG_DATA_HOME', str(home / '.local/share')))
    transaction = DesktopTransaction(home, data_home)
    if a.recover or a.rollback:
        require(a.plan_sha256 is not None, 'Original reviewed plan hash required')
        with transaction.lock():
            require(state(transaction.journal)['mode'] == 0o600, 'Journal is not private')
            document = gate.read_json(transaction.journal)
            require((document['phase'] == 'completed') == a.rollback,
                    'Use explicit rollback for completed installation, recover for interruption')
            result = transaction.rollback(document, a.plan_sha256)
    else:
        require(all([a.assets, a.assets_manifest_sha256, a.app, a.release_manifest, a.manifest_sha256, a.commit, a.identity_sha256]),
                'Independent asset and compiled release expectations are required together')
        app = a.app.resolve(strict=True); executable = app / 'weavelo'; safe(app)
        provenance = gate.verify_install(app, a.release_manifest.resolve(strict=True), a.manifest_sha256,
                    a.commit, a.identity_sha256, executable, executable)
        require(provenance['build_identity']['configuration']['platform'] == 'linux', 'Native Linux release required')
        assets = a.assets.resolve(strict=True)
        source = gate.read_json(assets / 'SOURCE.json', a.assets_manifest_sha256)
        require(source['product'] == 'Weavelo' and source['application_id'] == APP_ID, 'Asset identity differs')
        for name in transaction.targets():
            if name != 'desktop': require(gate.hash_file(assets / name) == source['assets'][name], 'Final brand asset differs')
        provenance = {'binary_sha256': provenance['binary_sha256'],
                      'commit': a.commit, 'identity_sha256': a.identity_sha256,
                      'manifest_sha256': a.manifest_sha256, 'asset_manifest_sha256': a.assets_manifest_sha256}
        if a.apply:
            require(a.plan_sha256 is not None, 'Reviewed plan hash required')
            result = transaction.apply(assets, executable, provenance, a.plan_sha256)
        else:
            plan = transaction.plan(assets, executable, provenance)
            result = {'plan': plan, 'plan_sha256': plan_hash(plan)}
    print(json.dumps(result, indent=2))


if __name__ == '__main__': main()
