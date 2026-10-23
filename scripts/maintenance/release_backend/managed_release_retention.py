#!/usr/bin/env python3
"""Reversibly archive exact registered release generations; dry-run by default.

No cache, build target, failure evidence, or unregistered path is a deletion
candidate. Backups are retained indefinitely: this adapter never prunes recovery.
"""
import argparse
import base64
import copy
import ctypes
import errno
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import uuid
import zipfile

MAX_BACKUP_BYTES = 256 * 1024 ** 2
MAX_RECOVERY_BYTES = 256 * 1024 ** 2
TRANSACTION = 'release-retention-transaction.json'
FORBIDDEN = {'target', 'build', 'deps', 'incremental', '.git', '.rust-storage', 'evidence', 'src'}


class Refusal(RuntimeError):
    pass


def trusted_file(path):
    path = Path(os.path.abspath(path))
    for part in (path, *path.parents):
        if part.is_symlink():
            raise Refusal(f'symlink component: {part}')
    info = path.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise Refusal(f'unsafe helper/metadata file: {path}')
    return path


def load_helper():
    # This is a sibling of the installed cleanup skill, never project-supplied code.
    path = Path(__file__).absolute().parents[2] / 'rust-project-lifecycle/scripts/rust_storage.py'
    if not path.exists() and not path.is_symlink():
        # The Gemini cleanup copy shares this one canonical, reviewed lifecycle
        # implementation; no environment or repository-provided import override.
        path = Path.home() / '.codex/skills/rust-project-lifecycle/scripts/rust_storage.py'
    trusted_file(path)
    trusted_file(path.with_name('rust_scope.py'))
    spec = importlib.util.spec_from_file_location('managed_release_rust_storage', path)
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    return helper


def compiler_activity(root):
    path = trusted_file(Path(__file__).absolute().with_name('rust_activity.py'))
    spec = importlib.util.spec_from_file_location('managed_release_activity', path)
    activity = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(activity)
    # The existing helper follows rustc -> Cargo parent locks and reinspects new
    # children, so an unrelated attributed build does not block this project.
    try:
        result = activity.inspect(str(root))
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        raise Refusal('compiler inspection failed: ' + str(error)) from error
    if result['state'] != 'idle':
        raise Refusal('project compiler active or compiler attribution unknown')


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(block)
    return result.hexdigest()


def xattrs(path):
    if hasattr(os, 'listxattr'):
        return {key: base64.b64encode(os.getxattr(path, key, follow_symlinks=False)).decode('ascii')
                for key in sorted(os.listxattr(path, follow_symlinks=False))}
    if sys.platform != 'darwin':
        raise Refusal('extended attribute inspection unavailable')
    # macOS Python does not expose os.*xattr. Use the system libc no-follow API,
    # rather than invoking xattr once per file or silently dropping attributes.
    libc = ctypes.CDLL(None, use_errno=True)
    libc.listxattr.argtypes = [ctypes.c_char_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int]
    libc.listxattr.restype = ctypes.c_ssize_t
    libc.getxattr.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p,
                             ctypes.c_size_t, ctypes.c_uint32, ctypes.c_int]
    libc.getxattr.restype = ctypes.c_ssize_t
    raw = os.fsencode(path)
    length = libc.listxattr(raw, None, 0, 1)
    if length < 0:
        raise OSError(ctypes.get_errno(), 'cannot list extended attributes', str(path))
    buffer = ctypes.create_string_buffer(length)
    actual = libc.listxattr(raw, buffer, length, 1)
    if actual < 0 or actual > length:
        raise Refusal('extended attributes changed during inspection')
    result = {}
    for key in sorted(buffer.raw[:actual].split(b'\0')):
        if not key:
            continue
        size = libc.getxattr(raw, key, None, 0, 0, 1)
        if size < 0 or size > 16 * 1024 ** 2:
            raise Refusal('extended attribute unavailable/oversized')
        value = ctypes.create_string_buffer(size)
        actual = libc.getxattr(raw, key, value, size, 0, 1)
        if actual != size:
            raise Refusal('extended attribute changed during inspection')
        result[os.fsdecode(key)] = base64.b64encode(value.raw[:size]).decode('ascii')
    return result


