#!/usr/bin/env python3
"""Stable macOS release identity. Uses Apple's verifier, never reads or edits TCC.

A public certificate fingerprint is policy, not a private signing credential.
Changing that policy is a reviewed migration, not an environment override.
"""
from __future__ import annotations
import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile
import build_identity_gate as identity_gate

ROOT = Path(__file__).resolve().parent.parent
POLICY_FILE = ROOT / 'deploy/macos/signing-policy.json'


class ReleaseRejected(RuntimeError):
    pass


@dataclass(frozen=True)
class Policy:
    bundle_id: str
    executable: str
    app_name: str
    certificate_sha1: str

    @property
    def requirement(self) -> str:
        return f'identifier "{self.bundle_id}" and certificate leaf = H"{self.certificate_sha1}"'


def load_policy(path: Path = POLICY_FILE) -> Policy:
    data = json.loads(path.read_text())
    if data.get('schema') != 1:
        raise ReleaseRejected('Unsupported signing policy schema')
    policy = Policy(**{key: data[key] for key in Policy.__dataclass_fields__})
    if not re.fullmatch(r'[a-zA-Z0-9.-]+', policy.bundle_id):
        raise ReleaseRejected('Invalid bundle identifier')
    if not re.fullmatch(r'[0-9a-f]{40}', policy.certificate_sha1):
        raise ReleaseRejected('Expected a pinned public certificate fingerprint')
    if policy.executable != 'remote_play' or policy.app_name != 'RemotePlay':
        raise ReleaseRejected('Unexpected product name or executable')
    return policy


def run(args: list[str], timeout: int = 45) -> str:
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ReleaseRejected(f'{args[0]} could not complete: {error}') from error
    if result.returncode:
        detail = (result.stdout + result.stderr)[-3000:]
        raise ReleaseRejected(f'{args[0]} rejected the operation ({result.returncode}): {detail}')
    return result.stdout + result.stderr


def sha256(path: Path, *, check=None) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            if check: check()
            digest.update(block)
    return digest.hexdigest()


def preflight(policy: Policy) -> str:
    identities = run(['/usr/bin/security', 'find-identity', '-v', '-p', 'codesigning'])
    fingerprints = re.findall(r'^\s*\d+\)\s+([0-9A-Fa-f]{40})\s+"', identities, re.M)
    if policy.certificate_sha1 not in {fingerprint.lower() for fingerprint in fingerprints}:
        raise ReleaseRejected('The pinned release signing identity is unavailable. Build the binary here, but sign on the existing authorized signing Mac. No ad-hoc fallback and no new certificate will be created.')
    return policy.certificate_sha1.upper()


def designated_requirement(text: str) -> str:
    match = re.search(r'^(?:#\s*)?designated\s*=>\s*(.+)$', text, re.M)
    if not match:
        raise ReleaseRejected('No designated requirement in the code signature')
    return ' '.join(match[1].split())


def version_key(text: str) -> tuple[int, ...]:
    match = re.fullmatch(r'(\d+)\.(\d+)\.(\d+)(?:-alpha\.(\d+))?', text)
    if not match:
        raise ReleaseRejected(f'Unsupported release version: {text}')
    major, minor, patch, alpha = match.groups()
    return int(major), int(minor), int(patch), int(alpha is None), int(alpha or 0)


def build_key(text: str) -> tuple[int, ...]:
    if not re.fullmatch(r'\d+(?:\.\d+){0,2}', text):
        raise ReleaseRejected(f'Invalid macOS build number: {text}')
    numbers = tuple(map(int, text.split('.')))
    return numbers + (0,) * (3 - len(numbers))


