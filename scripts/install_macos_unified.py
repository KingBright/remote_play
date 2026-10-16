#!/usr/bin/env python3
"""Guarded in-place upgrade. Default is a read-only plan; --apply requires passing every gate.

Never resets privacy grants, imports keys, changes membership, or touches other services.
Unsigned/ad-hoc installations require explicit manual recovery, not an automatic migration.
"""
from __future__ import annotations
import argparse
from contextlib import contextmanager, nullcontext
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
import installer_scope
import installer_build_identity as release_identity
from background_lifecycle_plan import plan as background_plan, LifecyclePlanRejected
from macos_release_guard import ReleaseRejected, check_upgrade, load_policy, run, sha256, verified_archive, verify_app

LABEL = 'fun.hackerlife.weavelo.host'
SYSTEM_MESH = 'system/fun.hackerlife.weavelo.mesh'


def service_label():
    scope = installer_scope.current()
    return scope.label if scope else LABEL


def service_state(domain: str, *, deadline=None) -> dict:
    result = subprocess.run(['/bin/launchctl', 'print', domain + '/' + service_label()], capture_output=True, text=True, timeout=deadline.timeout(15) if deadline else 15)
    if result.returncode:
        # Only the documented not-found response counts as absence, not an arbitrary error.
        if 'Could not find service' in result.stderr:
            return {'loaded': False, 'running': False, 'pid': None}
        raise ReleaseRejected('Cannot determine existing service state: ' + result.stderr[-1200:])
    pid = re.search(r'^\s*pid = (\d+)\s*$', result.stdout, re.M)
    program = re.search(r'^\s*program = (.+)$', result.stdout, re.M)
    arguments = re.search(r'\n\s*arguments = \{\n(.*?)\n\s*\}', result.stdout, re.S)
    running = bool(re.search(r'^\s*state = running\s*$', result.stdout, re.M))
    return {'loaded': True, 'running': running, 'pid': int(pid[1]) if pid else None,
            'program': program[1].strip() if program else None,
            'arguments': [line.strip() for line in arguments[1].strip().splitlines()] if arguments else None}



def system_mesh_state(*, deadline=None) -> dict:
    """Observe one external system service; never manage it or echo raw output."""
    if installer_scope.current():
        return {'status': 'not_applicable', 'loaded': False, 'running': False, 'pid': None,
                'scope': 'independent_fixture_profile_and_loopback_ports', 'management': 'not_managed'}
    base = {'domain': 'system', 'label': 'fun.hackerlife.weavelo.mesh',
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
    if installer_scope.current() and observation.get('status') == 'not_applicable': return None
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

def preserve_hashes(home: Path, plist: Path, *, deadline=None, lifecycle_change=False) -> dict:
    paths = [plist] if plist.is_file() and not lifecycle_change else []
    directory = home / 'Library/Application Support/Weavelo/NativeMesh'
    # The single-instance lock and UDP activation endpoint belong to a process,
    # so a legitimate restart recreates them. Everything else remains protected.
    runtime_files = {'desktop-instance.addr', 'desktop-instance.lock', 'desktop-instance.renderer.json'}
    if lifecycle_change: runtime_files.add('background-service.enabled')
    paths += [p for p in directory.glob('*') if p.is_file() and p.name not in runtime_files]
    # Only local comparison. Never print config contents, secret values, or invitations.
    return {str(p): sha256(p, check=deadline.check if deadline else None) for p in paths}


def validate_service(plist: Path, executable: Path, state: dict) -> None:
    if plist.exists():
        if plist.is_symlink():
            raise ReleaseRejected('Launch-agent file is a symlink')
        data = plistlib.loads(plist.read_bytes())
        if data.get('Label') != service_label() or data.get('ProgramArguments') not in ([str(executable)], [str(executable), '--background-service']):
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
    raise ReleaseRejected('Updated Weavelo did not become a stable running service')


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
    'recovery_required', 'lifecycle_write_requested', 'lifecycle_written',
    'rollback_lifecycle_requested',
    'identity_write_requested', 'identity_written', 'rollback_identity_requested',
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
    path = applications / '.weavelo-install.lock'
    if path.is_symlink(): raise ReleaseRejected('Installer lock is a symlink')
    fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'r+') as lock:
        info = os.fstat(lock.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_nlink != 1 or info.st_ino != path.lstat().st_ino):
            raise ReleaseRejected('Installer lock path changed or is unsafe')
        try: fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ReleaseRejected('Another Weavelo installer is active') from error
        if info.st_ino != path.lstat().st_ino:
            raise ReleaseRejected('Installer lock path changed after locking')
        # Keep the inode stable forever; the journal is a separate atomic file.
        yield