def tree_manifest(path, limit=MAX_BACKUP_BYTES):
    """Fail closed on links, special files, ownership, source/cache/database data."""
    pending, rows, total = [path], {}, 0
    started = time.monotonic()
    while pending:
        current = pending.pop()
        info = current.lstat()
        if len(rows) > 100000 or time.monotonic() - started > 60:
            raise Refusal('bounded release inspection exceeded limits')
        if current.name in FORBIDDEN or current.name in {'Cargo.toml', 'Cargo.lock'} or current.suffix.lower() in {
                '.rs', '.db', '.sqlite', '.sqlite3', '.mdb'}:
            raise Refusal(f'source/cache/evidence/database content: {current}')
        if info.st_uid != os.getuid() or stat.S_ISLNK(info.st_mode):
            raise Refusal(f'ownership or symlink: {current}')
        row = {'mode': stat.S_IMODE(info.st_mode), 'xattrs': xattrs(current)}
        if stat.S_ISDIR(info.st_mode):
            row['type'] = 'directory'
            with os.scandir(current) as entries:
                pending.extend(Path(item.path) for item in entries)
        elif stat.S_ISREG(info.st_mode) and info.st_nlink == 1:
            row.update(type='file', bytes=info.st_size)
            total += info.st_size
            if total > limit:
                raise Refusal('release exceeds backup byte budget; source retained')
            row['sha256'] = digest(current)
        else:
            raise Refusal(f'special file or hardlink: {current}')
        rows[str(current.relative_to(path))] = row
    return {'bytes': total, 'tree': rows}


def checked_run(command, timeout=30, empty_code=False):
    result = subprocess.run(command, capture_output=True, text=True,
                            encoding='utf-8', errors='surrogateescape', timeout=timeout)
    if result.stderr.strip() or result.returncode not in ({0, 1} if empty_code else {0}):
        raise Refusal(f'cannot reliably inspect/operate using {Path(command[0]).name}')
    return result.stdout


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for child in value.values():
            yield from strings(child)
    elif isinstance(value, list):
        for child in value:
            yield from strings(child)


def macports_launch_payload(plist, initial, system_folder, *,
                            system_root=Path('/Library/LaunchDaemons'),
                            ports_root=Path('/opt/local/etc/LaunchDaemons')):
    """Read a standard root-controlled MacPorts link; retain every reference check."""
    if (not system_folder or plist.parent != system_root or
            not plist.name.startswith('org.macports.') or initial.st_uid != 0 or
            not stat.S_ISLNK(initial.st_mode)):
        raise Refusal(f'cannot safely inspect launch service: {plist}')
    snapshots = [(plist, protocol_lock_stat(initial))]
    try:
        target = Path(os.readlink(plist))
        if target != ports_root / plist.stem / plist.name:
            raise Refusal(f'noncanonical MacPorts launch target: {plist}')
        for parent in dict.fromkeys((plist.parent, *plist.parents, target.parent, *target.parents)):
            info = parent.lstat()
            if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
                raise Refusal(f'untrusted MacPorts launch directory: {parent}')
            snapshots.append((parent, protocol_lock_stat(info)))
        info = target.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
            raise Refusal(f'untrusted MacPorts launch configuration: {target}')
        snapshots.append((target, protocol_lock_stat(info)))
        with os.fdopen(os.open(target, os.O_RDONLY | os.O_NOFOLLOW), 'rb') as stream:
            if protocol_lock_stat(os.fstat(stream.fileno())) != protocol_lock_stat(info):
                raise Refusal(f'MacPorts launch target changed before read: {plist}')
            payload = stream.read(1024 * 1024 + 1)
        if len(payload) > 1024 * 1024:
            raise Refusal(f'MacPorts launch configuration exceeds observation bound: {plist}')
        if any(protocol_lock_stat(path.lstat()) != before for path, before in snapshots):
            raise Refusal(f'MacPorts launch configuration changed during inspection: {plist}')
        return payload
    except OSError as error:
        raise Refusal(f'cannot inspect MacPorts launch configuration: {plist}') from error