def verify_app(app: Path, policy: Policy, *, runner=None, check=None) -> dict:
    execute = runner or run
    if check: check()
    if app.is_symlink() or not app.is_dir() or app.name != policy.app_name + '.app':
        raise ReleaseRejected('Expected a real RemotePlay.app directory, not a symlink or other product')
    info_path = app / 'Contents/Info.plist'
    executable = app / 'Contents/MacOS' / policy.executable
    if info_path.is_symlink() or executable.is_symlink() or not executable.is_file():
        raise ReleaseRejected('Missing or linked application metadata/executable')
    try:
        info = plistlib.loads(info_path.read_bytes())
    except (OSError, plistlib.InvalidFileException) as error:
        raise ReleaseRejected(f'Invalid Info.plist: {error}') from error
    if info.get('CFBundleIdentifier') != policy.bundle_id or info.get('CFBundleExecutable') != policy.executable:
        raise ReleaseRejected('Bundle identity or executable is not the authorized RemotePlay product')
    version = str(info.get('RemotePlayReleaseVersion', ''))
    version_key(version)
    build = str(info.get('CFBundleVersion', ''))
    build_key(build)
    if info.get('CFBundleShortVersionString') != version.split('-')[0]:
        raise ReleaseRejected('Release version and bundle version disagree')
    if not str(info.get('NSScreenCaptureUsageDescription', '')).strip():
        raise ReleaseRejected('Missing screen-capture usage description')
    resource_version = app / 'Contents/Resources/VERSION'
    if resource_version.exists() and resource_version.read_text().strip() != version:
        raise ReleaseRejected('Embedded version does not match Info.plist')
    # A correct bundle ID alone is insufficient: enforce the exact authorized signer.
    execute(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(app)])
    execute(['/usr/bin/codesign', '--verify', '--strict', '-R', '=' + policy.requirement, str(app)])
    requirement = designated_requirement(execute(['/usr/bin/codesign', '-d', '-r-', str(app)]))
    if requirement != ' '.join(policy.requirement.split()):
        raise ReleaseRejected('Non-stable designated requirement: release identity must not depend on a build hash or a broader signer rule')
    return {'version': version, 'build': build, 'bundle_id': policy.bundle_id,
            'certificate_sha1': policy.certificate_sha1, 'designated_requirement': requirement,
            'executable_sha256': sha256(executable, check=check), 'info_sha256': sha256(info_path, check=check),
            'signature_verified': True, 'notarization_verified': False}


def check_upgrade(previous: dict, candidate: dict) -> str:
    for key in ('bundle_id', 'certificate_sha1', 'designated_requirement'):
        if previous[key] != candidate[key]:
            raise ReleaseRejected(f'Upgrade would change authorized identity: {key}')
    if version_key(candidate['version']) < version_key(previous['version']):
        raise ReleaseRejected('Release downgrade rejected')
    if build_key(candidate['build']) < build_key(previous['build']):
        raise ReleaseRejected('Build number rollback rejected')
    if previous['version'] == candidate['version'] and previous['build'] == candidate['build']:
        if previous['executable_sha256'] != candidate['executable_sha256'] or previous['info_sha256'] != candidate['info_sha256']:
            raise ReleaseRejected('Same version/build contains different bytes; assign a new build instead of silently replacing it')
        return 'already_installed'
    return 'compatible_upgrade'


@contextmanager
def archive_workspace(owned: Path | None):
    if owned is None:
        # Standalone read-only verification keeps its existing temporary lifetime.
        with tempfile.TemporaryDirectory(prefix='rp-release-check-') as directory:
            yield Path(directory)
        return
    owned = Path(owned)
    if (not owned.is_dir() or any(path.is_symlink() for path in [owned, *owned.parents]) or
            owned.stat().st_uid != os.getuid()):
        raise ReleaseRejected('Archive verification requires the public-owned packaging workspace')
    root = owned / '.archive-check'
    try:
        root.mkdir(mode=0o700)  # Existing work is recovered by the original public aux transaction.
    except FileExistsError as error:
        raise ReleaseRejected('Archive verification workspace already exists; recover the current transaction first') from error
    identity = root.stat()
    try:
        yield root
    finally:
        current = root.lstat()
        if (not stat.S_ISDIR(current.st_mode) or
                (current.st_dev, current.st_ino) != (identity.st_dev, identity.st_ino)):
            raise ReleaseRejected('Archive verification workspace changed; preserve it for recovery')
        shutil.rmtree(root)