def canonical_paths() -> tuple[Path, Path, Path, str]:
    scope = installer_scope.current()
    home = scope.home if scope else Path.home(); applications = home / 'Applications'
    if os.getuid() == 0 or os.geteuid() == 0 or applications.is_symlink() or not applications.is_dir():
        raise ReleaseRejected('Use the logged-in user and their ordinary Applications directory')
    validate_user_profile(home, os.getuid())
    target = applications / 'Weavelo.app'
    plist = home / ('Library/LaunchAgents/' + service_label() + '.plist')
    return home, target, plist, f'gui/{os.getuid()}'


def validate_user_profile(home: Path, uid: int) -> None:
    """Metadata-only gate. An installer never creates or repairs user credentials."""
    profile = home / 'Library/Application Support/Weavelo/NativeMesh'
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
        raise ReleaseRejected('Close manually started/duplicate Weavelo processes before upgrading')


def journal_paths(document: dict, target: Path) -> tuple[Path, Path]:
    applications = target.parent
    try:
        uuid.UUID(document['operation_id'])
        if document['target'] != str(target): raise ValueError('target')
        backup = Path(document['backup']); candidate = Path(document['staging'])
        tag = backup.parent.name
        if not re.fullmatch(r'\d{8}T\d{6}Z-[0-9a-f]{8}', tag): raise ValueError('tag')
        if backup != applications / '.weavelo-backups' / tag / 'Weavelo.app': raise ValueError('backup')
        if (candidate.name != 'Weavelo.app' or candidate.parent.parent != applications
                or not candidate.parent.name.startswith('.weavelo-update-')): raise ValueError('staging')
        for item in [applications / '.weavelo-backups', backup.parent, candidate.parent,
                     target, backup, candidate]:
            if item.is_symlink(): raise ValueError('symlink')
        for name in ['previous', 'candidate']:
            if identity(document[name]) != document[name]: raise ValueError('identity')
        before = document['service_before']
        if not isinstance(before.get('loaded'), bool): raise ValueError('service')
        if before['loaded'] and before.get('program') != str(target / 'Contents/MacOS/weavelo'):
            raise ValueError('service path')
        if document.get('service_label', LABEL) != service_label(): raise ValueError('service label')
        if 'lifecycle' in document: validate_lifecycle_journal(document, plist_path=canonical_paths()[2])
        if 'release_identity' in document: validate_release_journal(document, target)
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
    if current['loaded']: deadline.run(['/bin/launchctl', 'bootout', domain + '/' + service_label()])


def profile_directory(home: Path) -> Path:
    return home / 'Library/Application Support/Weavelo/NativeMesh'


def product_capabilities(executable: Path, version: str, deadline: Deadline) -> dict:
    # The caller must verify the app under the pinned signing policy first.
    before = sha256(executable, check=deadline.check)
    env = {k: v for k, v in os.environ.items() if not k.startswith('WEAVELO_')}
    result = subprocess.run([str(executable), '--product-info-json'], env=env,
                            capture_output=True, text=True, timeout=deadline.timeout(15))
    if result.returncode or len(result.stdout) > 16384:
        raise ReleaseRejected('Signed candidate did not return bounded product capabilities')
    try:
        info = json.loads(result.stdout)
        from verify_desktop_gui import validate
        validate(info, 'macos', version)
    except (ValueError, TypeError, RuntimeError) as error:
        raise ReleaseRejected('Candidate product capabilities are invalid') from error
    if sha256(executable, check=deadline.check) != before:
        raise ReleaseRejected('Candidate executable changed during role verification')
    if (info.get('separated_lifecycle') is not True
            or info.get('background_entry') != '--background-service'
            or info.get('frontend_entry') != '--gui'):
        raise ReleaseRejected('Candidate cannot preserve separate background/GUI lifecycle')
    return info


