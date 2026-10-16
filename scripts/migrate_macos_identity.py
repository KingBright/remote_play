#!/usr/bin/env python3
"""One explicit RemotePlay -> Weavelo transaction; never a compatible upgrade.

Planning is read-only. Apply binds the complete reviewed plan and independent
release expectations. Recovery rolls back the original journal; never replay apply.
No TCC, system service, certificate, ACL or Keychain mutation.
"""
from __future__ import annotations
import argparse
from contextlib import contextmanager
from dataclasses import asdict
import hashlib
import fcntl
import json
import os
from pathlib import Path
import plistlib
import pwd
import re
import shutil
import stat
import subprocess
import sys

import build_identity_gate as gate
import install_macos_unified as upgrade
import installer_build_identity as release
from macos_release_guard import ReleaseRejected, load_policy, designated_requirement

OLD_LABEL = 'com.remoteplay.host'
NEW_LABEL = 'fun.hackerlife.weavelo.host'
RUNTIME = {'desktop-instance.lock', 'desktop-instance.addr', 'desktop-instance.renderer.json',
           'background.sock', 'background-service.enabled'}


def require(ok, message):
    if not ok: raise ReleaseRejected(message)


def digest(data): return hashlib.sha256(data).hexdigest()
def plan_hash(plan): return digest(gate.canonical(plan))


def safe_path(path: Path):
    require(path.is_absolute() and '..' not in path.parts, 'Noncanonical migration path')
    for item in [path, *path.parents]:
        require(not item.is_symlink(), 'Linked migration path')
        if item.exists():
            info = item.lstat()
            if item == path or item.is_relative_to(Path.home()):
                require(info.st_uid == os.getuid(), 'Migration path is not user-owned')


def files(path: Path, *, private=False, excluded=()):
    safe_path(path)
    if not path.exists(): return None
    require(path.is_dir(), 'Expected migration directory')
    require(not private or stat.S_IMODE(path.stat().st_mode) & 0o077 == 0,
            'Existing private directory has broad permissions; no permissions changed')
    records = []
    for entry in sorted(path.rglob('*')):
        relative = entry.relative_to(path).as_posix()
        if relative in excluded: continue
        safe_path(entry)
        if entry.is_dir(): continue
        info = entry.lstat()
        require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1,
                'Migration inventory contains linked/special material')
        require(not private or stat.S_IMODE(info.st_mode) & 0o077 == 0,
                'Existing private profile has broad permissions; no permissions changed')
        require(len(records) < 20000 and info.st_size <= 64 * 1024**2, 'Migration inventory exceeds bounds')
        records.append({'path': relative, 'sha256': gate.hash_file(entry), 'mode': stat.S_IMODE(info.st_mode)})
    return records


def preferences_bytes(path: Path) -> bytes:
    """Read legacy user settings; never use this for credentials/release state."""
    safe_path(path)
    gate.regular(path)
    before = path.lstat()
    def stable(info):
        return (info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_nlink,
                info.st_size, info.st_mtime_ns, info.st_ctime_ns)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as source:
        info = os.fstat(source.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
                and stat.S_IMODE(info.st_mode) in (0o600, 0o644)
                and info.st_size <= release.MAX_STATE and stable(info) == stable(before),
                'Legacy preferences must be a stable owned regular 0600/0644 file')
        raw = source.read(release.MAX_STATE + 1)
        require(len(raw) == info.st_size and stable(os.fstat(source.fileno())) == stable(info)
                and stable(path.lstat()) == stable(info), 'Legacy preferences changed while reading')
        require(gate.hash_file(path) == digest(raw) and stable(path.lstat()) == stable(info),
                'Legacy preferences changed after reading')
    return raw


def write_private(path, raw):
    safe_path(path)
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), 'wb') as output:
        output.write(raw); output.flush(); os.fsync(output.fileno())
    upgrade.fsync_directory(path.parent)


