#!/usr/bin/env python3
"""Thin release adapter: validate exact outputs and delegate retention to the public helper.

Delivery generations do not assert installed state or fresh-target source identity.
This adapter never deletes artifacts, copies a target, or changes Cargo settings.
"""
from contextlib import contextmanager
import argparse
import fcntl
import hashlib
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
GENERATION_ENV = 'REMOTEPLAY_RELEASE_GENERATION'
ANDROID_SIGNER = '215aedabf17f40a454ad381d9027772eef9446346cae3db031cac2a5ae51c3a5'
MAX_SNAPSHOT = 256 * 1024 ** 2
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
            'Release file must be a regular owned file without hardlinks')
    return path


def hashed(path):
    path = regular(path); before = path.stat(); value = hashlib.sha256()
    with path.open('rb') as incoming:
        for block in iter(lambda: incoming.read(1024 ** 2), b''):
            value.update(block)
    require(path.stat() == before, 'Release file changed while hashing')
    return value.hexdigest(), before.st_size


def document(path):
    path = regular(path)
    before = path.stat()
    require(before.st_size <= 256 * 1024, 'Release receipt exceeds metadata bound')
    raw = path.read_bytes()
    require(path.stat() == before, 'Release receipt changed while reading')
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


def public(action, *arguments):
    regular(HELPER)
    command = [sys.executable, '-B', str(HELPER), action, '--project', str(ROOT), *map(str, arguments)]
    result = subprocess.run(command, capture_output=True, text=True, timeout=180)
    require(result.returncode == 0, result.stderr.strip()[-2000:] or 'Public release lifecycle refused')
    return json.loads(result.stdout)


def git(*arguments):
    result = subprocess.run(['git', '--no-optional-locks', '-C', str(ROOT), *arguments],
                            capture_output=True, text=True, timeout=30)
    require(result.returncode == 0, 'Cannot determine producer checkout identity')
    return result.stdout.strip()


def observed_generation():
    require(not git('status', '--porcelain=v1', '-uall'), 'Automatic delivery grouping requires a clean checkout')
    commit = git('rev-parse', 'HEAD')
    version = (ROOT / 'VERSION').read_text().strip()
    require(re.fullmatch(r'[0-9a-f]{40}', commit) and re.fullmatch(r'\d+\.\d+\.\d+(?:-alpha\.\d+)?', version),
            'Invalid producer checkout observation')
    # This groups platform components; it is explicitly not compiled provenance.
    return f'producer-{version}-{commit}'


def generation(kind, explicit_required=False):
    selected = os.environ.get(GENERATION_ENV)
    if selected:
        require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.+-]{0,127}', selected), 'Invalid delivery generation')
        return selected, 'explicit-delivery-group'
    require(not explicit_required and not os.environ.get('REMOTE_PLAY_PREBUILT_BINARY'),
            'Prebuilt packaging requires REMOTEPLAY_RELEASE_GENERATION from its handoff')
    return observed_generation(), 'producer-checkout-observation-only'


def macos_output_root():
    result = subprocess.run(['cargo', 'metadata', '--format-version', '1', '--no-deps', '--locked', '--offline'],
                            cwd=ROOT, capture_output=True, text=True, timeout=30)
    require(result.returncode == 0, 'Cannot resolve the existing Cargo output contract')
    target = Path(json.loads(result.stdout)['target_directory'])
    require(target.is_absolute() and target == target.resolve(), 'Unsafe Cargo target path')
    return target / 'package/macos'


def check_lock(kind):
    require(LOCK_ENV in os.environ, 'Release producer lock is required')
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