def lifecycle_migration(plist: Path, executable: Path, source: Path,
                        candidate_info: dict, home: Path, deadline: Deadline) -> dict:
    if not plist.is_file() or plist.is_symlink():
        raise ReleaseRejected('Background migration requires the existing canonical launch agent')
    raw = plist.read_bytes()
    plist_meta = plist.lstat()
    if (plist_meta.st_uid != os.getuid() or plist_meta.st_nlink != 1
            or stat.S_IMODE(plist_meta.st_mode) not in (0o600, 0o644) or len(raw) > 65536):
        raise ReleaseRejected('Launch-agent ownership/mode is unsafe; no permissions changed')
    data = plistlib.loads(raw)
    profile = profile_directory(home)
    if not profile.is_dir():
        raise ReleaseRejected('Background migration requires the existing private device profile')
    for name in ('background-service.enabled', 'background.sock'):
        path = profile / name
        if not path.exists() and not path.is_symlink(): continue
        meta = path.lstat()
        kind_ok = stat.S_ISREG(meta.st_mode) and meta.st_size == 0 if name.endswith('enabled') else stat.S_ISSOCK(meta.st_mode)
        if not kind_ok or meta.st_uid != os.getuid() or meta.st_nlink != 1 or stat.S_IMODE(meta.st_mode) != 0o600:
            raise ReleaseRejected('Existing lifecycle process state is unsafe; no repair attempted')
    if data.get('EnvironmentVariables', {}).get('WEAVELO_DEVICE_GROUP_DIR') != str(profile):
        raise ReleaseRejected('Launch-agent device profile is not canonical; reconcile explicitly')
    info = product_capabilities(source / 'Contents/MacOS/weavelo', candidate_info['version'], deadline)
    try: proposal = background_plan(data, str(executable), info, expected_label=service_label())
    except LifecyclePlanRejected as error: raise ReleaseRejected(str(error)) from error
    after = plistlib.dumps(proposal['after'], sort_keys=False)
    return {'before_bytes': raw, 'after_bytes': after, 'mode': stat.S_IMODE(plist_meta.st_mode), 'gid': plist_meta.st_gid,
            'profile': profile, 'marker_before': (profile / 'background-service.enabled').exists(),
            'socket_before': (profile / 'background.sock').exists()}