@contextmanager
def migration_locks(applications):
    """Old then new, nonblocking; retain both lock inodes."""
    path = applications / '.remoteplay-install.lock'
    safe_path(path)
    fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid()
                and info.st_nlink == 1 and info.st_ino == path.lstat().st_ino,
                'Unsafe legacy installer lock')
        try: fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ReleaseRejected('Legacy installer is active; no migration') from error
        require(info.st_ino == path.lstat().st_ino, 'Legacy lock changed after acquiring')
        with upgrade.installation_lock(applications): yield
    finally: os.close(fd)


class MacBackend:
    def __init__(self, home, expected_release, old_binary_sha256, deadline):
        self.home, self.expected, self.old_hash, self.deadline = home, expected_release, gate.sha(old_binary_sha256), deadline

    def state(self, label):
        result = subprocess.run(['/bin/launchctl', 'print', f'gui/{os.getuid()}/{label}'],
                                capture_output=True, text=True, timeout=self.deadline.timeout(10))
        if result.returncode:
            require('Could not find service' in result.stderr, 'Unknown launchd state; no migration')
            return {'loaded': False, 'running': False, 'pid': None}
        pid = re.search(r'^\s*pid = (\d+)\s*$', result.stdout, re.M)
        program = re.search(r'^\s*program = (.+)$', result.stdout, re.M)
        argv = re.search(r'\n\s*arguments = \{\n(.*?)\n\s*\}', result.stdout, re.S)
        return {'loaded': True, 'running': bool(re.search(r'^\s*state = running\s*$', result.stdout, re.M)),
                'pid': int(pid[1]) if pid else None, 'program': program[1].strip() if program else None,
                'arguments': [s.strip() for s in argv[1].strip().splitlines()] if argv else None}

    def assert_no_other_products(self, allowed_pid=None):
        for line in self.deadline.run(['/bin/ps', '-axo', 'pid=,comm=']).splitlines():
            row = line.strip().split(None, 1)
            if len(row) != 2: continue
            name = Path(row[1]).name
            historical = row[1].endswith(('RemotePlayHost.app/Contents/MacOS/host', 'RemotePlayClient.app/Contents/MacOS/client'))
            if (name in ('remote_play', 'weavelo') or historical) and int(row[0]) != allowed_pid:
                raise ReleaseRejected('Another old/new product runtime exists; stop it explicitly before migration')

    def no_system_services(self):
        for label in ['com.remoteplay.mesh', 'fun.hackerlife.weavelo.mesh']:
            result = subprocess.run(['/bin/launchctl', 'print', 'system/' + label], capture_output=True,
                                    text=True, timeout=self.deadline.timeout(10))
            require(result.returncode != 0 and 'Could not find service' in result.stderr,
                    'External system service loaded or unknown; it is never managed here')

    def old_identity(self, app):
        policy = load_policy(); exe = app / 'Contents/MacOS/remote_play'
        require(app.name == 'RemotePlay.app', 'Unexpected legacy product')
        files(app)
        info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
        require(info.get('CFBundleIdentifier') == 'com.remoteplay.unified'
                and info.get('CFBundleExecutable') == 'remote_play', 'Legacy bundle identity differs')
        requirement = f'identifier "com.remoteplay.unified" and certificate leaf = H"{policy.certificate_sha1}"'
        self.deadline.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(app)])
        self.deadline.run(['/usr/bin/codesign', '--verify', '--strict', '-R', '=' + requirement, str(app)])
        require(designated_requirement(self.deadline.run(['/usr/bin/codesign', '-d', '-r-', str(app)])) == requirement,
                'Legacy signing requirement differs')
        require(gate.hash_file(exe) == self.old_hash, 'Legacy binary differs from independent expectation')
        return {'binary_sha256': self.old_hash, 'bundle_id': 'com.remoteplay.unified',
                'certificate_sha1': policy.certificate_sha1, 'designated_requirement': requirement}

    def candidate_identity(self, app, contract):
        report = upgrade.verify_with_budget(app, load_policy(), self.deadline)
        release.verify_app_contract(app, contract, report)
        upgrade.product_capabilities(app / 'Contents/MacOS/weavelo', report['version'], self.deadline)
        return report

    def verify_owner(self, state, executable, binary_sha256):
        require(state.get('loaded') is True and state.get('running') is True
                and state.get('program') == str(executable)
                and state.get('arguments') == [str(executable), '--background-service'],
                'Service is not the exact canonical background owner')
        gate.verify_running_macos(state['pid'], executable, binary_sha256,
                                  Path(self.expected.observer), self.expected.observer_sha256)
        owners = []
        for line in self.deadline.run(['/bin/ps', '-axo', 'pid=,comm=']).splitlines():
            row = line.strip().split(None, 1)
            if len(row) == 2 and row[1] == str(executable): owners.append(int(row[0]))
        require(owners == [state['pid']], 'Duplicate/manual runtime; close it before migration')

    def verify_cleanup_owner(self, state, app, contract, plist, expected_agent_sha256):
        """Never waive native image proof for a reported PID."""
        executable = app / 'Contents/MacOS/weavelo'
        for _ in range(4):
            current = state
            if not current['loaded']: return current
            if current.get('pid') is not None or current.get('running') is not False:
                self.verify_owner(current, executable, contract['manifest']['binary_sha256'])
            else:
                require(current.get('program') == str(executable)
                        and current.get('arguments') == [str(executable), '--background-service'],
                        'Unstarted job differs from the exact new backend')
                require(gate.hash_file(plist) == gate.sha(expected_agent_sha256),
                        'Unstarted job plist changed; no bootout')
                agent = plistlib.loads(plist.read_bytes())
                require(agent.get('Label') == NEW_LABEL
                        and agent.get('ProgramArguments') == [str(executable), '--background-service'],
                        'Unstarted job label/arguments differ')
                self.candidate_identity(app, contract)
                for line in self.deadline.run(['/bin/ps', '-axo', 'pid=,comm=']).splitlines():
                    row = line.strip().split(None, 1)
                    require(not (len(row) == 2 and row[1] == str(executable)),
                            'An unreported new backend process exists; no bootout')
            observed = self.state(NEW_LABEL)
            if observed == current: return current
            state = observed  # A newly appeared PID re-enters native verification.
        raise ReleaseRejected('New job generation changed during cleanup verification')

    def assert_absent(self, label, executable, old_pid=None):
        for _ in range(40):
            state = self.state(label)
            owners = [line.strip().split(None, 1) for line in self.deadline.run(['/bin/ps', '-axo', 'pid=,comm=']).splitlines()]
            matching = [row for row in owners if len(row) == 2 and (row[1] == str(executable) or (old_pid and row[0] == str(old_pid)))]
            if not state['loaded'] and not matching: return
            self.deadline.sleep(.25)
        raise ReleaseRejected('Old/new owner has not fully exited; no second runtime is started')

    def stop(self, label):
        if self.state(label)['loaded']:
            self.deadline.run(['/bin/launchctl', 'bootout', f'gui/{os.getuid()}/{label}'])

    def start(self, plist):
        self.deadline.run(['/bin/launchctl', 'bootstrap', f'gui/{os.getuid()}', str(plist)])

    def health(self, app, contract, profile):
        state = upgrade.wait_running(f'gui/{os.getuid()}', app / 'Contents/MacOS/weavelo', None, deadline=self.deadline)
        return upgrade.verified_backend_identity(app, contract, state,
                    self.home / f'Library/LaunchAgents/{NEW_LABEL}.plist', profile, self.deadline)