def launch_references(paths):
    warnings = []
    for folder, system_folder in [(Path.home() / 'Library/LaunchAgents', False),
                                  (Path('/Library/LaunchAgents'), True),
                                  (Path('/Library/LaunchDaemons'), True)]:
        try:
            folder.lstat()
        except FileNotFoundError:
            continue
        if folder.is_symlink():
            raise Refusal('symlink launch service directory')
        with os.scandir(folder) as items:
            plists = [Path(item.path) for item in items if item.name.endswith('.plist')]
        for plist in plists:
            info = plist.lstat()
            try:
                if stat.S_ISLNK(info.st_mode):
                    payload = macports_launch_payload(plist, info, system_folder)
                else:
                    with plist.open('rb') as stream:
                        payload = stream.read()
                # Some installed XML plists put a newline before the declaration.
                # Normalize only XML's ASCII whitespace in memory, never binary
                # plists or malformed/unknown content, and never the source file.
                xml_payload = payload.lstrip(b' \t\r\n')
                if xml_payload.startswith((b'<?xml', b'<!DOCTYPE plist', b'<plist')):
                    payload = xml_payload
                values = list(strings(plistlib.loads(payload)))
            except PermissionError as error:
                if not system_folder or info.st_uid != 0 or not stat.S_ISREG(info.st_mode):
                    raise
                if plist.lstat() != info:
                    raise Refusal(f'launch configuration changed during inspection: {plist}')
                # A root-only unrelated configuration is not evidence that an
                # exact historical release is used. Preserve this visibility gap;
                # registry/current/rollback, processes, open files and verified
                # recovery remain mandatory independent checks before archiving.
                warnings.append({'path': str(plist), 'error': str(error),
                                 'reason': 'unreadable-root-owned-global-launch-config',
                                 'inspection_complete': False})
                continue
            for value in values:
                for path in paths:
                    if str(path) in value:
                        raise Refusal(f'launch service references release: {plist}')
                    if value.startswith('/') and '\n' not in value:
                        resolved = Path(os.path.realpath(value))
                        if resolved == path or resolved.is_relative_to(path):
                            raise Refusal(f'launch service resolves to release: {plist}')
    return warnings


PROTOCOL_LOCK_NAMES = {'release-producer.lock', 'fixed-validation.lock', 'lock'}