def atomic_plist(path: Path, data: bytes, expected_sha256: str, mode: int, gid: int) -> None:
    meta = path.lstat()
    if (not stat.S_ISREG(meta.st_mode) or meta.st_uid != os.getuid()
            or meta.st_gid != gid or meta.st_nlink != 1
            or stat.S_IMODE(meta.st_mode) != mode or sha256(path) != expected_sha256):
        raise ReleaseRejected('Launch agent changed outside migration; refusing replacement')
    fd, temporary = tempfile.mkstemp(prefix='.weavelo-agent-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            if os.fstat(output.fileno()).st_gid != gid:
                raise ReleaseRejected('Atomic replacement would change launch-agent group; no permissions changed')
            os.fchmod(output.fileno(), mode)
            output.write(data); output.flush(); os.fsync(output.fileno())
        if sha256(path) != expected_sha256: raise ReleaseRejected('Launch agent changed before atomic replacement')
        os.replace(temporary, path); fsync_directory(path.parent)
    finally:
        if os.path.exists(temporary): os.unlink(temporary)


def prepare_lifecycle_backup(migration: dict, backup_root: Path, plist: Path) -> dict:
    import hashlib
    backup = backup_root / 'launch-agent.before.plist'
    fd = os.open(backup, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as output:
        output.write(migration['before_bytes']); output.flush(); os.fsync(output.fileno())
    fsync_directory(backup_root)
    return {'plist': str(plist), 'backup_plist': str(backup), 'mode': migration['mode'], 'gid': migration['gid'],
            'before_sha256': hashlib.sha256(migration['before_bytes']).hexdigest(),
            'after_sha256': hashlib.sha256(migration['after_bytes']).hexdigest(),
            'profile': str(migration['profile']), 'marker_before': migration['marker_before'],
            'socket_before': migration['socket_before']}


def ensure_role_marker(profile: Path) -> None:
    path = profile / 'background-service.enabled'
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    except FileExistsError:
        meta = path.lstat()
        if (not stat.S_ISREG(meta.st_mode) or meta.st_uid != os.getuid() or meta.st_nlink != 1
                or meta.st_size != 0 or stat.S_IMODE(meta.st_mode) != 0o600):
            raise ReleaseRejected('Unsafe background launch-role marker')
        return
    try: os.fsync(fd)
    finally: os.close(fd)
    fsync_directory(profile)


def validate_lifecycle_journal(document: dict, *, plist_path: Path) -> None:
    value = document['lifecycle']
    expected = {'plist', 'backup_plist', 'mode', 'gid', 'before_sha256', 'after_sha256',
                'profile', 'marker_before', 'socket_before'}
    backup_root = Path(document['backup']).parent
    document_home = plist_path.parents[2]
    if (not isinstance(value, dict) or set(value) != expected
            or value['plist'] != str(plist_path)
            or value['backup_plist'] != str(backup_root / 'launch-agent.before.plist')
            or value['profile'] != str(profile_directory(document_home))
            or type(value['mode']) is not int or value['mode'] not in (0o600, 0o644)
            or type(value['gid']) is not int or value['gid'] < 0
            or any(not re.fullmatch('[0-9a-f]{64}', value[k]) for k in ('before_sha256', 'after_sha256'))
            or any(type(value[k]) is not bool for k in ('marker_before', 'socket_before'))):
        raise ReleaseRejected('Invalid lifecycle migration journal')
    backup = Path(value['backup_plist']); private_regular_file(backup)
    if sha256(backup) != value['before_sha256']:
        raise ReleaseRejected('Original launch-agent backup changed')


@contextmanager
def idle_profile(profile: Path, deadline: Deadline):
    """Hold the existing process lock while moving app/plist/role state."""
    path = profile / 'desktop-instance.lock'
    if not profile.exists():
        yield; return
    fd = os.open(path, os.O_RDONLY | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        meta = os.fstat(fd)
        if (not stat.S_ISREG(meta.st_mode) or meta.st_uid != os.getuid()
                or meta.st_nlink != 1 or meta.st_ino != path.lstat().st_ino):
            raise ReleaseRejected('Unsafe profile runtime lock')
        for attempt in range(30):
            try: fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB); break
            except BlockingIOError:
                if attempt == 29: raise ReleaseRejected('Old runtime still owns the profile; no second host started')
                deadline.sleep(.1)
        yield
    finally: os.close(fd)


def restore_lifecycle(document: dict, plist: Path, deadline: Deadline) -> None:
    if 'lifecycle' not in document: return
    validate_lifecycle_journal(document, plist_path=plist)
    value = document['lifecycle']; current = sha256(plist)
    if current not in (value['before_sha256'], value['after_sha256']):
        raise ReleaseRejected('Launch agent was concurrently edited; retain app and plist backups')
    if current != value['before_sha256']:
        atomic_plist(plist, Path(value['backup_plist']).read_bytes(), current, value['mode'], value['gid'])
    profile = Path(value['profile'])
    for name, existed in [('background-service.enabled', value['marker_before']), ('background.sock', value['socket_before'])]:
        path = profile / name
        if existed or (not path.exists() and not path.is_symlink()): continue
        meta = path.lstat()
        kind_ok = stat.S_ISREG(meta.st_mode) and meta.st_size == 0 if name.endswith('enabled') else stat.S_ISSOCK(meta.st_mode)
        if not kind_ok or meta.st_uid != os.getuid() or meta.st_nlink != 1 or stat.S_IMODE(meta.st_mode) != 0o600:
            raise ReleaseRejected('New lifecycle process state changed; no unsafe cleanup performed')
        deadline.check(); path.unlink(); fsync_directory(profile)


def background_health(executable: Path, profile: Path, expected_pid: int, expected_sha256: str, deadline: Deadline) -> dict:
    env = {k: v for k, v in os.environ.items() if not k.startswith('WEAVELO_')}
    env['WEAVELO_DEVICE_GROUP_DIR'] = str(profile)
    for attempt in range(20):
        result = subprocess.run([str(executable), '--background-status-json'], env=env,
                                capture_output=True, text=True, timeout=deadline.timeout(10))
        try: info = json.loads(result.stdout) if result.returncode == 0 and len(result.stdout) <= 16384 else {}
        except ValueError: info = {}
        if (info.get('role') == 'background-service' and info.get('pid') == expected_pid
                and info.get('executable_sha256') == expected_sha256 and info.get('profile') == str(profile)):
            return {'background_role_verified': True, 'pid': expected_pid,
                    'executable_sha256': expected_sha256, 'gui_started_by_service': False}
        deadline.sleep(.25)
    raise ReleaseRejected('Updated process did not expose the expected background service identity')


def release_state_path(target: Path) -> Path:
    return target.parent / release_identity.STATE_NAME


@contextmanager
def installation_archive(archive, policy, expectations, deadline):
    if expectations is None:
        with verified_archive(archive, policy, runner=deadline.run, check=deadline.check) as (app, report):
            yield app, report, None
    else:
        with release_identity.verified_identity_archive(archive, policy, expectations,
                runner=deadline.run, check=deadline.check) as result: yield result


def prepare_release_backup(value: dict, target: Path, backup_root: Path) -> dict:
    path = release_state_path(target)
    before = release_identity.private_bytes(path) if path.exists() or path.is_symlink() else None
    after = release_identity.state_bytes(value)
    if before is not None: release_identity.write_new(backup_root / 'release-identity.before.json', before)
    release_identity.write_new(backup_root / 'release-identity.after.json', after)
    fsync_directory(backup_root)
    return {'state_path': str(path), 'before_sha256': release_identity.gate.digest(before) if before is not None else None,
            'after_sha256': release_identity.gate.digest(after)}


def validate_release_journal(document: dict, target: Path) -> None:
    value = document['release_identity']; root = Path(document['backup']).parent
    if (not isinstance(value, dict) or set(value) != {'state_path', 'before_sha256', 'after_sha256'}
            or value['state_path'] != str(release_state_path(target))):
        raise ReleaseRejected('Invalid release-identity journal')
    release_identity.gate.sha(value['after_sha256'])
    for name, expected in [('before', value['before_sha256']), ('after', value['after_sha256'])]:
        path = root / ('release-identity.' + name + '.json')
        if expected is None:
            if path.exists() or path.is_symlink(): raise ReleaseRejected('Unexpected prior release-identity backup')
            continue
        release_identity.gate.sha(expected)
        raw = release_identity.private_bytes(path)
        if release_identity.gate.digest(raw) != expected: raise ReleaseRejected('Release-identity backup changed')
        release_identity.parse_state(raw)
    path = release_state_path(target)
    current = release_identity.gate.digest(release_identity.private_bytes(path)) if path.exists() or path.is_symlink() else None
    if current not in (value['before_sha256'], value['after_sha256']):
        raise ReleaseRejected('Release identity was concurrently edited; retain the whole rollback tuple')
    if (document['phase'] == 'completed' and current != value['after_sha256']) or (
            document['phase'] == 'rolled_back' and current != value['before_sha256']):
        raise ReleaseRejected('Terminal journal does not match canonical release-identity state')


def restore_release_identity(document: dict, target: Path) -> None:
    if 'release_identity' not in document: return
    validate_release_journal(document, target)
    value = document['release_identity']; path = release_state_path(target)
    current = release_identity.gate.digest(release_identity.private_bytes(path)) if path.exists() else None
    if current == value['before_sha256']: return
    before = (Path(document['backup']).parent / 'release-identity.before.json')
    raw = release_identity.private_bytes(before) if value['before_sha256'] is not None else None
    release_identity.replace_state(path, raw, current)


def verified_backend_identity(target: Path, value: dict, state: dict, plist: Path, profile: Path, deadline):
    installed = release_identity.verify_app_contract(target, value, verify_with_budget(target, load_policy(), deadline))
    authenticated = background_health(target / 'Contents/MacOS/weavelo', profile, state['pid'],
                                       value['manifest']['binary_sha256'], deadline)
    observed = release_identity.verify_backend(target, value, state, plistlib.loads(plist.read_bytes()),
                                               service_label(), authenticated)
    after = service_state(f'gui/{os.getuid()}', deadline=deadline)
    if any(after.get(key) != state.get(key) for key in ('pid', 'program', 'arguments', 'running', 'loaded')):
        raise ReleaseRejected('Backend generation changed during native image verification')
    return dict(installed, **observed)


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
        validate_service(plist, target / 'Contents/MacOS/weavelo', current)
        validate_processes(target / 'Contents/MacOS/weavelo', current, deadline)
        if current['loaded'] and (observed['target'] != old or 'lifecycle' in document or 'release_identity' in document):
            checkpoint(journal, document, 'rollback_stop_requested')
            deadline.run(['/bin/launchctl', 'bootout', domain + '/' + service_label()])
            checkpoint(journal, document, 'service_stopped')
    profile_guard = idle_profile(profile_directory(plist.parents[2]), deadline) if phase != 'prepared' else nullcontext()
    with profile_guard:
        if 'lifecycle' in document and phase != 'prepared':
            checkpoint(journal, document, 'rollback_lifecycle_requested')
            restore_lifecycle(document, plist, deadline)
        if 'release_identity' in document and phase != 'prepared':
            checkpoint(journal, document, 'rollback_identity_requested')
            restore_release_identity(document, target)
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
        previous_release = release_identity.read_state(release_state_path(target)) if 'release_identity' in document else None
        if previous_release is not None:
            release_identity.verify_app_contract(target, previous_release, verify_with_budget(target, policy, deadline))
    if phase != 'prepared' and document['service_before']['loaded']:
        current = service_state(domain, deadline=deadline)
        validate_service(plist, target / 'Contents/MacOS/weavelo', current)
        if not current['loaded']:
            checkpoint(journal, document, 'rollback_start_requested')
            deadline.run(['/bin/launchctl', 'bootstrap', domain, str(plist)])
            checkpoint(journal, document, 'started')
        restored = wait_running(domain, target / 'Contents/MacOS/weavelo', None, deadline=deadline)
        if previous_release is not None:
            verified_backend_identity(target, previous_release, restored, plist, profile_directory(plist.parents[2]), deadline)
    receipt = {'action': 'recovered_previous', 'target': str(target), 'applied': False,
               'configuration_unchanged': None, 'permissions_modified': False,
               'operation_id': document['operation_id'], 'backup': str(backup),
               'capture_health': 'not_tested_by_installer'}
    if 'release_identity' in document:
        receipt.update(previous_release_state_restored=True,
                       previous_build_identity_verified=previous_release is not None)
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
    journal = target.parent / '.weavelo-install-journal.json'
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


def install(archive: Path, apply: bool = False, *, deadline_seconds: float = 300,
            separate_background: bool = False, expected_release=None) -> dict:
    deadline = Deadline(deadline_seconds); policy = load_policy()
    home, target, plist, domain = canonical_paths()
    applications = target.parent; executable = target / 'Contents/MacOS/weavelo'
    journal = applications / '.weavelo-install-journal.json'
    state_path = release_state_path(target)
    release_identity.require_context(expected_release, state_path, separate_background=separate_background,
                                      isolated_fixture=installer_scope.current() is not None, apply=apply)
    before_release_bytes = release_identity.private_bytes(state_path) if state_path.exists() or state_path.is_symlink() else None
    previous_release = release_identity.parse_state(before_release_bytes) if before_release_bytes is not None else None
    with installation_archive(archive, policy, expected_release, deadline) as (source, candidate_info, candidate_release):
        pending = read_journal(journal)
        if pending is not None: journal_paths(pending, target)
        if pending is not None and pending['phase'] not in TERMINAL_PHASES:
            raise ReleaseRejected('Installer journal requires explicit --recover; do not replay --apply')
        previous = verify_with_budget(target, policy, deadline)
        if previous_release is not None: release_identity.verify_app_contract(target, previous_release, previous)
        action = check_upgrade(previous, candidate_info)
        state = service_state(domain, deadline=deadline)
        validate_service(plist, executable, state)
        migration = lifecycle_migration(plist, executable, source, candidate_info, home, deadline) if separate_background else None
        service_document = plistlib.loads(plist.read_bytes()) if plist.is_file() else {}
        background_mode = service_document.get('ProgramArguments') == [str(executable), '--background-service']
        if candidate_release is not None and not (migration is not None or background_mode):
            raise ReleaseRejected('Identity-bound installation requires an explicit backend migration or the existing backend role')
        if background_mode and migration is None:
            product_capabilities(source / 'Contents/MacOS/weavelo', candidate_info['version'], deadline)
        if action == 'already_installed' and migration is not None and migration['before_bytes'] != migration['after_bytes']:
            action = 'migrate_background_lifecycle'
        if candidate_release is not None and previous['build'] == candidate_info['build']:
            if action != 'already_installed' or before_release_bytes != release_identity.state_bytes(candidate_release):
                raise ReleaseRejected('Assign a new signed build for a release-identity or launch-role migration')
        system = system_mesh_state(deadline=deadline)
        blocker = system_mesh_blocker(system) if action != 'already_installed' else None
        plan = {'action': action, 'target': str(target), 'previous': previous, 'candidate': candidate_info,
                'will_restart_only_weavelo': state['loaded'] and action != 'already_installed' and not blocker, 'permissions_modified': False,
                'capture_health': 'not_tested_by_installer', 'applied': False,
                'system_mesh': system, 'apply_blockers': [blocker] if blocker else [],
                'lifecycle_migration_requested': separate_background,
                'service_role_after': 'background-service' if migration or background_mode else 'preserved',
                'isolated_fixture': installer_scope.current() is not None}
        plan.update(build_identity_preflight_verified=candidate_release is not None,
                    backend_running_image_verified=False,
                    legacy_signature_only=candidate_release is None,
                    release_authorized=False)
        if candidate_release is not None and not state['loaded']:
            plan['apply_blockers'].append('Existing user service is unloaded; actual backend identity cannot be verified without an explicit service start')
        if not apply: return plan
        if candidate_release is not None and not state['loaded']:
            raise ReleaseRejected('Identity-bound apply requires the existing loaded backend service; no implicit service start')
        if action != 'already_installed': require_system_mesh_absent(deadline, system)
        with installation_lock(applications):
            pending = read_journal(journal)
            if pending is not None: journal_paths(pending, target)
            if pending is not None and pending['phase'] not in TERMINAL_PHASES:
                raise ReleaseRejected('Installer journal requires explicit --recover; do not replay --apply')
            if verify_with_budget(target, policy, deadline) != previous:
                raise ReleaseRejected('Installed app changed during preflight')
            now_release_bytes = release_identity.private_bytes(state_path) if state_path.exists() or state_path.is_symlink() else None
            if now_release_bytes != before_release_bytes: raise ReleaseRejected('Installed release state changed during preflight')
            current = service_state(domain, deadline=deadline)
            validate_service(plist, executable, current)
            if current != state: raise ReleaseRejected('Service changed during preflight')
            if migration is not None:
                import hashlib
                if sha256(plist) != hashlib.sha256(migration['before_bytes']).hexdigest():
                    raise ReleaseRejected('Launch agent changed during migration preflight')
            if action == 'already_installed':
                if candidate_release is not None and current['loaded']:
                    plan.update(verified_backend_identity(target, candidate_release, current, plist, profile_directory(home), deadline))
                    plan['backend_running_image_verified'] = True
                return plan
            require_system_mesh_absent(deadline)
            validate_processes(executable, current, deadline)
            validate_user_profile(home, os.getuid())
            preserved = preserve_hashes(home, plist, deadline=deadline, lifecycle_change=migration is not None)
            tag = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-' + uuid.uuid4().hex[:8]
            backup_root = applications / '.weavelo-backups' / tag
            if backup_root.parent.is_symlink(): raise ReleaseRejected('Backup directory is a symlink')
            backup_root.mkdir(parents=True, mode=0o700)
            backup = backup_root / 'Weavelo.app'
            lifecycle = prepare_lifecycle_backup(migration, backup_root, plist) if migration is not None else None
            release_backup = prepare_release_backup(candidate_release, target, backup_root) if candidate_release is not None else None
            staging_root = Path(tempfile.mkdtemp(prefix='.weavelo-update-', dir=applications))
            candidate = staging_root / 'Weavelo.app'
            def copy_file(source_path, destination_path):
                deadline.check(); result = shutil.copy2(source_path, destination_path); deadline.check(); return result
            try:
                shutil.copytree(source, candidate, copy_function=copy_file)
                if identity(verify_with_budget(candidate, policy, deadline)) != identity(candidate_info):
                    raise ReleaseRejected('Staged candidate changed')
                if candidate_release is not None:
                    release_identity.verify_app_contract(candidate.resolve(), candidate_release, candidate_info)
                require_system_mesh_absent(deadline)
            except Exception:
                shutil.rmtree(staging_root)
                raise
            document = {'schema': 1, 'operation_id': str(uuid.uuid4()), 'target': str(target),
                        'staging': str(candidate), 'backup': str(backup),
                        'previous': identity(previous), 'candidate': identity(candidate_info),
                        'service_before': state, 'service_label': service_label(), 'deadline_seconds': deadline_seconds,
                        'recovery_budget_seconds': 120}
            if lifecycle is not None: document['lifecycle'] = lifecycle
            if release_backup is not None: document['release_identity'] = release_backup
            checkpoint(journal, document, 'prepared')
            try:
                checkpoint(journal, document, 'stop_requested')
                stop_service(domain, plist, executable, deadline)
                checkpoint(journal, document, 'service_stopped')
                with idle_profile(profile_directory(home), deadline):
                    checkpoint(journal, document, 'move_old_requested')
                    deadline.check(); durable_rename(target, backup)
                    checkpoint(journal, document, 'old_moved')
                    checkpoint(journal, document, 'move_new_requested')
                    deadline.check(); durable_rename(candidate, target)
                    checkpoint(journal, document, 'new_installed')
                    checkpoint(journal, document, 'verify_new_requested')
                    if identity(verify_with_budget(target, policy, deadline)) != identity(candidate_info):
                        raise ReleaseRejected('Installed candidate changed')
                    if candidate_release is not None: release_identity.verify_app_contract(target, candidate_release, candidate_info)
                    checkpoint(journal, document, 'new_verified')
                    if migration is not None:
                        checkpoint(journal, document, 'lifecycle_write_requested')
                        atomic_plist(plist, migration['after_bytes'], lifecycle['before_sha256'], lifecycle['mode'], lifecycle['gid'])
                        ensure_role_marker(migration['profile'])
                        checkpoint(journal, document, 'lifecycle_written')
                    if release_backup is not None:
                        checkpoint(journal, document, 'identity_write_requested')
                        release_identity.replace_state(state_path, release_identity.private_bytes(backup_root / 'release-identity.after.json'),
                                                        release_backup['before_sha256'])
                        checkpoint(journal, document, 'identity_written')
                if state['loaded']:
                    checkpoint(journal, document, 'start_requested')
                    deadline.run(['/bin/launchctl', 'bootstrap', domain, str(plist)])
                    checkpoint(journal, document, 'started')
                checkpoint(journal, document, 'health_requested')
                final = wait_running(domain, executable, state['pid'], deadline=deadline) if state['loaded'] else {'running': False}
                if state['loaded'] and (migration is not None or background_mode):
                    final.update(background_health(executable, profile_directory(home), final['pid'], candidate_info['executable_sha256'], deadline))
                if candidate_release is not None and state['loaded']:
                    final.update(verified_backend_identity(target, candidate_release, final, plist, profile_directory(home), deadline))
                    plan['backend_running_image_verified'] = True
                if preserve_hashes(home, plist, deadline=deadline, lifecycle_change=migration is not None) != preserved:
                    raise ReleaseRejected('Network or launch configuration changed during upgrade')
                plan.update(applied=True, service=final, configuration_unchanged=True,
                            backup=str(backup), operation_id=document['operation_id'],
                            launch_policy_changed=migration is not None,
                            rollback_includes_original_launch_agent=migration is not None)
                if candidate_release is not None: plan['rollback_includes_release_identity'] = True
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
    parser.add_argument('--separate-background', action='store_true', help='Journal an explicit background-only launch-agent migration and app+plist rollback')
    parser.add_argument('--isolated-fixture-root', type=Path, help='Explicit private disposable fixture scope; never changes production paths')
    parser.add_argument('--test-service-label', help='Independent fun.hackerlife.weavelo.lifecycle-test.<uuid> label; requires the fixture root')
    parser.add_argument('--expected-commit')
    parser.add_argument('--expected-identity-sha256')
    parser.add_argument('--expected-manifest-sha256')
    parser.add_argument('--native-observer', type=Path)
    parser.add_argument('--native-observer-sha256')
    parser.add_argument('--deadline-seconds', type=float, default=None, help='Cooperative budget; defaults to 300 for apply/plan, 120 for recovery')
    args = parser.parse_args()
    if args.recover and (args.apply or args.archive): parser.error('--recover cannot be combined with an archive or --apply')
    if args.recover and args.separate_background: parser.error('--recover cannot change forward launch policy')
    if bool(args.isolated_fixture_root) != bool(args.test_service_label): parser.error('Fixture root and independent test label must be specified together')
    if not args.recover and args.archive is None: parser.error('archive is required unless --recover is selected')
    try:
        expectations = release_identity.requested(args.expected_commit, args.expected_identity_sha256,
                            args.expected_manifest_sha256, args.native_observer, args.native_observer_sha256)
        if args.recover and expectations is not None: parser.error('Recovery restores the journaled previous release expectations; no new expectation overrides')
        scope = installer_scope.isolated(args.isolated_fixture_root, args.test_service_label) if args.isolated_fixture_root else nullcontext()
        with scope:
            if args.recover:
                result = recover_interrupted(deadline_seconds=120 if args.deadline_seconds is None else args.deadline_seconds)
            else:
                result = install(args.archive, args.apply, deadline_seconds=300 if args.deadline_seconds is None else args.deadline_seconds,
                                 separate_background=args.separate_background, expected_release=expectations)
        print('MACOS_INSTALL_RECEIPT ' + json.dumps(result), flush=True)
    except Exception as error:
        import sys
        print(f'MACOS_INSTALL_REJECTED: {error}', file=sys.stderr); sys.exit(1)
