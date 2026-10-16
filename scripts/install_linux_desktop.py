#!/usr/bin/env python3
"""Install the verified Weavelo desktop entry and hicolor assets as one user transaction.

No root, service control, package manager, shared icon-cache rewrite or permissions repair.
Plans are read-only. Apply/recover require the exact reviewed plan hash.
"""
from __future__ import annotations
import argparse
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
        # Desktop Exec is not a shell; quote and escape its field-code/metacharacter syntax.
        require(executable.is_absolute() and not any(ord(c) < 32 or c in '\\"`$%' for c in str(executable)),
                'Desktop executable contains unsupported control/quoting characters')
        name = str(executable).replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$').replace('%', '%%')
        desktop = f'[Desktop Entry]\nType=Application\nName=Weavelo\nExec="{name}"\nIcon={APP_ID}\nStartupWMClass={APP_ID}\nTerminal=false\nCategories=Network;RemoteAccess;\n'
        return {n: desktop.encode() if n == 'desktop' else gate.regular(assets / n).read_bytes() for n in self.targets()}

    def plan(self, assets, executable, provenance):
        safe(self.home); safe(self.work)
        require(not self.journal.exists(), 'An original desktop transaction exists; observe/recover it')
        payloads = self.payloads(assets, executable)
        require(gate.hash_file(executable) == gate.sha(provenance['binary_sha256']), 'Verified executable changed')
        return {'schema': 1, 'kind': 'weavelo-user-desktop-install', 'home': str(self.home),
                'executable': str(executable), 'data_home': str(self.data_home),
                'executable_sha256': provenance['binary_sha256'], 'provenance': provenance,
                'entries': [{'name': name, 'path': str(path), 'before': state(path),
                             'after': {'sha256': sha(payloads[name]), 'mode': 0o644}}
                            for name, path in self.targets().items()],
                'security_settings_modified': False, 'new_file_mode': 0o644,
                'services_modified': False, 'applied': False}

    @contextmanager
    def lock(self):
        safe(self.work)
        self.work.mkdir(parents=True, exist_ok=True, mode=0o700)
        require(stat.S_IMODE(self.work.stat().st_mode) == 0o700, 'Desktop transaction directory is not private')
        lock = self.work / 'lock'; safe(lock)
        fd = os.open(lock, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        try:
            m = os.fstat(fd)
            require(m.st_uid == os.getuid() and m.st_nlink == 1 and stat.S_ISREG(m.st_mode), 'Unsafe desktop transaction lock')
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            yield
        finally: os.close(fd)

    def save(self, document):
        current = state(self.journal)
        atomic_write(self.journal, json.dumps(document, sort_keys=True, indent=2).encode() + b'\n', 0o600, current)

    def validate(self, document, expected):
        plan = document['plan']
        require(plan_hash(plan) == gate.sha(expected) == document['plan_sha256']
                and plan['schema'] == 1 and plan['kind'] == 'weavelo-user-desktop-install'
                and plan['home'] == str(self.home), 'Original desktop transaction hash/home differs')
        require([(e['name'], e['path']) for e in plan['entries']] == [(n, str(p)) for n, p in self.targets().items()],
                'Desktop transaction destinations differ')
        require(plan['data_home'] == str(self.data_home), 'Desktop data directory differs from original plan')
        return plan

    def apply(self, assets, executable, provenance, expected):
        with self.lock():
            plan = self.plan(assets, executable, provenance)
            require(plan_hash(plan) == gate.sha(expected), 'Reviewed desktop plan drift')
            payloads = self.payloads(assets, executable)
            backup = self.work / 'backup'; safe(backup)
            require(not backup.exists(), 'Unknown previous desktop backup; preserve it')
            backup.mkdir(mode=0o700)
            document = {'schema': 1, 'plan': plan, 'plan_sha256': expected, 'phase': 'backing_up'}
            self.save(document)
            try:
                for index, e in enumerate(plan['entries']):
                    path = Path(e['path'])
                    require(state(path) == e['before'], 'Desktop file changed before backup')
                    if e['before'] is not None:
                        raw = path.read_bytes(); require(sha(raw) == e['before']['sha256'], 'Unstable desktop preimage')
                        atomic_write(backup / str(index), raw, 0o600, None)
                require(gate.hash_file(executable) == plan['executable_sha256'], 'Executable drift before desktop publication')
                document['phase'] = 'installing'; self.save(document)
                for e in plan['entries']:
                    require(sha(payloads[e['name']]) == e['after']['sha256'], 'Source asset drift')
                    atomic_write(Path(e['path']), payloads[e['name']], 0o644, e['before'])
                require(all(state(Path(e['path'])) == e['after'] for e in plan['entries'])
                        and gate.hash_file(executable) == plan['executable_sha256'], 'Desktop install/executable verification failed')
                document['phase'] = 'completed'; self.save(document)
                return {'applied': True, 'plan_sha256': expected, 'installed_files': len(plan['entries']),
                        'journal': str(self.journal), 'services_modified': False, 'system_icon_display': 'not_evaluated'}
            except Exception:
                self.rollback(document, expected)
                raise

    def rollback(self, document, expected):
        plan = self.validate(document, expected)
        for index, e in reversed(list(enumerate(plan['entries']))):
            path = Path(e['path']); current = state(path)
            if current == e['before']: continue
            require(current == e['after'], 'Desktop file changed outside original transaction; no unsafe recovery')
            if e['before'] is None:
                path.unlink(); sync(path.parent)
            else:
                saved = self.work / 'backup' / str(index)
                require(state(saved) is not None and gate.hash_file(saved) == e['before']['sha256'], 'Desktop backup differs')
                atomic_write(path, saved.read_bytes(), e['before']['mode'], current)
        require(all(state(Path(e['path'])) == e['before'] for e in plan['entries']), 'Desktop rollback unverified')
        document['phase'] = 'rolled_back'; self.save(document)
        return {'rolled_back': True, 'journal': str(self.journal)}


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