@contextmanager
def producer(kind, explicit_required=False):
    if LOCK_ENV in os.environ:
        yield check_lock(kind)
        return
    state_dir = ROOT / '.rust-storage'
    require(ROOT == ROOT.resolve(strict=True) and not state_dir.is_symlink(), 'Unsafe producer metadata path')
    state_dir.mkdir(mode=0o700, exist_ok=True)
    path = state_dir / 'release-producer.lock'
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
                and stat.S_IMODE(info.st_mode) == 0o600 and info.st_ino == path.stat().st_ino,
                'Unsafe producer lock')
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise Refusal('Another RemotePlay release/JNI producer is active') from error
        public('release-setup', '--authority', ROOT / AUTHORITY, '--output-root', macos_output_root())
        preflight = public('release-preflight')
        require(isinstance(preflight.get('registered_deliveries'), list), 'Public release summary protocol is unavailable')
        selected, basis = (None, 'component-only') if kind == 'android-native' else generation(kind, explicit_required)
        if selected:
            require(preflight.get('registered_deliveries_complete') is True,
                    'Delivery identity coverage is unknown; retry before producing another release')
            component = 'macos-arm64-release' if kind == 'macos' else 'android-arm64-release'
            require(not any(item.get('generation') == selected and item.get('component') == component
                            for item in preflight['registered_deliveries']),
                    'Delivery generation/component already registered (live or archived); '
                    'use a new REMOTEPLAY_RELEASE_GENERATION before rebuilding')
        known = [{key: item[key] for key in ('generation', 'validation_kind', 'validation_digest', 'archived')}
                 for item in preflight.get('registered_deliveries', [])
                 if item.get('component') == 'android-arm64-release' and item.get('snapshot') is True]
        state = {'project': str(ROOT), 'kind': kind, 'generation': selected, 'generation_basis': basis,
                 'android_deliveries': known,
                 'deliveries_complete': preflight.get('registered_deliveries_complete') is True}
        require(len(json.dumps(state).encode()) <= 65536, 'Too many protected release entries for bounded producer state')
        os.ftruncate(fd, 0); os.pwrite(fd, json.dumps(state).encode(), 0); os.fsync(fd)
        yield fd, state
    finally:
        # The stable lock file remains; only this invocation's descriptor closes.
        os.close(fd)


