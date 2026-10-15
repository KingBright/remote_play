#!/usr/bin/env python3
"""Validate current releases and delegate their transaction to the sole public authority.

No target copies, release generations or independent artifact ledger. Signatures do
not replace the separate fresh-target source identity and installation gates.
"""
from contextlib import contextmanager
import argparse
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import stat
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
HELPER = Path.home() / '.codex/skills/rust-project-lifecycle/scripts/rust_storage.py'
AUTHORITY = 'docs/plans/ARTIFACT-LIFECYCLE-ADOPTION.json'
LOCK_ENV = 'REMOTEPLAY_RELEASE_LOCK_FD'
PID_ENV = 'REMOTEPLAY_RELEASE_PRODUCER_PID'
ANDROID_SIGNER = '215aedabf17f40a454ad381d9027772eef9446346cae3db031cac2a5ae51c3a5'
ENTRIES = {'macos': 'package_macos_unified.sh', 'android-release': 'build_android_release.sh',
           'android-native': 'build_android_native.sh'}


class Refusal(RuntimeError):
    pass


def require(condition, reason):
    if not condition:
        raise Refusal(reason)


def regular(path):
    path = Path(path).absolute()
    require(path == path.resolve(strict=True), 'Linked release path rejected')
    info = path.stat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1,
            'Release file must be regular, owned, and without hardlinks')
    return path


def hashed(path):
    path = regular(path); before = path.stat(); value = hashlib.sha256()
    with path.open('rb') as incoming:
        for block in iter(lambda: incoming.read(1024 ** 2), b''):
            value.update(block)
    require(path.stat() == before, 'Release file changed while hashing')
    return value.hexdigest(), before.st_size


def document(path):
    path = regular(path); before = path.stat()
    require(before.st_size <= 256 * 1024, 'Release receipt exceeds metadata bound')
    raw = path.read_bytes()
    require(path.stat() == before, 'Release receipt changed while reading')
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


def public(action, *arguments):
    regular(HELPER)
    command = [sys.executable, '-B', str(HELPER), action, '--project', str(ROOT), *map(str, arguments)]
    result = subprocess.run(command, capture_output=True, text=True, timeout=180)
    require(result.returncode == 0, result.stderr.strip()[-2000:] or 'Public current-output transaction refused')
    return json.loads(result.stdout)


def macos_output_root():
    result = subprocess.run(['cargo', 'metadata', '--format-version', '1', '--no-deps', '--locked', '--offline'],
                            cwd=ROOT, capture_output=True, text=True, timeout=30)
    require(result.returncode == 0, 'Cannot resolve the existing Cargo output contract')
    target = Path(json.loads(result.stdout)['target_directory'])
    require(target.is_absolute() and target.resolve() == (Path.home() / 'rust-target').resolve(),
            'Production macOS packaging requires the existing ~/rust-target Cargo output; remove the target override')
    return target.resolve() / 'package/macos'


def android_files():
    apk = ROOT / 'android/app/build/outputs/apk/release/app-release.apk'
    return [apk, apk.with_suffix('.verification.json')]


def stage_paths(files):
    return [Path(path).with_name('.' + Path(path).name + '.pending') for path in files]


def android_workspace():
    return android_files()[0].parent / '.remoteplay-android-package.pending'


def check_lock(kind):
    require(LOCK_ENV in os.environ, 'Use the formal release script: its producer lock is required')
    fd = int(os.environ[LOCK_ENV]); path = ROOT / '.rust-storage/release-producer.lock'
    info = os.fstat(fd)
    require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
            and stat.S_IMODE(info.st_mode) == 0o600 and not path.is_symlink()
            and (info.st_dev, info.st_ino) == (path.stat().st_dev, path.stat().st_ino), 'Invalid producer lock descriptor')
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    require(info.st_size <= 65536, 'Producer lock metadata exceeds bound')
    state = json.loads(os.pread(fd, 65536, 0))
    require(state['project'] == str(ROOT) and (state['kind'] == kind or
            (state['kind'] == 'android-release' and kind == 'android-native')), 'Wrong producer lock scope')
    return fd, state


def write_state(fd, state):
    raw = json.dumps(state).encode()
    require(len(raw) <= 65536, 'Producer state exceeds bound')
    os.ftruncate(fd, 0); os.pwrite(fd, raw, 0); os.fsync(fd)


