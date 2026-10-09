#!/usr/bin/env python3
"""Guarded in-place upgrade. Default is a read-only plan; --apply requires passing every gate.

Never resets privacy grants, imports keys, changes membership, or touches other services.
Unsigned/ad-hoc installations require explicit manual recovery, not an automatic migration.
"""
from __future__ import annotations
import argparse
from contextlib import contextmanager
import math
import stat
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import tempfile
import time
import uuid
from macos_release_guard import ReleaseRejected, check_upgrade, load_policy, run, sha256, verified_archive, verify_app

LABEL = 'com.remoteplay.host'
SYSTEM_MESH = 'system/com.remoteplay.mesh'


def service_state(domain: str, *, deadline=None) -> dict:
    result = subprocess.run(['/bin/launchctl', 'print', domain + '/' + LABEL], capture_output=True, text=True, timeout=deadline.timeout(15) if deadline else 15)
    if result.returncode:
        # Only the documented not-found response counts as absence, not an arbitrary error.
        if 'Could not find service' in result.stderr:
            return {'loaded': False, 'running': False, 'pid': None}
        raise ReleaseRejected('Cannot determine existing service state: ' + result.stderr[-1200:])
    pid = re.search(r'^\s*pid = (\d+)\s*$', result.stdout, re.M)
    program = re.search(r'^\s*program = (.+)$', result.stdout, re.M)
    running = bool(re.search(r'^\s*state = running\s*$', result.stdout, re.M))
    return {'loaded': True, 'running': running, 'pid': int(pid[1]) if pid else None,
            'program': program[1].strip() if program else None}



def system_mesh_state(*, deadline=None) -> dict:
    """Observe one external system service; never manage it or echo raw output."""
    base = {'domain': 'system', 'label': 'com.remoteplay.mesh',
            'management': 'not_managed_by_user_installer'}
    unknown = dict(base, status='unavailable', loaded=None, running=None, pid=None)
    try:
        result = subprocess.run(['/bin/launchctl', 'print', SYSTEM_MESH],
                                capture_output=True, text=True,
                                timeout=deadline.timeout(15) if deadline else 15)
    except subprocess.TimeoutExpired:
        return dict(unknown, reason='timeout')
    except OSError:
        return dict(unknown, reason='command_unavailable')
    if result.returncode:
        if 'Could not find service' in result.stderr:
            return dict(base, status='not_loaded', loaded=False, running=False, pid=None)
        return dict(unknown, reason='query_failed', exit_code=result.returncode)
    if not result.stdout.strip():
        return dict(unknown, reason='empty_response')
    pid = re.search(r'^\s*pid = (\d+)\s*$', result.stdout, re.M)
    program = re.search(r'^\s*program = (.+)$', result.stdout, re.M)
    return dict(base, status='observed', loaded=True,
                running=bool(re.search(r'^\s*state = running\s*$', result.stdout, re.M)),
                pid=int(pid[1]) if pid else None,
                program=program[1].strip() if program else None)


def system_mesh_blocker(observation: dict) -> str | None:
    if observation.get('status') == 'not_loaded' and observation.get('loaded') is False:
        return None
    if observation.get('status') == 'observed' and observation.get('loaded') is True:
        return 'system_mesh_loaded'
    return 'system_mesh_unknown'


def require_system_mesh_absent(deadline, observation=None) -> dict:
    current = observation if observation is not None else system_mesh_state(deadline=deadline)
    blocker = system_mesh_blocker(current)
    if blocker == 'system_mesh_loaded':
        raise ReleaseRejected(SYSTEM_MESH + ' is loaded; the user installer cannot stop or upgrade this system service')
    if blocker:
        raise ReleaseRejected('Cannot determine ' + SYSTEM_MESH + ' absence; refusing application mutation')
    return current