@contextmanager
def verified_archive(archive: Path, policy: Policy, *, runner=None, check=None, owned_workspace=None,
                     identity_expectations=None):
    manifest_name = None
    pinned_archive_sha = None
    if identity_expectations is not None:
        import build_identity_gate as gate
        pinned_archive_sha = gate.verify_package(archive, identity_expectations['manifest_sha256'],
                            identity_expectations['commit'], identity_expectations['identity_sha256'])['archive_sha256']
        manifest_name = gate.MANIFEST
    # Preflight the whole archive before extracting; no path traversal, links, or zip bombs.
    with zipfile.ZipFile(archive) as source, archive_workspace(owned_workspace) as root:
        entries = source.infolist()
        if len(entries) > 20000 or sum(entry.file_size for entry in entries) > 1024 ** 3:
            raise ReleaseRejected('Archive exceeds release size limits')
        seen = set()
        for entry in entries:
            if check: check()
            path = PurePosixPath(entry.filename)
            if path.is_absolute() or '..' in path.parts or '\\' in entry.filename or not path.parts:
                raise ReleaseRejected('Unsafe path in release archive')
            folded = str(path).casefold()
            if folded in seen:
                raise ReleaseRejected('Duplicate or case-colliding archive entry')
            seen.add(folded)
            mode = entry.external_attr >> 16
            if stat.S_IFMT(mode) not in (0, stat.S_IFDIR, stat.S_IFREG):
                raise ReleaseRejected('Symlink or special file in release archive')
            if entry.flag_bits & 1:
                raise ReleaseRejected('Encrypted release archives are not supported')
            if str(path) != manifest_name and path.parts[0] not in (policy.app_name + '.app', '__MACOSX'):
                raise ReleaseRejected('Unexpected top-level archive content')
        for entry in entries:
            if check: check()
            path = PurePosixPath(entry.filename)
            if path.parts[0] == '__MACOSX' or str(path) == manifest_name:
                continue
            target = root.joinpath(*path.parts)
            if entry.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.open(entry) as incoming, target.open('xb') as output:
                    for block in iter(lambda: incoming.read(1024 * 1024), b''):
                        if check: check()
                        output.write(block)
                permissions = (entry.external_attr >> 16) & 0o777
                os.chmod(target, permissions or 0o644)
        app = root / (policy.app_name + '.app')
        report = verify_app(app, policy, runner=runner, check=check)
        report.update(archive_sha256=sha256(archive, check=check), size_bytes=archive.stat().st_size)
        if pinned_archive_sha is not None and report['archive_sha256'] != pinned_archive_sha:
            raise ReleaseRejected('Identity-bound archive changed during signature verification')
        yield app, report


def verify_stage(stage: Path, policy: Policy, *, macos_only: bool = False) -> list[dict]:
    metadata = json.loads((stage / 'desktop-releases.json').read_text())
    verified = []
    for entry in metadata.get('releases', []):
        name = entry['file']
        if Path(name).name != name or not name:
            raise ReleaseRejected('Invalid artifact name')
        if macos_only and 'macos' not in name.lower():
            continue
        if macos_only and not entry.get('build_identity_expectations'):
            raise ReleaseRejected('macOS update requires verified source identity pins')
        path = stage / name
        if path.is_symlink() or not path.is_file():
            raise ReleaseRejected('Artifact missing or linked')
        if sha256(path) != entry['sha256'] or path.stat().st_size != entry['size_bytes']:
            raise ReleaseRejected('Artifact bytes do not match release metadata')
        if 'macos' in name.lower():
            with verified_archive(path, policy,
                                  identity_expectations=entry.get('build_identity_expectations')) as (_, report):
                if report['version'] != entry['version']:
                    raise ReleaseRejected('macOS artifact version disagrees with publication metadata')
                verified.append(report)
    if len(verified) != 1:
        raise ReleaseRejected('Publication must contain exactly one verified macOS release')
    return verified


@contextmanager
def packaging_workspace(output: Path, owned: Path | None):
    if owned is not None:
        if owned.is_symlink() or not owned.is_dir() or any(owned.iterdir()):
            raise ReleaseRejected('Expected the empty public-owned packaging workspace')
        yield owned  # The shared transaction owns its cleanup, including crash recovery.
        return
    # Direct function callers are isolated acceptance fixtures, not the production CLI.
    stage = output / '.remoteplay-macos-package.pending'
    stage.mkdir(mode=0o700)
    try:
        yield stage
    finally:
        shutil.rmtree(stage)