class Migration:
    def __init__(self, home, backend, import_preferences=False):
        self.home, self.backend = home, backend
        self.import_preferences = import_preferences
        self.apps = home / 'Applications'
        self.old, self.new = self.apps / 'RemotePlay.app', self.apps / 'Weavelo.app'
        self.old_exe, self.new_exe = self.old / 'Contents/MacOS/remote_play', self.new / 'Contents/MacOS/weavelo'
        self.old_plist = home / f'Library/LaunchAgents/{OLD_LABEL}.plist'
        self.new_plist = home / f'Library/LaunchAgents/{NEW_LABEL}.plist'
        self.old_profile = home / 'Library/Application Support/RemotePlay/NativeMesh'
        self.new_profile = home / 'Library/Application Support/Weavelo/NativeMesh'
        self.old_preferences = self.old_profile.parent / 'preferences.json'
        self.new_preferences = self.new_profile.parent / 'preferences.json'
        self.work = self.apps / '.weavelo-identity-migration'
        self.journal = self.work / 'journal.json'

    def plan(self, candidate, contract):
        for p in [self.apps, self.old, self.new, self.old_plist, self.new_plist, self.old_profile, self.new_profile, self.work]: safe_path(p)
        require(not self.work.exists(), 'Migration transaction already exists; observe/recover it, never replay apply')
        require(self.apps.is_dir() and self.old.is_dir() and self.old_plist.is_file(), 'Existing legacy app/service is required')
        require(not self.new.exists() and not self.new_plist.exists() and not self.new_profile.exists(),
                'First migration requires absent new app/agent/profile; use ordinary upgrade for an existing Weavelo installation')
        require(not (self.new.parent / release.STATE_NAME).exists(), 'New installed-release state already exists')
        require(not self.new_preferences.exists() and not self.new_preferences.is_symlink(), 'New identity preferences already exist')
        preferences_hash = self.preferences_hash()
        if self.import_preferences and preferences_hash is not None:
            gate.json_bytes(preferences_bytes(self.old_preferences))
        self.backend.no_system_services()
        old_info = self.backend.old_identity(self.old)
        candidate_info = self.backend.candidate_identity(candidate, contract)
        state = self.backend.state(OLD_LABEL)
        raw = self.old_plist.read_bytes(); plist = plistlib.loads(raw)
        require(plist.get('Label') == OLD_LABEL
                and plist.get('ProgramArguments') == [str(self.old_exe), '--background-service']
                and plist.get('Program', str(self.old_exe)) == str(self.old_exe), 'Legacy agent does not match the old app/role')
        self.backend.verify_owner(state, self.old_exe, old_info['binary_sha256'])
        self.backend.assert_no_other_products(state['pid'])
        self.backend.assert_absent(NEW_LABEL, self.new_exe)
        return {'schema': 1, 'kind': 'weavelo-first-identity-migration', 'home': str(self.home),
                'old': old_info, 'old_files': files(self.old), 'candidate': candidate_info,
                'candidate_files': files(candidate), 'old_agent_sha256': digest(raw), 'old_service': state,
                'new_service': self.new_agent(),
                'old_profile_files': files(self.old_profile, private=True, excluded=RUNTIME),
                'old_release_state_sha256': self.old_release_state_hash(),
                'old_preferences_sha256': preferences_hash, 'import_preferences': self.import_preferences,
                'release_expectations': asdict(contract['expectations']),
                'new_pairing': 'fresh-authenticated-profile-no-credential-import',
                'backup': str(self.work / 'backup'), 'permissions_modified': False,
                'platform_consent': 'user-may-need-to-authorize-new-identity', 'applied': False}

    def new_agent(self):
        return {'Label': NEW_LABEL, 'ProgramArguments': [str(self.new_exe), '--background-service'],
                'RunAtLoad': True, 'KeepAlive': {'SuccessfulExit': False},
                'EnvironmentVariables': {'WEAVELO_DEVICE_GROUP_DIR': str(self.new_profile)}}

    def preferences_hash(self):
        safe_path(self.old_preferences)
        return digest(preferences_bytes(self.old_preferences)) if self.old_preferences.exists() else None

    def old_release_state_hash(self):
        path = self.apps / '.remoteplay-release-identity.json'
        safe_path(path)
        return digest(release.private_bytes(path)) if path.exists() else None

    def checkpoint(self, document, phase):
        document['phase'] = phase; upgrade.atomic_json(self.journal, document)

    def apply(self, candidate, contract, expected_plan):
        with migration_locks(self.apps):
            plan = self.plan(candidate, contract)
            require(plan_hash(plan) == gate.sha(expected_plan), 'Migration plan drift; no mutation')
            self.work.mkdir(mode=0o700)
            backup = self.work / 'backup'; backup.mkdir(mode=0o700)
            document = {'schema': 1, 'plan': plan, 'plan_sha256': expected_plan, 'phase': 'preparing'}
            self.checkpoint(document, 'preparing')
            try:
                stage = self.work / 'Weavelo.app'
                shutil.copytree(candidate, stage, symlinks=False)
                require(files(stage) == plan['candidate_files'], 'Staging differs from sealed candidate')
                self.backend.candidate_identity(stage, contract)
                write_private(backup / 'legacy-agent.plist', self.old_plist.read_bytes())
                require(self.old_release_state_hash() == plan['old_release_state_sha256'], 'Legacy release-state drift')
                if plan['old_release_state_sha256'] is not None:
                    legacy_state = release.private_bytes(self.apps / '.remoteplay-release-identity.json')
                    require(digest(legacy_state) == plan['old_release_state_sha256'], 'Legacy state changed while reading')
                    write_private(backup / 'legacy-release-state.json', legacy_state)
                require(self.preferences_hash() == plan['old_preferences_sha256'], 'Legacy preferences drift')
                if plan['old_preferences_sha256'] is not None:
                    preferences = preferences_bytes(self.old_preferences)
                    require(digest(preferences) == plan['old_preferences_sha256'], 'Preferences changed while reading')
                    write_private(backup / 'preferences.json', preferences)
                staged_profile = self.work / 'NativeMesh'
                staged_profile.mkdir(mode=0o700)
                upgrade.ensure_role_marker(staged_profile)
                profile_meta = staged_profile.stat()
                document['profile_inode'] = [profile_meta.st_dev, profile_meta.st_ino]
                new_agent = plistlib.dumps(plan['new_service'])
                new_state_bytes = release.state_bytes(contract)
                document['new_agent_sha256'] = digest(new_agent)
                document['new_state_sha256'] = digest(new_state_bytes)
                profile_backup = backup / 'profile'; profile_backup.mkdir(mode=0o700)
                for entry in plan['old_profile_files'] or []:
                    source = self.old_profile / entry['path']; destination = profile_backup / entry['path']
                    destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                    require(gate.hash_file(source) == entry['sha256'], 'Legacy profile drift before backup')
                    raw = source.read_bytes()
                    require(digest(raw) == entry['sha256'], 'Profile bytes changed while reading')
                    write_private(destination, raw)
                self.checkpoint(document, 'stop_old_requested')
                require(digest(self.old_plist.read_bytes()) == plan['old_agent_sha256']
                        and self.preferences_hash() == plan['old_preferences_sha256']
                        and self.old_release_state_hash() == plan['old_release_state_sha256'], 'Legacy agent/preferences/state changed before stopping')
                self.backend.verify_owner(self.backend.state(OLD_LABEL), self.old_exe, plan['old']['binary_sha256'])
                self.backend.stop(OLD_LABEL)
                self.backend.assert_absent(OLD_LABEL, self.old_exe, plan['old_service']['pid'])
                self.checkpoint(document, 'old_stopped')
                with upgrade.idle_profile(self.old_profile, self.backend.deadline):
                    require(files(self.old, excluded=()) == plan['old_files']
                            and digest(self.old_plist.read_bytes()) == plan['old_agent_sha256']
                            and files(self.old_profile, private=True, excluded=RUNTIME) == plan['old_profile_files'],
                            'Legacy payload/profile changed; rollback required')
                    self.checkpoint(document, 'move_old_requested')
                    upgrade.durable_rename(self.old, backup / 'RemotePlay.app')
                    upgrade.durable_rename(self.old_plist, backup / f'{OLD_LABEL}.plist')
                    self.checkpoint(document, 'install_new_requested')
                    upgrade.durable_rename(stage, self.new)
                    self.new_profile.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                    upgrade.durable_rename(staged_profile, self.new_profile)
                    if plan['import_preferences'] and plan['old_preferences_sha256'] is not None:
                        raw = (backup / 'preferences.json').read_bytes()
                        require(digest(raw) == plan['old_preferences_sha256'], 'Preferences backup differs')
                        write_private(self.new_preferences, raw)
                    write_private(self.new_plist, new_agent)
                    write_private(self.new.parent / release.STATE_NAME, new_state_bytes)
                    self.backend.candidate_identity(self.new, contract)
                    document['new_agent_sha256'] = gate.hash_file(self.new_plist)
                    document['new_state_sha256'] = gate.hash_file(self.new.parent / release.STATE_NAME)
                    self.checkpoint(document, 'start_new_requested')
                self.backend.assert_absent(OLD_LABEL, self.old_exe, plan['old_service']['pid'])
                self.backend.assert_no_other_products()
                self.backend.start(self.new_plist)
                proof = self.backend.health(self.new, contract, self.new_profile)
                self.backend.assert_absent(OLD_LABEL, self.old_exe)
                require(self.preferences_hash() == plan['old_preferences_sha256']
                        and self.old_release_state_hash() == plan['old_release_state_sha256'], 'Original retained data changed')
                self.checkpoint(document, 'completed')
                return {'applied': True, 'plan_sha256': expected_plan, 'journal': str(self.journal),
                        'backend': proof, 'permissions_modified': False, 'physical_acceptance': 'not_evaluated'}
            except Exception:
                self.rollback(document, contract)
                raise

    def rollback(self, document, contract):
        plan = document['plan']; backup = self.work / 'backup'
        require(document.get('schema') == 1 and plan.get('home') == str(self.home)
                and plan_hash(plan) == document.get('plan_sha256'), 'Invalid original migration journal')
        for p in [self.work, self.journal, backup, self.old, self.new, self.old_plist, self.new_plist, self.new_profile]: safe_path(p)
        self.backend.no_system_services()
        new_state = self.backend.state(NEW_LABEL)
        if new_state['loaded']:
            expected_agent = digest(plistlib.dumps(plan['new_service']))
            require(document.get('new_agent_sha256') == expected_agent, 'Original new-agent binding differs')
            new_state = self.backend.verify_cleanup_owner(new_state, self.new, contract, self.new_plist, expected_agent)
            if new_state['loaded']: self.backend.stop(NEW_LABEL)
        self.backend.assert_absent(NEW_LABEL, self.new_exe, new_state.get('pid'))
        self.checkpoint(document, 'rollback_new_stopped')
        failed = self.work / 'failed'; safe_path(failed); failed.mkdir(mode=0o700, exist_ok=True)
        require(stat.S_IMODE(failed.stat().st_mode) == 0o700, 'Failed-state directory is not private')
        if self.new.exists():
            require(files(self.new) == plan['candidate_files'], 'New app changed; preserve both apps for manual recovery')
            upgrade.durable_rename(self.new, failed / 'Weavelo.app')
        for source, destination, expected in [
            (self.new_plist, failed / 'new-agent.plist', document.get('new_agent_sha256')),
            (self.new.parent / release.STATE_NAME, failed / 'new-release-state.json', document.get('new_state_sha256'))]:
            if source.exists():
                require(expected is not None and gate.hash_file(source) == expected,
                        'New install metadata changed; no unsafe cleanup')
                upgrade.durable_rename(source, destination)
        if self.new_preferences.exists() or self.new_preferences.is_symlink():
            safe_path(self.new_preferences); release.private_bytes(self.new_preferences)
            upgrade.durable_rename(self.new_preferences, failed / 'preferences.json')
        if self.new_profile.exists():
            meta = self.new_profile.stat()
            require([meta.st_dev, meta.st_ino] == document.get('profile_inode'),
                    'New profile is not this transaction inode; preserve it for recovery')
            files(self.new_profile, private=True, excluded=RUNTIME)
            upgrade.durable_rename(self.new_profile, failed / 'NativeMesh')
        if (backup / 'RemotePlay.app').exists():
            require(not self.old.exists() and files(backup / 'RemotePlay.app') == plan['old_files'], 'Legacy backup/path differs')
            upgrade.durable_rename(backup / 'RemotePlay.app', self.old)
        legacy_plist = backup / f'{OLD_LABEL}.plist'
        if legacy_plist.exists():
            require(not self.old_plist.exists() and gate.hash_file(legacy_plist) == plan['old_agent_sha256'], 'Legacy agent backup differs')
            upgrade.durable_rename(legacy_plist, self.old_plist)
        require(files(self.old) == plan['old_files'] and gate.hash_file(self.old_plist) == plan['old_agent_sha256']
                and files(self.old_profile, private=True, excluded=RUNTIME) == plan['old_profile_files'],
                'Legacy recovery verification failed; backups retained')
        require(self.old_release_state_hash() == plan['old_release_state_sha256']
                and self.preferences_hash() == plan['old_preferences_sha256'], 'Original legacy release state/preferences changed; backups retained')
        self.backend.old_identity(self.old)
        self.checkpoint(document, 'rollback_old_start_requested')
        old_state = self.backend.state(OLD_LABEL)
        self.backend.assert_no_other_products(old_state.get('pid') if old_state['loaded'] else None)
        if not old_state['loaded']: self.backend.start(self.old_plist)
        for _ in range(40):
            state = self.backend.state(OLD_LABEL)
            if state.get('running'):
                self.backend.verify_owner(state, self.old_exe, plan['old']['binary_sha256'])
                self.checkpoint(document, 'rolled_back'); return {'rolled_back': True, 'journal': str(self.journal)}
            self.backend.deadline.sleep(.25)
        raise ReleaseRejected('Legacy service recovery not verified; observe original journal')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('archive', type=Path)
    for name in ['commit', 'identity-sha256', 'manifest-sha256', 'observer-sha256', 'old-executable-sha256']:
        p.add_argument('--' + name, required=True)
    p.add_argument('--observer', type=Path, required=True)
    p.add_argument('--apply', action='store_true')
    p.add_argument('--recover', action='store_true')
    p.add_argument('--rollback', action='store_true')
    p.add_argument('--plan-sha256')
    p.add_argument('--import-preferences', action='store_true', help='One-time local preferences import; never imports old pairing credentials')
    args = p.parse_args()
    require(sys.platform == 'darwin' and os.getuid() != 0 and os.getuid() == os.geteuid(), 'Native ordinary-user macOS only')
    require(sum([args.apply, args.recover, args.rollback]) <= 1, 'Apply/recover/rollback are mutually exclusive')
    expected = release.requested(args.commit, args.identity_sha256, args.manifest_sha256, args.observer, args.observer_sha256)
    deadline = upgrade.Deadline(300); home = Path.home().resolve(strict=True)
    require(home == Path(pwd.getpwuid(os.getuid()).pw_dir).resolve(strict=True), 'HOME differs from the current OS user')
    backend = MacBackend(home, expected, args.old_executable_sha256, deadline); migration = Migration(home, backend, args.import_preferences)
    with release.verified_identity_archive(args.archive.resolve(strict=True), load_policy(), expected,
                                           runner=deadline.run, check=deadline.check) as (candidate, _, contract):
        if args.recover or args.rollback:
            require(args.plan_sha256 is not None, 'Original reviewed plan hash is required for recovery/rollback')
            with migration_locks(migration.apps):
                upgrade.private_regular_file(migration.journal)
                document = gate.read_json(migration.journal)
                require(document['plan']['release_expectations'] == asdict(expected), 'Original recovery expectations differ')
                require(document['plan_sha256'] == gate.sha(args.plan_sha256), 'Original reviewed migration hash differs')
                require((document['phase'] == 'completed') == args.rollback,
                        'Use explicit rollback for a completed migration, recover for an interruption')
                result = migration.rollback(document, contract)
        elif args.apply:
            require(args.plan_sha256 is not None, 'Exact reviewed plan hash is required for apply')
            result = migration.apply(candidate, contract, args.plan_sha256)
        else:
            plan = migration.plan(candidate, contract); result = {'plan': plan, 'plan_sha256': plan_hash(plan)}
        print(json.dumps(result, indent=2))


if __name__ == '__main__': main()