def preserve_hashes(home: Path, plist: Path, *, deadline=None) -> dict:
    paths = [plist] if plist.is_file() else []
    directory = home / 'Library/Application Support/RemotePlay/NativeMesh'
    # The single-instance lock and UDP activation endpoint belong to a process,
    # so a legitimate restart recreates them. Everything else remains protected.
    runtime_files = {'desktop-instance.addr', 'desktop-instance.lock', 'desktop-instance.renderer.json'}
    paths += [p for p in directory.glob('*') if p.is_file() and p.name not in runtime_files]
    # Only local comparison. Never print config contents, secret values, or invitations.
    return {str(p): sha256(p, check=deadline.check if deadline else None) for p in paths}


def validate_service(plist: Path, executable: Path, state: dict) -> None:
    if plist.exists():
        if plist.is_symlink():
            raise ReleaseRejected('Launch-agent file is a symlink')
        data = plistlib.loads(plist.read_bytes())
        if data.get('Label') != LABEL or data.get('ProgramArguments') != [str(executable)]:
            raise ReleaseRejected('Launch agent points to another path or unexpected arguments; reconcile manually')
        if 'Program' in data and data['Program'] != str(executable):
            raise ReleaseRejected('Conflicting launch-agent executable')
    if state['loaded'] and (not plist.is_file() or state.get('program') != str(executable)):
        raise ReleaseRejected('Loaded service does not match the canonical app/launch-agent file')


def wait_running(domain: str, executable: Path, old_pid: int | None, *, deadline=None) -> dict:
    for _ in range(20):
        state = service_state(domain, deadline=deadline)
        if state['running'] and state['pid'] and state['pid'] != old_pid and state.get('program') == str(executable):
            # Require a second observation to catch immediate restart loops.
            deadline.sleep(1) if deadline else time.sleep(1)
            second = service_state(domain, deadline=deadline)
            if second == state:
                return state
        deadline.sleep(0.5) if deadline else time.sleep(0.5)
    raise ReleaseRejected('Updated RemotePlay did not become a stable running service')


def replace_with_rollback(candidate: Path, target: Path, backup: Path, verify, stop, start, health) -> dict:
    """Small transaction, independently tested with injected service hooks."""
    verify(candidate)  # Never stop the old service before the candidate passes.
    moved_old = False; installed = False; stopped = False
    try:
        stop(); stopped = True
        target.rename(backup); moved_old = True
        candidate.rename(target); installed = True
        verify(target)
        start()
        return health()
    except Exception as error:
        rollback_error = None
        try:
            if stopped:
                stop()
            if installed:
                target.rename(candidate)
            if moved_old:
                backup.rename(target)
            if stopped:
                start()
        except Exception as failure:
            rollback_error = str(failure)
        if rollback_error:
            raise ReleaseRejected(f'Upgrade failed: {error}; rollback also failed: {rollback_error}. Backup retained at {backup}') from error
        raise ReleaseRejected(f'Upgrade failed and previous app was restored: {error}') from error


class Deadline:
    """Cooperative wall-clock budget; no watchdog kills the installer."""
    def __init__(self, seconds: float):
        if not math.isfinite(seconds) or seconds <= 0:
            raise ReleaseRejected('Installer deadline must be a finite positive number')
        self.end = time.monotonic() + seconds

    def timeout(self, limit: float) -> float:
        remaining = self.end - time.monotonic()
        if remaining <= 0:
            raise ReleaseRejected('Installer deadline expired')
        return min(limit, remaining)

    def check(self) -> None:
        self.timeout(float('inf'))

    def run(self, args: list[str], timeout: float = 45) -> str:
        # A successful return remains an acknowledgement even if its budget just
        # elapsed. The next boundary checks time before another mutation.
        return run(args, timeout=self.timeout(timeout))

    def sleep(self, seconds: float) -> None:
        time.sleep(self.timeout(seconds))
        self.check()


IDENTITY_KEYS = ('version', 'build', 'bundle_id', 'certificate_sha1',
                 'designated_requirement', 'executable_sha256', 'info_sha256')