def publish(state, component, files, validation_kind, receipt, expected, snapshot=False, selected=None,
            defer_failure=False):
    require(selected or state.get('generation'), 'Complete delivery generation is required')
    if not selected and state.get('generation_basis') == 'producer-checkout-observation-only':
        require(state['generation'] == observed_generation(), 'Producer checkout changed before delivery registration')
    require(len(expected) == len(files) and all(re.fullmatch('[0-9a-f]{64}', value) for value in expected),
            'Exact verified file digests are required')
    receipt_sha = expected[files.index(receipt)]
    args = ['--generation', selected or state['generation'], '--component', component,
            '--validation-kind', validation_kind, '--validation-digest', receipt_sha]
    for path, value in zip(files, expected):
        args.extend(['--file', str(path)])
        args.extend(['--expected-sha256', value])
    if snapshot:
        args.append('--snapshot')
    try:
        if snapshot:
            require(sum(regular(path).stat().st_size for path in files) <= MAX_SNAPSHOT, 'Exact release group exceeds snapshot budget')
        result = public('release-publish', *args)
        require(result.get('registration') in ('created', 'unchanged'),
                'Release registration was not confirmed; retain the pending output group')
        return result
    except (Refusal, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        if not defer_failure:
            raise
        result = {'status': 'deferred', 'domain': 'delivery', 'retention_satisfied': False,
                  'installed_status': 'not-asserted', 'reason': str(error)}
        print('RELEASE_RETENTION_DEFERRED ' + json.dumps(result), file=sys.stderr)
        return result


def admit(state, component, files, snapshot=False, selected=None):
    """Reserve one exact output group before production; failures retain the public reservation."""
    chosen = selected or state.get('generation')
    require(chosen, 'Complete delivery generation is required before production')
    args = ['--generation', chosen, '--component', component]
    for path in files:
        args.extend(['--file', str(Path(path).absolute())])
    if snapshot:
        args.append('--snapshot')
    result = public('release-admit', *args)
    require(result.get('admission') == 'reserved', 'Release output admission was not confirmed')
    return result


def admit_macos(state, output, version, build):
    archive = Path(output) / f'RemotePlay-macos-arm64-{version}-{build}.zip'
    return admit(state, 'macos-arm64-release', [archive, archive.with_suffix('.verification.json')])


def publish_macos(state, report):
    archive = regular(Path(report['archive']))
    receipt = archive.with_suffix('.verification.json')
    recorded, receipt_sha = document(receipt)
    require(recorded == report, 'macOS receipt changed after signing verification')
    value, size = hashed(archive)
    require(report.get('signature_verified') is True and report.get('archive_sha256') == value
            and report.get('size_bytes') == size, 'macOS receipt does not bind the exact signed archive')
    return publish(state, 'macos-arm64-release', [archive, receipt], 'signing', receipt,
                   [value, receipt_sha], defer_failure=True)


def android_files():
    apk = ROOT / 'android/app/build/outputs/apk/release/app-release.apk'
    return apk, apk.with_suffix('.verification.json')


def android_report(apk, receipt):
    report, receipt_sha = document(receipt); value, size = hashed(apk)
    require(report.get('sha256') == value and report.get('size_bytes') == size
            and report.get('signer_sha256') == ANDROID_SIGNER and report.get('debuggable') is False
            and report.get('abi') == ['arm64-v8a'] and report.get('native_16kb_aligned') is True,
            'APK and existing signing receipt do not agree; refusing overwrite/publication')
    return report, receipt_sha


def verify_existing_android_signer(apk):
    sdk = Path(os.environ.get('ANDROID_SDK_ROOT') or os.environ.get('ANDROID_HOME') or Path.home() / 'Library/Android/sdk')
    tools = sorted((sdk / 'build-tools').glob('*/apksigner'),
                   key=lambda path: tuple(map(int, re.findall(r'\d+', path.parent.name))))
    require(bool(tools), 'Existing APK needs the Android SDK signer verifier before overwrite')
    result = subprocess.run([str(tools[-1]), 'verify', '--verbose', '--print-certs', str(apk)],
                            capture_output=True, text=True, timeout=60)
    fingerprint = re.search(r'Signer #1 certificate SHA-256 digest: ([0-9a-f]+)', result.stdout)
    require(result.returncode == 0 and fingerprint and fingerprint[1] == ANDROID_SIGNER
            and 'Android Debug' not in result.stdout, 'Existing APK signature verification failed; preserving it')


def preserve_existing_android(state):
    apk, receipt = android_files()
    if not apk.exists() and not apk.is_symlink() and not receipt.exists() and not receipt.is_symlink():
        return None
    require(state.get('deliveries_complete') is True,
            'Existing APK delivery coverage is unknown; refusing overwrite')
    report, receipt_sha = android_report(apk, receipt)
    verify_existing_android_signer(apk)
    known = [item for item in state.get('android_deliveries', []) if item['validation_digest'] == receipt_sha]
    if known:
        require(len(known) == 1, 'Ambiguous existing APK delivery identity')
        require(known[0].get('archived') is False,
                'Existing APK belongs to an archived delivery; preserve it and use public recovery before overwrite')
        # Re-publish idempotently verifies the immutable snapshot before overwrite.
        return publish(state, 'android-arm64-release', [apk, receipt], known[0]['validation_kind'], receipt,
                       [report['sha256'], receipt_sha], snapshot=True, selected=known[0]['generation'])
    # Legacy bootstrap preserves exactly observed signing evidence, not source/install identity.
    require(sum(regular(path).stat().st_size for path in (apk, receipt)) <= MAX_SNAPSHOT,
            'Exact release group exceeds snapshot budget')
    admit(state, 'android-arm64-release', [apk, receipt], snapshot=True, selected='legacy-' + report['sha256'])
    return publish(state, 'android-arm64-release', [apk, receipt], 'existing-signing-evidence', receipt,
                   [report['sha256'], receipt_sha], snapshot=True, selected='legacy-' + report['sha256'])


def publish_android(state):
    apk, receipt = android_files()
    report, receipt_sha = android_report(apk, receipt)
    return publish(state, 'android-arm64-release', [apk, receipt], 'signing', receipt,
                   [report['sha256'], receipt_sha], snapshot=True, defer_failure=True)


def run(kind, arguments):
    if kind == 'macos':
        # Preserve signing preflight before any cleanup can touch an older package.
        check = subprocess.run([sys.executable, '-B', str(ROOT / 'scripts/macos_release_guard.py'), 'preflight'])
        require(check.returncode == 0, 'Signing preflight failed before release lifecycle changes')
    with producer(kind) as (fd, state):
        if kind == 'android-release':
            preserve_existing_android(state)
            admit(state, 'android-arm64-release', android_files(), snapshot=True)
        env = dict(os.environ, **{LOCK_ENV: str(fd)})
        command = ['bash', str(ROOT / 'scripts' / ENTRIES[kind]), *arguments]
        child = subprocess.Popen(command, env=env, pass_fds=(fd,), start_new_session=True)
        try:
            return child.wait()
        except BaseException:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL); child.wait()
            raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    command = commands.add_parser('run'); command.add_argument('kind', choices=ENTRIES)
    command.add_argument('arguments', nargs=argparse.REMAINDER)
    command = commands.add_parser('check-lock'); command.add_argument('kind', choices=ENTRIES)
    commands.add_parser('publish-android')
    args = parser.parse_args()
    if args.action == 'run':
        return run(args.kind, args.arguments)
    if args.action == 'check-lock':
        check_lock(args.kind)
    else:
        _, state = check_lock('android-release')
        print(json.dumps(publish_android(state)))
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (Refusal, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f'RELEASE_LIFECYCLE_REJECTED: {error}', file=sys.stderr)
        sys.exit(2)