def protocol_lock_stat(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_gid,
            info.st_nlink, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def protocol_lock_snapshots(root, records, previous=None):
    """Verify only this invocation's already-held exact protocol lock FDs."""
    if not isinstance(records, (tuple, list)) or len(records) > 3:
        raise Refusal('invalid verified protocol lock records')
    root = Path(os.path.abspath(root))
    result, paths = {}, set()
    for row in records:
        if (not isinstance(row, dict) or set(row) != {'pid', 'fd', 'path', 'device', 'inode'} or
                any(type(row[key]) is not int for key in ('pid', 'fd', 'device', 'inode')) or
                row['pid'] != os.getpid() or row['fd'] < 0 or not isinstance(row['path'], str)):
            raise Refusal('invalid verified protocol lock identity')
        path = Path(row['path'])
        if (path.parent != root / '.rust-storage' or path.name not in PROTOCOL_LOCK_NAMES or
                row['path'] != str(path) or not path.is_absolute() or
                any(part.is_symlink() for part in (path, *path.parents))):
            raise Refusal('verified protocol lock path is not an exact owned lock')
        key = (row['pid'], row['fd'])
        if key in result or path in paths:
            raise Refusal('duplicate verified protocol lock')
        try:
            info, named = os.fstat(row['fd']), path.lstat()
            snapshot = protocol_lock_stat(info)
            if (snapshot != protocol_lock_stat(named) or not stat.S_ISREG(info.st_mode) or
                    info.st_uid != os.getuid() or info.st_nlink != 1 or info.st_mode & 0o022 or
                    (info.st_dev, info.st_ino) != (row['device'], row['inode'])):
                raise Refusal('verified protocol lock FD/path changed')
            # macOS lsof reports an empty lock field for flock. A fresh FD must
            # conflict with both EX and SH probes, proving an existing exclusive
            # lock. Never acquire/release the supplied FD or a global lock.
            probe = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            try:
                if protocol_lock_stat(os.fstat(probe)) != snapshot:
                    raise Refusal('protocol lock changed before held-lock probe')
                for mode in (fcntl.LOCK_EX, fcntl.LOCK_SH):
                    try:
                        fcntl.flock(probe, mode | fcntl.LOCK_NB)
                    except OSError as error:
                        if error.errno not in {errno.EAGAIN, errno.EWOULDBLOCK}:
                            raise
                    else:
                        fcntl.flock(probe, fcntl.LOCK_UN)
                        raise Refusal('verified protocol FD is not exclusively locked')
            finally:
                os.close(probe)
            if (protocol_lock_stat(os.fstat(row['fd'])) != snapshot or
                    protocol_lock_stat(path.lstat()) != snapshot):
                raise Refusal('verified protocol lock changed during inspection')
        except OSError as error:
            raise Refusal('cannot verify protocol lock FD/path or held lock') from error
        result[key] = {'record': dict(row), 'stat': snapshot}
        paths.add(path)
    if previous is not None and result != previous:
        raise Refusal('verified protocol locks changed during activity inspection')
    return result


def only_protocol_open_files(output, verified, expected):
    """Parse lsof NUL fields; every observed FD must match one exact proof."""
    if not output:
        if expected:
            raise Refusal('lsof omitted an expected held protocol lock')
        return
    if not output.endswith('\0\n'):
        raise Refusal('malformed protocol lsof field output')
    groups, group = [], {}
    for item in output[:-1].split('\0')[:-1]:
        boundary = item.startswith('\n')
        if boundary:
            if not group:
                raise Refusal('empty protocol lsof field group')
            groups.append(group); group = {}
            item = item[1:]
        if not item or item[0] not in {'p', 'f', 'D', 'i', 'n'} or item[0] in group:
            raise Refusal('unknown or repeated protocol lsof field')
        group[item[0]] = item[1:]
    groups.append(group)
    pid, seen_pids, seen = None, set(), set()
    process_has_file = False
    for group in groups:
        if set(group) == {'p'}:
            if (not group['p'].isdigit() or int(group['p']) <= 0 or int(group['p']) in seen_pids or
                    (pid is not None and not process_has_file)):
                raise Refusal('invalid protocol lsof process group')
            pid = int(group['p']); seen_pids.add(pid); process_has_file = False
            continue
        if (pid is None or set(group) != {'f', 'D', 'i', 'n'} or not group['f'].isdigit() or
                not re.fullmatch(r'0x[0-9a-fA-F]+', group['D']) or not group['i'].isdigit()):
            raise Refusal('unowned or incomplete protocol lsof file group')
        key = (pid, int(group['f']))
        proof = verified.get(key, {}).get('record')
        if (key in seen or proof is None or group['n'] != proof['path'] or
                int(group['D'], 16) != proof['device'] or int(group['i']) != proof['inode']):
            raise Refusal('release has open files outside the exact protocol locks')
        seen.add(key); process_has_file = True
    if not process_has_file or seen != expected:
        raise Refusal('lsof protocol lock set is incomplete or outside the inspected path')


def assert_idle(root, paths, verified_control_pids=(), *, verified_protocol_locks=()):
    # The cooperative project + shlock locks are already held. These checks also
    # protect existing users of a release and noncooperating compiler processes.
    protocol = protocol_lock_snapshots(root, verified_protocol_locks)
    output = checked_run(['/bin/ps', '-axo', 'pid=,comm=,args='], timeout=10)
    if not output.strip():
        raise Refusal('empty process inspection')
    for line in output.splitlines():
        fields = line.strip().split(None, 2)
        if len(fields) < 2 or not fields[0].isdigit():
            raise Refusal('invalid process inspection')
        if int(fields[0]) == os.getpid():
            continue
        command = fields[2] if len(fields) == 3 else fields[1]
        # Current-update callers may supply only their verified helper-to-owner
        # ancestor chain. This excludes control-command arguments, never file
        # users, compiler checks, producer descendants or unrelated processes.
        if int(fields[0]) not in verified_control_pids and any(str(path) in command for path in paths):
            raise Refusal('process references release')
        if re.search(r'(?:^|/)xcodebuild(?:\s|$)', line):
            raise Refusal('Xcode build attribution is unknown')
    compiler_activity(root)
    for path in paths:
        if path.exists():
            fields = '-F0pfDin' if protocol else '-Fn'
            command = (['/usr/sbin/lsof', '-nP', '+D', str(path), fields] if path.is_dir() else
                       ['/usr/sbin/lsof', '-nP', fields, '--', str(path)])
            opened = checked_run(command, empty_code=True)
            if protocol:
                expected = {key for key, value in protocol.items()
                            if Path(value['record']['path']) == path or
                            (path.is_dir() and Path(value['record']['path']).is_relative_to(path))}
                only_protocol_open_files(opened, protocol, expected)
            elif opened.strip():
                raise Refusal('release has open files')
    warnings = launch_references(paths)
    protocol_lock_snapshots(root, verified_protocol_locks, protocol)
    return warnings


def note_warnings(destination, warnings):
    if destination is not None:
        for warning in warnings or []:
            if warning not in destination:
                destination.append(warning)


def safe_dir(helper, path):
    helper.safe_absolute(path)
    if not path.exists():
        path.mkdir(mode=0o700)
    info = path.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise Refusal(f'unsafe recovery directory: {path}')
    return path


def ditto_create(source, archive):
    checked_run(['/usr/bin/ditto', '-c', '-k', '--sequesterRsrc', '--keepParent', '--rsrc',
                 str(source), str(archive)], timeout=120)


def ditto_restore(archive, destination):
    checked_run(['/usr/bin/ditto', '-x', '-k', '--rsrc', str(archive), str(destination)], timeout=120)


def verify_backup(recovery, metadata, limit):
    if (not re.fullmatch(r'\.?[A-Za-z0-9_-]+\.zip', metadata.get('archive', ''))
            or Path(metadata.get('basename', '')).name != metadata.get('basename')
            or metadata.get('basename') in {'', '.', '..'}):
        raise Refusal('unsafe backup names')
    archive = trusted_file(recovery / metadata['archive'])
    if archive.parent != recovery or archive.name != metadata['archive']:
        raise Refusal('backup escapes recovery root')
    if digest(archive) != metadata['sha256']:
        raise Refusal('backup checksum mismatch')
    if shutil.disk_usage(recovery).free < metadata['manifest']['bytes'] + 64 * 1024 ** 2:
        raise Refusal('insufficient headroom to verify recovery archive')
    # Check before ditto extraction, including AppleDouble entries from --rsrc.
    with zipfile.ZipFile(archive) as zipped:
        unpacked = 0
        for item in zipped.infolist():
            name = Path(item.filename)
            unpacked += item.file_size
            if (name.is_absolute() or '..' in name.parts or not name.parts
                    or name.parts[0] not in {metadata['basename'], '__MACOSX'}
                    or stat.S_ISLNK(item.external_attr >> 16)
                    or unpacked > 2 * limit + 16 * 1024 ** 2):
                raise Refusal('unsafe or oversized recovery archive')
    with tempfile.TemporaryDirectory(prefix='.verify-release-', dir=recovery) as temporary:
        temporary = Path(temporary)
        ditto_restore(archive, temporary)
        if sorted(p.name for p in temporary.iterdir()) != [metadata['basename']]:
            raise Refusal('unexpected restored archive contents')
        restored = temporary / metadata['basename']
        if tree_manifest(restored, limit) != metadata['manifest']:
            raise Refusal('backup restore bytes/mode/xattrs mismatch')
    return archive


def prepare_backup(helper, root, entry, limit):
    source = helper.artifact_path(root, entry['path'], helper.load(root)[1])
    before = tree_manifest(source, limit)
    recovery = safe_dir(helper, root / helper.STATE / 'recovery')
    token = hashlib.sha256((entry['id'] + '\0' + entry['generation']).encode()).hexdigest()[:32]
    record = recovery / (token + '.json')
    if record.exists():
        trusted_file(record)
        metadata = json.loads(record.read_text())
        if metadata.get('entry') != entry or metadata.get('manifest') != before:
            raise Refusal('existing recovery metadata differs; do not overwrite backup')
        verify_backup(recovery, metadata, limit)
        return metadata
    metadata_bound = len(json.dumps(before).encode()) + len(json.dumps(entry).encode()) + 8192
    # Persistent recovery has a separate hard cap across all invocations. Do not
    # purge older backups to meet it. Reserve the uncompressed source upper bound.
    recovery_bytes = helper.measure(recovery)
    if recovery_bytes + before['bytes'] + metadata_bound > MAX_RECOVERY_BYTES:
        raise Refusal('persistent recovery byte budget exhausted; source retained')
    # At most one source copy plus one restored copy is temporarily present.
    if shutil.disk_usage(root).free < 2 * before['bytes'] + 64 * 1024 ** 2:
        raise Refusal('insufficient headroom for bounded backup verification')
    archive = recovery / (token + '.zip')
    if archive.exists():
        raise Refusal('unregistered archive exists; preserve for inspection')
    temporary = recovery / ('.' + token + '-' + uuid.uuid4().hex + '.zip')
    try:
        ditto_create(source, temporary)
        metadata = {'schema': 1, 'entry': copy.deepcopy(entry), 'archive': temporary.name,
                    'basename': source.name, 'sha256': digest(temporary), 'manifest': before}
        verify_backup(recovery, metadata, limit)
        if tree_manifest(source, limit) != before:
            raise Refusal('source changed during backup')
        if temporary.stat().st_size >= before['bytes']:
            raise Refusal('compression does not save space; source retained')
        if recovery_bytes + temporary.stat().st_size + metadata_bound > MAX_RECOVERY_BYTES:
            raise Refusal('persistent recovery byte budget exhausted; source retained')
        temporary.rename(archive)
        metadata['archive'] = archive.name
        helper.write_json(record, metadata)
        return metadata
    finally:
        if temporary.exists():
            temporary.unlink()  # Only this call's incomplete generated backup.


def exact_release(helper, root, policy, entry):
    if (entry.get('kind') != 'release' or entry.get('role') != 'history'
            or entry.get('owner') != policy['project_id'] or entry.get('unique_delivery')):
        return False
    if not isinstance(entry.get('generation'), str) or not entry['generation'].strip():
        return False
    if not isinstance(entry.get('id'), str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,128}', entry['id']):
        return False
    if policy.get('mode') == 'release-only':
        if entry.get('domain') != 'delivery' or not entry.get('content_signature'):
            return False
        path = helper.artifact_path(root, entry['path'], policy)
        return path.is_file() or path.is_dir()
    path = Path(entry['path'])
    if len(path.parts) != 2 or path.parts[0] != 'units' or path.parts[1] in FORBIDDEN:
        return False
    return helper.artifact_path(root, entry['path'], helper.load(root)[1]).is_dir()


def plan(helper, root, policy, registry):
    original = helper.report(root, policy, registry)
    prospective = copy.deepcopy(registry)
    ids = [entry.get('id') for entry in registry['entries']]
    if len(ids) != len(set(ids)):
        raise Refusal('duplicate registry ids')
    for entry in prospective['entries']:
        if exact_release(helper, root, policy, entry):
            # Only these two backing-related reasons can disappear. All other
            # report protections, including entire-generation protection, remain.
            entry['backed_up'] = True
    future = helper.report(root, policy, prospective)
    eligible = {entry['id'] for entry in future['candidates']
                if exact_release(helper, root, policy, entry)}
    groups = {}
    for entry in registry['entries']:
        if entry.get('id') in eligible:
            groups.setdefault(entry['generation'], []).append(entry)
    # Never archive only one platform component of a generation.
    for generation in list(groups):
        members = [entry for entry in registry['entries'] if entry.get('generation') == generation]
        if {entry['id'] for entry in members} != {entry['id'] for entry in groups[generation]}:
            del groups[generation]
    return original, groups


def transaction_paths(helper, root, tx):
    if not re.fullmatch(r'[0-9a-f]{32}', tx.get('id', '')):
        raise Refusal('invalid retention transaction id')
    parent = helper.safe_absolute(root / helper.STATE / 'retiring' / tx['id'])
    paths = []
    for metadata in tx['backups']:
        entry = metadata['entry']
        if not re.fullmatch(r'[A-Za-z0-9_-]{1,128}', entry.get('id', '')):
            raise Refusal('invalid transaction entry id')
        paths.append((helper.artifact_path(root, entry['path'], helper.load(root)[1]), parent / entry['id'], metadata))
    return parent, paths


def finish_transaction(helper, root, registry, tx, limit, inspection_warnings=None):
    """Retry committed removals, or roll back pre-commit moves, after a crash."""
    parent, paths = transaction_paths(helper, root, tx)
    ids = {metadata['entry']['id'] for _, _, metadata in paths}
    live = {entry['id'] for entry in registry['entries']}
    archived = {entry['id'] for entry in registry.get('archived_entries', [])}
    committed = ids <= archived and not ids & live
    if not committed and not ids <= live:
        raise Refusal('ambiguous retention transaction registry state')
    recovery = safe_dir(helper, root / helper.STATE / 'recovery')
    for source, retired, metadata in paths:
        verify_backup(recovery, metadata, limit)
        if committed:
            if source.exists():
                raise Refusal('source reappeared after archive commit; preserve both')
            if retired.exists():
                note_warnings(inspection_warnings, assert_idle(root, [source, retired]))
                actual = tree_manifest(retired, limit)
                expected = metadata['manifest']['tree']
                if any(expected.get(key) != row for key, row in actual['tree'].items()):
                    raise Refusal('retiring tree changed; preserve unexpected content')
                if retired.is_dir():
                    shutil.rmtree(retired)
                else:
                    retired.unlink()
        elif retired.exists():
            if source.exists():
                raise Refusal('source and retiring path both exist; preserve both')
            if tree_manifest(retired, limit) != metadata['manifest']:
                raise Refusal('uncommitted retiring tree changed; preserve for recovery')
            retired.rename(source)
        elif not source.exists():
            raise Refusal('uncommitted source missing; restore from verified recovery archive')
    if parent.exists():
        parent.rmdir()
    (root / helper.STATE / TRANSACTION).unlink()
    return 'completed' if committed else 'rolled-back'


def archive_generation(helper, root, policy, registry, entries, limit, inspection_warnings=None):
    sources = [helper.artifact_path(root, entry['path'], helper.load(root)[1]) for entry in entries]
    note_warnings(inspection_warnings, assert_idle(root, sources))
    backups = [prepare_backup(helper, root, entry, limit) for entry in entries]
    prospective = copy.deepcopy(registry)
    ids = {entry['id'] for entry in entries}
    for entry in prospective['entries']:
        if entry['id'] in ids:
            entry['backed_up'] = True
    if not ids <= {entry['id'] for entry in helper.report(root, policy, prospective)['candidates']}:
        raise Refusal('generation is protected after verified backup')
    if helper.load(root)[2] != registry:
        raise Refusal('registry changed despite held lock')
    for source, metadata in zip(sources, backups):
        if tree_manifest(source, limit) != metadata['manifest']:
            raise Refusal('source changed before retirement')
    note_warnings(inspection_warnings, assert_idle(root, sources))
    if any(source.stat().st_dev != (root / helper.STATE).stat().st_dev for source in sources):
        raise Refusal('cross-filesystem retirement is unsupported; source retained')
    tx = {'schema': 1, 'id': uuid.uuid4().hex, 'backups': backups}
    retiring = safe_dir(helper, root / helper.STATE / 'retiring')
    parent = safe_dir(helper, retiring / tx['id'])
    txpath = root / helper.STATE / TRANSACTION
    helper.write_json(txpath, tx)
    try:
        for source, entry in zip(sources, entries):
            source.rename(parent / entry['id'])
        note_warnings(inspection_warnings, assert_idle(root, sources + [parent / entry['id'] for entry in entries]))
        updated = copy.deepcopy(registry)
        updated['entries'] = [entry for entry in updated['entries'] if entry['id'] not in ids]
        for metadata in backups:
            entry = copy.deepcopy(metadata['entry'])
            archive = root / helper.STATE / 'recovery' / metadata['archive']
            entry.update(archived_at=time.time(), backup_path=str(archive), backup_sha256=metadata['sha256'],
                         backup_bytes=archive.stat().st_size, recovery_record=str(archive.with_suffix('.json')),
                         restore_command=shlex.join([sys.executable, str(Path(__file__).absolute()),
                                                     '--project', str(root), '--restore', entry['id'], '--apply']))
            updated.setdefault('archived_entries', []).append(entry)
        helper.write_registry(root, helper.load(root)[1], updated)
    except BaseException:
        finish_transaction(helper, root, helper.load(root)[2], tx, limit, inspection_warnings)
        raise
    finish_transaction(helper, root, updated, tx, limit, inspection_warnings)
    return updated


def restore_entry(helper, root, registry, identifier, limit):
    entries = [entry for entry in registry.get('archived_entries', []) if entry['id'] == identifier]
    if len(entries) != 1:
        raise Refusal('restore requires one exact archived entry id')
    entry = entries[0]
    recovery = safe_dir(helper, root / helper.STATE / 'recovery')
    record = trusted_file(entry['recovery_record'])
    if record.parent != recovery:
        raise Refusal('recovery record outside managed recovery directory')
    metadata = json.loads(record.read_text())
    if metadata['entry']['id'] != identifier or str(recovery / metadata['archive']) != entry['backup_path']:
        raise Refusal('recovery record mismatch')
    archive = verify_backup(recovery, metadata, limit)
    source = helper.artifact_path(root, metadata['entry']['path'], helper.load(root)[1])
    if source.exists():
        if tree_manifest(source, limit) != metadata['manifest']:
            raise Refusal('restore destination differs; never overwrite a release')
    else:
        with tempfile.TemporaryDirectory(prefix='.restore-release-', dir=recovery) as temporary:
            temporary = Path(temporary)
            ditto_restore(archive, temporary)
            restored = temporary / metadata['basename']
            if tree_manifest(restored, limit) != metadata['manifest']:
                raise Refusal('restore validation failed')
            restored.rename(source)
    updated = copy.deepcopy(registry)
    updated['entries'].append(metadata['entry'])
    updated['archived_entries'] = [row for row in updated['archived_entries'] if row['id'] != identifier]
    helper.write_registry(root, helper.load(root)[1], updated)
    return {'mode': 'restored', 'id': identifier, 'path': str(source)}


def retention_status(report, limit):
    """Describe the observed release state, not whether an apply call ran."""
    reasons = []
    if report.get('pending_transaction'):
        reasons.append('unfinished-retention-transaction')
    legacy = report.get('legacy_retention')
    if not isinstance(legacy, dict) or not isinstance(legacy.get('compiled_paths'), list):
        reasons.append('release-identity-coverage-unknown')
    elif legacy['compiled_paths']:
        reasons.append('legacy-releases-without-generation')
    rows = report.get('candidates', []) + report.get('protected', [])
    releases = [row for row in rows if row.get('kind') == 'release']
    generations = {row['generation'] for row in releases if row.get('generation')}
    if any(not row.get('generation') for row in releases) and 'legacy-releases-without-generation' not in reasons:
        reasons.append('legacy-releases-without-generation')
    if len(generations) > limit:
        reasons.append('release-generation-count-exceeds-limit')
    if any('generation' in item or 'generations' in item for item in report.get('retention_exceptions', [])):
        reasons.append('protected-release-generation-exceptions')
    if report.get('unregistered_paths_keep'):
        reasons.append('unregistered-artifact-identity-unknown')
    return {'retention_satisfied': not reasons, 'retention_scope': ('registered-delivery-generations-only' if report.get('release_domain') == 'delivery'
                                                       else 'registered-release-generations'),
            'retention_incomplete_reasons': reasons, 'release_generation_count': len(generations),
            'release_generation_limit': limit}


def execute(project, apply=False, max_backup_bytes=MAX_BACKUP_BYTES, restore=None, helper=None):
    helper = helper or load_helper()
    inspection_warnings = []
    if not 0 < max_backup_bytes <= MAX_BACKUP_BYTES:
        raise Refusal('backup budget must be between 1 and 256 MiB; targets are unsupported')
    with helper.lock(project, global_cleanup=True) as (root, policy, registry):
        names = (('release-binding.json', 'release-deliveries.json', 'lock') if policy.get('mode') == 'release-only'
                 else ('policy.json', 'registry.json', 'lock'))
        for name in names:
            trusted_file(root / helper.STATE / name)
        txpath = root / helper.STATE / TRANSACTION
        if txpath.exists():
            trusted_file(txpath)
            if not apply:
                return {'mode': 'dry-run', 'retention_satisfied': False,
                        'retention_incomplete_reasons': ['unfinished-retention-transaction'],
                        'protected': ['unfinished-transaction; use --apply to safely resume']}
            finish_transaction(helper, root, registry, json.loads(txpath.read_text()), max_backup_bytes, inspection_warnings)
            root, policy, registry = helper.load(root)
        if restore:
            if not apply:
                raise Refusal('restore requires --apply')
            result = restore_entry(helper, root, registry, restore, max_backup_bytes)
            result['inspection_warnings'] = inspection_warnings
            return result
        original, groups = plan(helper, root, policy, registry)
        result = {'mode': 'apply' if apply else 'dry-run', 'project': str(root),
                  'free_bytes_before': shutil.disk_usage(root).free, 'archived': [], 'protected': [],
                  'inspection_warnings': inspection_warnings,
                  'retained_generations': original.get('retained_generations', []), 'candidates': []}
        budget_used = 0
        for generation, entries in groups.items():
            try:
                size = sum(tree_manifest(helper.artifact_path(root, entry['path'], helper.load(root)[1]), max_backup_bytes)['bytes']
                           for entry in entries)
                if budget_used + size > max_backup_bytes:
                    raise Refusal('per-run backup byte budget exhausted')
                note_warnings(inspection_warnings, assert_idle(root, [helper.artifact_path(root, entry['path'], helper.load(root)[1]) for entry in entries]))
                row = {'generation': generation, 'paths': [entry['path'] for entry in entries], 'logical_bytes': size}
                result['candidates'].append(row)
                budget_used += size
                if apply:
                    registry = archive_generation(helper, root, policy, registry, entries, max_backup_bytes, inspection_warnings)
                    result['archived'].append(row)
            except (Refusal, OSError, ValueError, subprocess.SubprocessError) as error:
                result['protected'].append({'generation': generation, 'reason': str(error)})
                if txpath.exists():
                    break
        result['free_bytes_after'] = shutil.disk_usage(root).free
        result['free_bytes_change'] = result['free_bytes_after'] - result['free_bytes_before']
        result['report'] = helper.report(root, policy, registry) if not txpath.exists() else {'pending_transaction': str(txpath)}
        result.update(retention_status(result['report'], policy['keep_history']))
        result['retained_generations'] = result['report'].get('retained_generations', [])
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--project', required=True)
    parser.add_argument('--apply', action='store_true')
    parser.add_argument('--max-backup-bytes', type=int, default=MAX_BACKUP_BYTES)
    parser.add_argument('--restore', metavar='ARCHIVED_ENTRY_ID')
    args = parser.parse_args()
    try:
        result = execute(args.project, args.apply, args.max_backup_bytes, args.restore)
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0 if not result.get('protected') else 2
    except (Exception, KeyboardInterrupt) as error:
        print(json.dumps({'mode': 'refused', 'retention_satisfied': False,
                          'retention_incomplete_reasons': ['inspection-or-operation-refused'],
                          'error': str(error)}), file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