def package(binary: Path, output: Path, version: str, build: str, policy: Policy, *,
            archive: Path | None = None, receipt: Path | None = None,
            workspace: Path | None = None, current_archive: Path | None = None,
            observed_build: dict | None = None, expected_commit: str | None = None,
            expected_identity: str | None = None) -> dict:
    signer = preflight(policy)  # Check before altering any existing output.
    version_key(version); build_key(build)
    if not binary.is_file() or binary.is_symlink():
        raise ReleaseRejected('Missing build output')
    output.mkdir(parents=True, exist_ok=True)
    archive = archive or output / 'RemotePlay-macos-arm64.zip'
    receipt = receipt or archive.with_suffix('.verification.json')
    if archive.exists() or archive.is_symlink() or receipt.exists() or receipt.is_symlink():
        raise ReleaseRejected('Output already exists; update current through the production entrypoint')
    with packaging_workspace(output, workspace) as stage:
        app = stage / 'RemotePlay.app'
        macos = app / 'Contents/MacOS'; resources = app / 'Contents/Resources'
        macos.mkdir(parents=True); resources.mkdir(parents=True)
        shutil.copy2(binary, macos / policy.executable); os.chmod(macos / policy.executable, 0o755)
        info = {'CFBundleExecutable': policy.executable, 'CFBundleIdentifier': policy.bundle_id,
                'CFBundleName': policy.app_name, 'CFBundleDisplayName': policy.app_name,
                'CFBundleShortVersionString': version.split('-')[0], 'CFBundleVersion': build,
                'RemotePlayReleaseVersion': version, 'CFBundlePackageType': 'APPL',
                'LSMinimumSystemVersion': '13.0', 'NSHighResolutionCapable': True,
                'NSMicrophoneUsageDescription': 'RemotePlay uses the microphone when talkback is enabled.',
                'NSScreenCaptureUsageDescription': 'RemotePlay captures your screen only for authorized remote viewing.'}
        (app / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
        (resources / 'VERSION').write_text(version + '\n')
        (resources / 'release-identity.json').write_text(json.dumps({
            'schema': 1, 'version': version, 'build': build, 'bundle_id': policy.bundle_id,
            'certificate_sha1': policy.certificate_sha1, 'designated_requirement': policy.requirement,
        }, indent=2) + '\n')
        # No mutation of signed bundle contents is permitted after this point.
        run(['/usr/bin/codesign', '--force', '--sign', signer, '--timestamp=none',
             '--identifier', policy.bundle_id, '-r', '=designated => ' + policy.requirement, str(app)])
        report = verify_app(app, policy)
        identity_expectations = None
        if observed_build is not None:
            import build_identity_gate as gate
            sealed = gate.seal_tree(stage, observed_build, expected_commit, expected_identity,
                                    compiler_binary=binary)
            identity_expectations = {'commit': expected_commit, 'identity_sha256': expected_identity,
                                     'manifest_sha256': sealed['manifest_sha256']}
        candidate = stage / 'candidate.zip'
        if identity_expectations is not None:
            # The identity inventory includes exact regular payload files;
            # Finder/resource sidecars are not unlisted release payloads.
            with zipfile.ZipFile(candidate, 'x', compression=zipfile.ZIP_DEFLATED) as output_zip:
                for payload in sorted(app.rglob('*')):
                    if payload.is_file():
                        output_zip.write(payload, payload.relative_to(stage).as_posix())
                output_zip.write(stage / gate.MANIFEST, gate.MANIFEST)
        else:
            run(['/usr/bin/ditto', '-c', '-k', '--sequesterRsrc', '--keepParent', str(app), str(candidate)], 120)
        with verified_archive(candidate, policy, owned_workspace=stage,
                              identity_expectations=identity_expectations) as (_, extracted):
            check_upgrade(report, extracted)
        if identity_expectations is not None:
            report['build_identity_expectations'] = identity_expectations
        # This is the transaction's private stage; shared publish atomically updates current.
        os.replace(candidate, archive)
        report.update(archive=str(current_archive or archive), archive_sha256=sha256(archive), size_bytes=archive.stat().st_size)
        with receipt.open('x') as outgoing:
            json.dump(report, outgoing, indent=2); outgoing.write('\n'); outgoing.flush(); os.fsync(outgoing.fileno())
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    sub.add_parser('preflight')
    for name in ['verify-app', 'verify-archive', 'verify-stage']:
        child = sub.add_parser(name); child.add_argument('path', type=Path)
        if name == 'verify-stage': child.add_argument('--macos-only', action='store_true')
        if name == 'verify-archive':
            child.add_argument('--expected-commit')
            child.add_argument('--expected-identity-sha256')
            child.add_argument('--expected-manifest-sha256')
    child = sub.add_parser('package')
    child.add_argument('--binary', type=Path, required=True); child.add_argument('--output', type=Path, required=True)
    child.add_argument('--version', required=True); child.add_argument('--build', required=True)
    child.add_argument('--build-receipt', type=Path)
    child.add_argument('--build-receipt-sha256')
    child.add_argument('--expected-commit')
    child.add_argument('--expected-identity-sha256')
    args = parser.parse_args(); policy = load_policy()
    if args.command == 'preflight': result = {'signing_identity': preflight(policy), 'ad_hoc_fallback': False}
    elif args.command == 'verify-app': result = verify_app(args.path, policy)
    elif args.command == 'verify-archive':
        values = (args.expected_commit, args.expected_identity_sha256, args.expected_manifest_sha256)
        if any(values) and not all(values):
            raise ReleaseRejected('All independent archive identity expectations are required together')
        pins = dict(zip(('commit', 'identity_sha256', 'manifest_sha256'), values)) if all(values) else None
        with verified_archive(args.path, policy, identity_expectations=pins) as (_, result): pass
    elif args.command == 'verify-stage': result = verify_stage(args.path, policy, macos_only=args.macos_only)
    else:
        preflight(policy)  # Preserve signer refusal before any retention mutation.
        import build_identity_gate as gate
        identity_args = (args.build_receipt, args.build_receipt_sha256,
                         args.expected_commit, args.expected_identity_sha256)
        if not all(identity_args):
            raise ReleaseRejected('Production packaging requires an independently pinned observed build receipt')
        observed = gate.read_json(args.build_receipt, args.build_receipt_sha256)
        gate.validate_identity(observed.get('build_identity'), args.expected_commit, args.expected_identity_sha256)
        import release_artifact_lifecycle as lifecycle
        with lifecycle.producer('macos') as (_, delivery):
            version_key(args.version); build_key(args.build)
            if not args.binary.is_file() or args.binary.is_symlink():
                raise ReleaseRejected('Missing build output')
            output = args.output.resolve()
            lifecycle.require(output == lifecycle.macos_output_root(), 'Use the configured default macOS package directory')
            archive = output / 'RemotePlay-macos-arm64.zip'
            files = [archive, archive.with_suffix('.verification.json')]
            current = None
            if any(path.exists() or path.is_symlink() for path in files):
                recorded, current = lifecycle.macos_evidence(*files, archive)
                with verified_archive(archive, policy,
                                      identity_expectations=recorded.get('build_identity_expectations')) as (_, checked):
                    lifecycle.require(all(recorded.get(key) == value for key, value in checked.items()),
                                      'Existing current archive differs from its original verified receipt')
            admission = lifecycle.admit(delivery, 'macos-arm64-release', files, current=current,
                                        aux_stage=output / '.remoteplay-macos-package.pending')
            staged_archive, staged_receipt = map(Path, admission['staged_files'])
            result = package(args.binary, output, args.version, args.build, policy,
                             archive=staged_archive, receipt=staged_receipt,
                             workspace=Path(admission['owned_aux_stages'][0]), current_archive=archive,
                             observed_build=observed, expected_commit=args.expected_commit,
                             expected_identity=args.expected_identity_sha256)
            current_output = lifecycle.publish_macos(delivery, admission, result)
            result = dict(result, current_output=current_output)
    print('MACOS_RELEASE_VERIFIED ' + json.dumps(result), flush=True)


if __name__ == '__main__':
    try:
        main()
    except (ReleaseRejected, identity_gate.IdentityRejected, ValueError, KeyError, OSError, zipfile.BadZipFile) as error:
        print(f'MACOS_RELEASE_REJECTED: {error}', file=sys.stderr); sys.exit(1)