TERMINAL_PHASES = {'completed', 'rolled_back'}
PHASES = TERMINAL_PHASES | {
    'prepared', 'stop_requested', 'service_stopped', 'move_old_requested',
    'old_moved', 'move_new_requested', 'new_installed', 'verify_new_requested',
    'new_verified', 'start_requested', 'started', 'health_requested',
    'rollback_stop_requested', 'rollback_candidate_move_requested',
    'rollback_old_restore_requested', 'rollback_start_requested',
    'recovery_required',
}
UNCERTAIN_PHASES = {'stop_requested', 'start_requested',
                    'rollback_stop_requested', 'rollback_start_requested'}


def identity(info: dict) -> dict:
    return {key: info[key] for key in IDENTITY_KEYS}


def fsync_directory(directory: Path) -> None:
    fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def private_regular_file(path: Path) -> None:
    info = path.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or info.st_nlink != 1 or stat.S_IMODE(info.st_mode) != 0o600):
        raise ReleaseRejected('Unsafe installer journal/receipt file')


def atomic_json(path: Path, document: dict) -> None:
    if path.exists() or path.is_symlink():
        private_regular_file(path)
    fd, temporary = tempfile.mkstemp(prefix='.' + path.name + '.', dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as output:
            json.dump(document, output, indent=2)
            output.write('\n'); output.flush(); os.fsync(output.fileno())
        os.replace(temporary, path)
        fsync_directory(path.parent)
    finally:
        if os.path.exists(temporary): os.unlink(temporary)


def write_journal(path: Path, document: dict) -> None:
    atomic_json(path, document)


def read_journal(path: Path) -> dict | None:
    if not path.exists() and not path.is_symlink(): return None
    private_regular_file(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        if os.fstat(fd).st_ino != path.lstat().st_ino:
            raise ReleaseRejected('Installer journal changed while opening')
        with os.fdopen(fd, 'r') as source:
            fd = -1
            text = source.read(65537)
        if len(text) > 65536: raise ValueError('oversized')
        document = json.loads(text)
        if (not isinstance(document, dict) or document.get('schema') != 1
                or document.get('phase') not in PHASES):
            raise ValueError('unknown schema or phase')
        return document
    except (ValueError, TypeError) as error:
        raise ReleaseRejected('Invalid installer journal; do not replay installation') from error
    finally:
        if fd >= 0: os.close(fd)


def checkpoint(path: Path, document: dict, phase: str, **fields) -> None:
    document.update(fields, phase=phase, updated_at=datetime.now(timezone.utc).isoformat())
    write_journal(path, document)


@contextmanager
def installation_lock(applications: Path):
    path = applications / '.remoteplay-install.lock'
    if path.is_symlink(): raise ReleaseRejected('Installer lock is a symlink')
    fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'r+') as lock:
        info = os.fstat(lock.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_nlink != 1 or info.st_ino != path.lstat().st_ino):
            raise ReleaseRejected('Installer lock path changed or is unsafe')
        try: fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ReleaseRejected('Another RemotePlay installer is active') from error
        if info.st_ino != path.lstat().st_ino:
            raise ReleaseRejected('Installer lock path changed after locking')
        # Keep the inode stable forever; the journal is a separate atomic file.
        yield


def canonical_paths() -> tuple[Path, Path, Path, str]:
    home = Path.home(); applications = home / 'Applications'
    if os.getuid() == 0 or os.geteuid() == 0 or applications.is_symlink() or not applications.is_dir():
        raise ReleaseRejected('Use the logged-in user and their ordinary Applications directory')
    validate_user_profile(home, os.getuid())
    target = applications / 'RemotePlay.app'
    plist = home / 'Library/LaunchAgents/com.remoteplay.host.plist'
    return home, target, plist, f'gui/{os.getuid()}'


def validate_user_profile(home: Path, uid: int) -> None:
    """Metadata-only gate. An installer never creates or repairs user credentials."""
    profile = home / 'Library/Application Support/RemotePlay/NativeMesh'
    if '..' in home.parts:
        raise ReleaseRejected('User home contains parent traversal; configuration was not changed')
    for path in (home, home / 'Applications', home / 'Library',
                 home / 'Library/Application Support', profile.parent, profile):
        try: info = path.lstat()
        except FileNotFoundError: continue
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != uid:
            raise ReleaseRejected(f'User profile path {path} must be an ordinary directory owned by UID {uid}; no ownership or permissions changed')
        if path == profile and stat.S_IMODE(info.st_mode) & 0o077:
            raise ReleaseRejected(f'User profile {path} is not private; permissions were not changed')
    for path in (profile / 'mesh.conf', profile / 'mesh.secret'):
        try: info = path.lstat()
        except FileNotFoundError: continue
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise ReleaseRejected(f'Protected profile file {path} must be an ordinary single-link file; configuration was not changed')
        if info.st_uid != uid:
            raise ReleaseRejected(f'Protected profile file {path} belongs to UID {info.st_uid}, expected UID {uid}; arrange explicit administrator ownership recovery; configuration was not changed')
        if stat.S_IMODE(info.st_mode) & 0o077:
            raise ReleaseRejected(f'Protected profile file {path} is not private; permissions were not changed')


def verify_with_budget(path: Path, policy, deadline: Deadline) -> dict:
    return verify_app(path, policy, runner=deadline.run, check=deadline.check)


def validate_processes(executable: Path, state: dict, deadline: Deadline) -> None:
    owners = []
    for line in deadline.run(['/bin/ps', '-axo', 'pid=,comm=']).splitlines():
        parts = line.strip().split(None, 1)
        if len(parts) == 2 and parts[1] == str(executable): owners.append(int(parts[0]))
    if owners and (not state['loaded'] or owners != [state['pid']]):
        raise ReleaseRejected('Close manually started/duplicate RemotePlay processes before upgrading')


def journal_paths(document: dict, target: Path) -> tuple[Path, Path]:
    applications = target.parent
    try:
        uuid.UUID(document['operation_id'])
        if document['target'] != str(target): raise ValueError('target')
        backup = Path(document['backup']); candidate = Path(document['staging'])
        tag = backup.parent.name
        if not re.fullmatch(r'\d{8}T\d{6}Z-[0-9a-f]{8}', tag): raise ValueError('tag')
        if backup != applications / '.remoteplay-backups' / tag / 'RemotePlay.app': raise ValueError('backup')
        if (candidate.name != 'RemotePlay.app' or candidate.parent.parent != applications
                or not candidate.parent.name.startswith('.remoteplay-update-')): raise ValueError('staging')
        for item in [applications / '.remoteplay-backups', backup.parent, candidate.parent,
                     target, backup, candidate]:
            if item.is_symlink(): raise ValueError('symlink')
        for name in ['previous', 'candidate']:
            if identity(document[name]) != document[name]: raise ValueError('identity')
        before = document['service_before']
        if not isinstance(before.get('loaded'), bool): raise ValueError('service')
        if before['loaded'] and before.get('program') != str(target / 'Contents/MacOS/remote_play'):
            raise ValueError('service path')
        return candidate, backup
    except (KeyError, TypeError, ValueError) as error:
        raise ReleaseRejected('Invalid installer journal paths or preconditions') from error


def durable_rename(source: Path, destination: Path) -> None:
    if destination.exists() or destination.is_symlink():
        raise ReleaseRejected('Transaction rename destination already exists')
    source.rename(destination)
    fsync_directory(source.parent)
    if source.parent != destination.parent: fsync_directory(destination.parent)


def stop_service(domain: str, plist: Path, executable: Path, deadline: Deadline) -> None:
    current = service_state(domain, deadline=deadline)
    validate_service(plist, executable, current)
    if current['loaded']: deadline.run(['/bin/launchctl', 'bootout', domain + '/' + LABEL])


def clean_staging(candidate: Path) -> None:
    # Paths and signed contents were checked under the installation lock.
    if candidate.parent.exists():
        if candidate.parent.is_symlink() or set(candidate.parent.iterdir()) - {candidate}:
            raise ReleaseRejected('Unexpected staging contents; retain for diagnosis')
        shutil.rmtree(candidate.parent)
        fsync_directory(candidate.parent.parent)


def rollback_locked(document: dict, journal: Path, target: Path, plist: Path,
                    domain: str, policy, deadline: Deadline) -> dict:
    candidate, backup = journal_paths(document, target)
    phase = document['phase']
    if phase in UNCERTAIN_PHASES or document.get('uncertain_external_phase'):
        raise ReleaseRejected('External service command outcome unknown; recovery requires diagnosis, not replay')
    require_system_mesh_absent(deadline)
    observed = {}
    for name, path in [('target', target), ('backup', backup), ('staging', candidate)]:
        observed[name] = identity(verify_with_budget(path, policy, deadline)) if path.exists() else None
    old, new = document['previous'], document['candidate']
    if phase == 'prepared' and observed != {'target': old, 'backup': None, 'staging': new}:
        raise ReleaseRejected('Prepared journal does not match filesystem; retain transaction')
    if observed['backup'] not in (None, old) or observed['staging'] not in (None, new):
        raise ReleaseRejected('Recovery app bytes do not match journal; no mutation performed')
    if observed['target'] not in (None, old, new):
        raise ReleaseRejected('Canonical app changed outside transaction; no mutation performed')
    if observed['target'] == old:
        if observed['backup'] is not None:
            raise ReleaseRejected('Ambiguous recovery layout; retain both apps')
    elif observed['backup'] != old:
        raise ReleaseRejected('Verified previous app is unavailable; retain transaction')
    if observed['target'] == new and observed['staging'] is not None:
        raise ReleaseRejected('Ambiguous candidate copies; retain transaction')
    if phase != 'prepared':
        # Read-only query first. Every external mutation gets a durable intent.
        current = service_state(domain, deadline=deadline)
        validate_service(plist, target / 'Contents/MacOS/remote_play', current)
        validate_processes(target / 'Contents/MacOS/remote_play', current, deadline)
        if current['loaded'] and observed['target'] != old:
            checkpoint(journal, document, 'rollback_stop_requested')
            deadline.run(['/bin/launchctl', 'bootout', domain + '/' + LABEL])
            checkpoint(journal, document, 'service_stopped')
    if observed['target'] == new:
        checkpoint(journal, document, 'rollback_candidate_move_requested')
        deadline.check(); durable_rename(target, candidate)
        checkpoint(journal, document, 'old_moved')
    if not target.exists():
        checkpoint(journal, document, 'rollback_old_restore_requested')
        deadline.check(); durable_rename(backup, target)
        checkpoint(journal, document, 'service_stopped')
    if identity(verify_with_budget(target, policy, deadline)) != old:
        raise ReleaseRejected('Restored app does not match pinned previous app')
    if phase != 'prepared' and document['service_before']['loaded']:
        current = service_state(domain, deadline=deadline)
        validate_service(plist, target / 'Contents/MacOS/remote_play', current)
        if not current['loaded']:
            checkpoint(journal, document, 'rollback_start_requested')
            deadline.run(['/bin/launchctl', 'bootstrap', domain, str(plist)])
            checkpoint(journal, document, 'started')
        wait_running(domain, target / 'Contents/MacOS/remote_play', None, deadline=deadline)
    receipt = {'action': 'recovered_previous', 'target': str(target), 'applied': False,
               'configuration_unchanged': None, 'permissions_modified': False,
               'operation_id': document['operation_id'], 'backup': str(backup),
               'capture_health': 'not_tested_by_installer'}
    clean_staging(candidate)
    atomic_json(backup.parent / 'receipt.json', receipt)
    checkpoint(journal, document, 'rolled_back')
    return receipt


def recovery_failure(journal: Path, document: dict, error: Exception) -> None:
    phase = document['phase']
    fields = {'error': str(error)[-2000:]}
    if phase in UNCERTAIN_PHASES: fields['uncertain_external_phase'] = phase
    checkpoint(journal, document, 'recovery_required', **fields)


def recover_interrupted(*, deadline_seconds: float = 120) -> dict:
    """Explicit conservative rollback, under the same inode lock; never forward replay."""
    deadline = Deadline(deadline_seconds); policy = load_policy()
    _, target, plist, domain = canonical_paths()
    journal = target.parent / '.remoteplay-install-journal.json'
    with installation_lock(target.parent):
        document = read_journal(journal)
        if document is None:
            return {'applied': False, 'action': 'no_recovery_required'}
        journal_paths(document, target)
        if document['phase'] in TERMINAL_PHASES:
            expected = document['candidate'] if document['phase'] == 'completed' else document['previous']
            if identity(verify_with_budget(target, policy, deadline)) != expected:
                raise ReleaseRejected('Terminal journal does not match canonical app')
            return {'applied': False, 'action': 'no_recovery_required', 'phase': document['phase']}
        try:
            return rollback_locked(document, journal, target, plist, domain, policy, deadline)
        except Exception as error:
            recovery_failure(journal, document, error)
            raise


def install(archive: Path, apply: bool = False, *, deadline_seconds: float = 300) -> dict:
    deadline = Deadline(deadline_seconds); policy = load_policy()
    home, target, plist, domain = canonical_paths()
    applications = target.parent; executable = target / 'Contents/MacOS/remote_play'
    journal = applications / '.remoteplay-install-journal.json'
    with verified_archive(archive, policy, runner=deadline.run, check=deadline.check) as (source, candidate_info):
        pending = read_journal(journal)
        if pending is not None: journal_paths(pending, target)
        if pending is not None and pending['phase'] not in TERMINAL_PHASES:
            raise ReleaseRejected('Installer journal requires explicit --recover; do not replay --apply')
        previous = verify_with_budget(target, policy, deadline)
        action = check_upgrade(previous, candidate_info)
        state = service_state(domain, deadline=deadline)
        validate_service(plist, executable, state)
        system = system_mesh_state(deadline=deadline)
        blocker = system_mesh_blocker(system) if action != 'already_installed' else None
        plan = {'action': action, 'target': str(target), 'previous': previous, 'candidate': candidate_info,
                'will_restart_only_remoteplay': state['loaded'] and action != 'already_installed' and not blocker, 'permissions_modified': False,
                'capture_health': 'not_tested_by_installer', 'applied': False,
                'system_mesh': system, 'apply_blockers': [blocker] if blocker else []}
        if not apply: return plan
        if action != 'already_installed': require_system_mesh_absent(deadline, system)
        with installation_lock(applications):
            pending = read_journal(journal)
            if pending is not None: journal_paths(pending, target)
            if pending is not None and pending['phase'] not in TERMINAL_PHASES:
                raise ReleaseRejected('Installer journal requires explicit --recover; do not replay --apply')
            if verify_with_budget(target, policy, deadline) != previous:
                raise ReleaseRejected('Installed app changed during preflight')
            current = service_state(domain, deadline=deadline)
            validate_service(plist, executable, current)
            if current != state: raise ReleaseRejected('Service changed during preflight')
            if action == 'already_installed': return plan
            require_system_mesh_absent(deadline)
            validate_processes(executable, current, deadline)
            validate_user_profile(home, os.getuid())
            preserved = preserve_hashes(home, plist, deadline=deadline)
            tag = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-' + uuid.uuid4().hex[:8]
            backup_root = applications / '.remoteplay-backups' / tag
            if backup_root.parent.is_symlink(): raise ReleaseRejected('Backup directory is a symlink')
            backup_root.mkdir(parents=True, mode=0o700)
            backup = backup_root / 'RemotePlay.app'
            staging_root = Path(tempfile.mkdtemp(prefix='.remoteplay-update-', dir=applications))
            candidate = staging_root / 'RemotePlay.app'
            def copy_file(source_path, destination_path):
                deadline.check(); result = shutil.copy2(source_path, destination_path); deadline.check(); return result
            try:
                shutil.copytree(source, candidate, copy_function=copy_file)
                if identity(verify_with_budget(candidate, policy, deadline)) != identity(candidate_info):
                    raise ReleaseRejected('Staged candidate changed')
                require_system_mesh_absent(deadline)
            except Exception:
                shutil.rmtree(staging_root)
                raise
            document = {'schema': 1, 'operation_id': str(uuid.uuid4()), 'target': str(target),
                        'staging': str(candidate), 'backup': str(backup),
                        'previous': identity(previous), 'candidate': identity(candidate_info),
                        'service_before': state, 'deadline_seconds': deadline_seconds,
                        'recovery_budget_seconds': 120}
            checkpoint(journal, document, 'prepared')
            try:
                checkpoint(journal, document, 'stop_requested')
                stop_service(domain, plist, executable, deadline)
                checkpoint(journal, document, 'service_stopped')
                checkpoint(journal, document, 'move_old_requested')
                deadline.check(); durable_rename(target, backup)
                checkpoint(journal, document, 'old_moved')
                checkpoint(journal, document, 'move_new_requested')
                deadline.check(); durable_rename(candidate, target)
                checkpoint(journal, document, 'new_installed')
                checkpoint(journal, document, 'verify_new_requested')
                if identity(verify_with_budget(target, policy, deadline)) != identity(candidate_info):
                    raise ReleaseRejected('Installed candidate changed')
                checkpoint(journal, document, 'new_verified')
                if state['loaded']:
                    checkpoint(journal, document, 'start_requested')
                    deadline.run(['/bin/launchctl', 'bootstrap', domain, str(plist)])
                    checkpoint(journal, document, 'started')
                checkpoint(journal, document, 'health_requested')
                final = wait_running(domain, executable, state['pid'], deadline=deadline) if state['loaded'] else {'running': False}
                if preserve_hashes(home, plist, deadline=deadline) != preserved:
                    raise ReleaseRejected('Network or launch configuration changed during upgrade')
                plan.update(applied=True, service=final, configuration_unchanged=True,
                            backup=str(backup), operation_id=document['operation_id'])
                atomic_json(backup_root / 'receipt.json', plan)
                checkpoint(journal, document, 'completed')
            except Exception as error:
                # Recovery has a separate, finite safety budget; elapsed apply
                # time must not disable rollback. Unknown external outcomes stop.
                try:
                    rollback_locked(document, journal, target, plist, domain, policy, Deadline(120))
                except Exception as recovery_error:
                    recovery_failure(journal, document, recovery_error)
                    raise ReleaseRejected(f'Upgrade failed: {error}; recovery required: {recovery_error}') from error
                raise ReleaseRejected(f'Upgrade failed and previous app was restored: {error}') from error
            try:
                clean_staging(candidate)
            except (OSError, ReleaseRejected) as cleanup_error:
                plan['staging_retained'] = True
                plan['cleanup_warning'] = str(cleanup_error)[-1000:]
                atomic_json(backup_root / 'receipt.json', plan)
            return plan


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path, nargs='?')
    parser.add_argument('--apply', action='store_true', help='Apply a compatible upgrade; default only checks')
    parser.add_argument('--recover', action='store_true', help='Explicitly restore the pinned previous app from an interrupted transaction')
    parser.add_argument('--deadline-seconds', type=float, default=None, help='Cooperative budget; defaults to 300 for apply/plan, 120 for recovery')
    args = parser.parse_args()
    if args.recover and (args.apply or args.archive): parser.error('--recover cannot be combined with an archive or --apply')
    if not args.recover and args.archive is None: parser.error('archive is required unless --recover is selected')
    try:
        if args.recover:
            result = recover_interrupted(deadline_seconds=120 if args.deadline_seconds is None else args.deadline_seconds)
        else:
            result = install(args.archive, args.apply, deadline_seconds=300 if args.deadline_seconds is None else args.deadline_seconds)
        print('MACOS_INSTALL_RECEIPT ' + json.dumps(result), flush=True)
    except Exception as error:
        import sys
        print(f'MACOS_INSTALL_REJECTED: {error}', file=sys.stderr); sys.exit(1)