@contextmanager
def producer(kind):
    if LOCK_ENV in os.environ:
        yield check_lock(kind)
        return
    state_dir = ROOT / '.rust-storage'
    require(ROOT == ROOT.resolve(strict=True) and not state_dir.is_symlink(), 'Unsafe producer metadata path')
    state_dir.mkdir(mode=0o700, exist_ok=True)
    path = state_dir / 'release-producer.lock'
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    prepared = False
    state = {'project': str(ROOT), 'kind': kind, 'producer_pid': os.getpid()}
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
                and stat.S_IMODE(info.st_mode) == 0o600 and info.st_ino == path.stat().st_ino, 'Unsafe producer lock')
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise Refusal('Another RemotePlay release/JNI producer is active') from error
        # Existing authority remains mandatory; never create an independent current ledger.
        macos_root = macos_output_root() if kind == 'macos' else (Path.home() / 'rust-target/package/macos').resolve()
        public('release-setup', '--authority', ROOT / AUTHORITY, '--output-root', macos_root,
               '--output-root', android_files()[0].parent)
        public('release-preflight', '--producer-pid', state['producer_pid'])
        prepared = True
        write_state(fd, state)
        yield fd, state
    finally:
        if prepared and kind != 'android-native':
            try:
                public('release-preflight', '--abort-current', '--producer-pid', state['producer_pid'])
            except (Refusal, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
                # Public journal remains authoritative and blocks another transaction until recovered.
                print('CURRENT_OUTPUT_CLEANUP_DEFERRED: ' + str(error), file=sys.stderr)
        os.close(fd)


def admit(state, component, files, current=None, aux_stage=None):
    files = [Path(path).absolute() for path in files]
    for path in files:
        require(path.parent == path.parent.resolve(), 'Linked default output parent rejected')
        path.parent.mkdir(parents=True, exist_ok=True)
    args = ['--component', component, '--producer-pid', state['producer_pid']]
    for path in files:
        args.extend(['--file', path])
    if aux_stage:
        args.extend(['--aux-stage', aux_stage])
    if current:
        args.extend(['--adopt-current', '--current-validation-file', files[1],
                     '--current-validation-digest', current[1]])
        for value in current:
            args.extend(['--current-sha256', value])
    result = public('release-admit', *args)
    require(result.get('admission') == 'reserved' and result.get('files') == list(map(str, files))
            and result.get('staged_files') == list(map(str, stage_paths(files))), 'Exact current-output reservation was not confirmed')
    require(result.get('owned_aux_stages', []) == ([str(aux_stage)] if aux_stage else []), 'Unexpected packaging workspace ownership')
    return result


def publish(state, admission, expected):
    require(len(expected) == 2 and all(re.fullmatch('[0-9a-f]{64}', value) for value in expected),
            'Exact verified artifact and receipt digests are required')
    args = ['--component', admission['component'], '--producer-pid', state['producer_pid'],
            '--validation-kind', 'signing', '--validation-digest', expected[1],
            '--validation-file', admission['staged_files'][1]]
    for path, value in zip(admission['files'], expected):
        args.extend(['--file', path, '--expected-sha256', value])
    result = public('release-publish', *args)
    require(result.get('current_committed') is True and result.get('paths') == admission['files'],
            'Current output commit was not confirmed; preserve the public transaction for recovery')
    if result.get('cleanup_status') != 'completed':
        print('CURRENT_OUTPUT_CLEANUP_DEFERRED: ' + json.dumps(result.get('retention', {})), file=sys.stderr)
    return result


def macos_evidence(archive, receipt, expected_archive):
    report, receipt_sha = document(receipt); value, size = hashed(archive)
    require(report.get('signature_verified') is True and report.get('archive') == str(expected_archive)
            and report.get('archive_sha256') == value and report.get('size_bytes') == size,
            'macOS receipt does not bind the exact signed current archive')
    return report, [value, receipt_sha]


def publish_macos(state, admission, report):
    archive, receipt = map(Path, admission['staged_files'])
    recorded, expected = macos_evidence(archive, receipt, admission['files'][0])
    require(recorded == report, 'macOS receipt changed after signing verification')
    return publish(state, admission, expected)


def android_evidence(apk, receipt):
    report, receipt_sha = document(receipt); value, size = hashed(apk)
    require(report.get('sha256') == value and report.get('size_bytes') == size
            and report.get('signer_sha256') == ANDROID_SIGNER and report.get('debuggable') is False
            and report.get('abi') == ['arm64-v8a'] and report.get('native_16kb_aligned') is True,
            'APK and existing signing receipt do not agree; preserving current output')
    return [value, receipt_sha]


def verify_existing_android_signer(apk):
    sdk = Path(os.environ.get('ANDROID_SDK_ROOT') or os.environ.get('ANDROID_HOME') or Path.home() / 'Library/Android/sdk')
    tools = sorted((sdk / 'build-tools').glob('*/apksigner'),
                   key=lambda path: tuple(map(int, re.findall(r'\d+', path.parent.name))))
    require(bool(tools), 'Existing current APK requires the Android SDK signer verifier')
    result = subprocess.run([str(tools[-1]), 'verify', '--verbose', '--print-certs', str(apk)],
                            capture_output=True, text=True, timeout=60)
    fingerprint = re.search(r'Signer #1 certificate SHA-256 digest: ([0-9a-f]+)', result.stdout)
    require(result.returncode == 0 and fingerprint and fingerprint[1] == ANDROID_SIGNER
            and 'Android Debug' not in result.stdout, 'Existing APK signature verification failed; preserving it')


def admit_android(state):
    files = android_files(); current = None
    if any(path.exists() or path.is_symlink() for path in files):
        current = android_evidence(*files)
        verify_existing_android_signer(files[0])
    return admit(state, 'android-arm64-release', files, current=current, aux_stage=android_workspace())


def publish_android(state):
    from verify_release_apk import verify
    admission = state['admission']; apk, receipt = map(Path, admission['staged_files'])
    require(admission['owned_aux_stages'] == [str(android_workspace())], 'Unexpected APK packaging workspace')
    candidate = android_workspace() / 'release/app-release.apk'
    regular(candidate)
    report = verify(candidate, ROOT)  # Unchanged version/JNI/ABI/alignment and actual signature gates.
    value, size = hashed(candidate)
    require(report.get('sha256') == value and report.get('size_bytes') == size, 'APK changed after signing verification')
    require(not apk.exists() and not apk.is_symlink(), 'Staged APK already exists')
    require(not receipt.exists() and not receipt.is_symlink(), 'Staged receipt already exists')
    os.replace(candidate, apk)  # Same output filesystem: no APK copy or snapshot.
    with receipt.open('x') as outgoing:
        json.dump(report, outgoing, indent=2); outgoing.write('\n'); outgoing.flush(); os.fsync(outgoing.fileno())
    expected = android_evidence(apk, receipt)
    result = publish(state, admission, expected)
    print('RELEASE_APK_VERIFIED ' + json.dumps(report), flush=True)
    return result


def check_current(output_directory):
    # The JVM does not preserve arbitrary lock FDs when starting Python. The
    # public check verifies the transaction, process start identity and caller ancestry.
    regular(HELPER)
    spec = importlib.util.spec_from_file_location('rp_public_current_helper', HELPER)
    module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
    pid = int(os.environ.get(PID_ENV, '0'))
    require(pid > 1, 'Use scripts/build_android_release.sh for a verified current APK')
    result = module.companion('rust_release').validate_producer(
        module.release_api(), str(ROOT), 'android-arm64-release', list(map(str, android_files())), pid)
    require(result['staged_files'] == list(map(str, stage_paths(android_files()))), 'Unexpected APK stage contract')
    require(result['owned_aux_stages'] == [str(android_workspace())]
            and Path(output_directory).resolve() == android_workspace() / 'release',
            'Gradle APK output must be the exact public-owned packaging workspace')


def stop_child_group(child):
    # Only this Popen's new session. Its shell can exit before JVM descendants.
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        child.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass
    finally:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except PermissionError:
            # macOS can retain a zombie-only group after TERM while refusing
            # another signal. Never suppress EPERM for a live/unknown process.
            status = subprocess.run(['ps', '-ax', '-o', 'pgid=,stat='],
                                    capture_output=True, text=True, timeout=10)
            if status.returncode != 0 or any(
                len(parts := row.split()) >= 2 and parts[0] == str(child.pid)
                and not parts[1].startswith('Z') for row in status.stdout.splitlines()
            ):
                raise
        child.wait()


def run(kind, arguments):
    if kind == 'macos':
        check = subprocess.run([sys.executable, '-B', str(ROOT / 'scripts/macos_release_guard.py'), 'preflight'])
        require(check.returncode == 0, 'Signing preflight failed before release lifecycle changes')
    with producer(kind) as (fd, state):
        if kind == 'android-release':
            state['admission'] = admit_android(state); write_state(fd, state)
        env = dict(os.environ, **{LOCK_ENV: str(fd), PID_ENV: str(state['producer_pid'])})
        command = ['bash', str(ROOT / 'scripts' / ENTRIES[kind]), *arguments]
        child = subprocess.Popen(command, env=env, pass_fds=(fd,), start_new_session=True)
        try:
            return child.wait()
        except BaseException:
            stop_child_group(child)
            raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    command = commands.add_parser('run'); command.add_argument('kind', choices=ENTRIES)
    command.add_argument('arguments', nargs=argparse.REMAINDER)
    command = commands.add_parser('check-lock'); command.add_argument('kind', choices=ENTRIES)
    command = commands.add_parser('check-current')
    command.add_argument('--apk-output-directory', required=True)
    commands.add_parser('publish-android')
    args = parser.parse_args()
    if args.action == 'run':
        return run(args.kind, args.arguments)
    if args.action == 'check-lock':
        check_lock(args.kind)
    elif args.action == 'check-current':
        check_current(args.apk_output_directory)
    else:
        _, state = check_lock('android-release')
        print(json.dumps(publish_android(state)))
    return 0


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    try:
        sys.exit(main())
    except (Refusal, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f'RELEASE_LIFECYCLE_REJECTED: {error}', file=sys.stderr)
        sys.exit(2)
