"""Installer-owned package/backend adapter for the reviewed build identity gate.

The three release expectations and native-observer digest must come from the
independent handoff. Nothing is inferred from a candidate's own claims. The
frontend gate is deliberately not called with fabricated --gui arguments.
"""
from contextlib import contextmanager
from dataclasses import asdict, dataclass
import base64
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile
import zipfile

import build_identity_gate as gate
from macos_release_guard import ReleaseRejected, verified_archive

STATE_NAME = '.remoteplay-release-identity.json'
MAX_STATE = 4 * 1024 ** 2


@dataclass(frozen=True)
class Expectations:
    commit: str
    identity_sha256: str
    manifest_sha256: str
    observer: str
    observer_sha256: str

    def validate(self):
        gate.sha(self.commit, 40)
        for value in (self.identity_sha256, self.manifest_sha256, self.observer_sha256): gate.sha(value)
        observer = Path(self.observer)
        gate.require(observer.is_absolute() and observer == observer.resolve(strict=True),
                     'An independently verified canonical native observer is required')
        gate.require(gate.hash_file(observer) == self.observer_sha256, 'Native observer differs from independent expectation')
        return self


def requested(commit=None, identity_sha256=None, manifest_sha256=None, observer=None, observer_sha256=None):
    values = (commit, identity_sha256, manifest_sha256, observer, observer_sha256)
    if not any(value is not None for value in values): return None
    if any(value is None for value in values):
        raise ReleaseRejected('All independent release expectations and native observer fields are required together')
    return Expectations(commit, identity_sha256, manifest_sha256, str(observer), observer_sha256).validate()


def require_context(expectations, state_path: Path, *, separate_background: bool, isolated_fixture: bool, apply: bool):
    if expectations is not None: expectations.validate()
    if expectations is None and (state_path.exists() or state_path.is_symlink()
                                 or ((separate_background or apply) and not isolated_fixture)):
        raise ReleaseRejected('Independent build-identity expectations are required; no legacy fallback for production apply, migration or an identity-bound installation')


def private_bytes(path: Path) -> bytes:
    gate.regular(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as source:
        info = os.fstat(source.fileno())
        gate.require(info.st_uid == os.getuid() and info.st_nlink == 1
                     and stat.S_IMODE(info.st_mode) == 0o600
                     and info.st_ino == path.lstat().st_ino and info.st_size <= MAX_STATE,
                     'Installed release state is not private and stable')
        raw = source.read(MAX_STATE + 1)
        gate.require(len(raw) <= MAX_STATE and gate.hash_file(path) == gate.digest(raw),
                     'Installed release state changed while reading')
    return raw


def contract(raw_manifest: bytes, expectations: Expectations) -> dict:
    expectations.validate()
    manifest = gate.manifest_document(raw_manifest, expectations.manifest_sha256,
                                      expectations.commit, expectations.identity_sha256)
    gate.require(manifest['build_identity']['configuration']['platform'] == 'macos',
                 'Installer requires a macOS release manifest')
    return {'expectations': expectations, 'raw_manifest': raw_manifest, 'manifest': manifest}


def state_bytes(value: dict) -> bytes:
    raw = json.dumps({'schema': 1, 'kind': 'remoteplay-installed-release',
                      'expectations': asdict(value['expectations']),
                      'manifest_base64': base64.b64encode(value['raw_manifest']).decode('ascii')},
                     sort_keys=True, indent=2).encode() + b'\n'
    gate.require(len(raw) <= MAX_STATE, 'Installed release state exceeds bounds')
    return raw


def parse_state(raw: bytes) -> dict:
    gate.require(len(raw) <= MAX_STATE, 'Installed release state exceeds bounds')
    def unique(pairs):
        result = {}
        for key, value in pairs:
            gate.require(key not in result, 'Duplicate installed release-state key'); result[key] = value
        return result
    data = json.loads(raw, object_pairs_hook=unique)
    gate.require(type(data) is dict and set(data) == {'schema', 'kind', 'expectations', 'manifest_base64'}
                 and type(data['schema']) is int and data['schema'] == 1
                 and data['kind'] == 'remoteplay-installed-release'
                 and type(data['expectations']) is dict
                 and set(data['expectations']) == set(Expectations.__dataclass_fields__),
                 'Invalid installed release-state contract')
    raw_manifest = base64.b64decode(data['manifest_base64'], validate=True)
    return contract(raw_manifest, Expectations(**data['expectations']))


def read_state(path: Path):
    if not path.exists() and not path.is_symlink(): return None
    return parse_state(private_bytes(path))


def write_new(path: Path, raw: bytes):
    gate.require(len(raw) <= MAX_STATE, 'Installed release state exceeds bounds')
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as output:
        output.write(raw); output.flush(); os.fsync(output.fileno())


def replace_state(path: Path, raw: bytes | None, expected_sha256: str | None):
    current = gate.digest(private_bytes(path)) if path.exists() or path.is_symlink() else None
    gate.require(current == expected_sha256, 'Release state changed outside transaction; refusing replacement')
    if raw is None:
        if current is not None: path.unlink()
    else:
        parse_state(raw)
        fd, name = tempfile.mkstemp(prefix='.remoteplay-identity-', dir=path.parent)
        try:
            with os.fdopen(fd, 'wb') as output:
                output.write(raw); output.flush(); os.fsync(output.fileno())
            actual = gate.digest(private_bytes(path)) if path.exists() or path.is_symlink() else None
            gate.require(actual == expected_sha256, 'Release state changed before replacement')
            os.replace(name, path)
        finally:
            if os.path.exists(name): os.unlink(name)
    fd = os.open(path.parent, os.O_RDONLY)
    try: os.fsync(fd)
    finally: os.close(fd)


@contextmanager
def verified_identity_archive(archive: Path, policy, expectations: Expectations, *, runner, check):
    """Verify the complete original archive, then use the unchanged signing guard.

    Only the independently anchored external manifest is omitted from a bounded
    temporary App-only archive view. Payload bytes/modes are preserved and the
    full extracted App is checked again against the original manifest.
    """
    expectations.validate(); check()
    package = gate.verify_package(archive, expectations.manifest_sha256,
                                  expectations.commit, expectations.identity_sha256)
    with tempfile.TemporaryDirectory(prefix='rp-installer-identity-') as directory:
        view = Path(directory).resolve() / 'signed-app-view.zip'
        with zipfile.ZipFile(archive) as source, zipfile.ZipFile(view, 'x') as output:
            raw_manifest = source.read(gate.MANIFEST)
            value = contract(raw_manifest, expectations)
            for entry in source.infolist():
                check()
                if entry.filename == gate.MANIFEST: continue
                with source.open(entry) as incoming, output.open(entry, 'w') as outgoing:
                    while block := incoming.read(1024 * 1024):
                        check(); outgoing.write(block)
        gate.require(gate.hash_file(archive) == package['archive_sha256'], 'Original release archive changed')
        with verified_archive(view, policy, runner=runner, check=check) as (app, report):
            app = app.resolve(strict=True)
            verify_app_contract(app, value, report)
            report.update(archive_sha256=package['archive_sha256'], size_bytes=archive.stat().st_size)
            yield app, report, value


def verify_app_contract(app: Path, value: dict, signed_info: dict) -> dict:
    m = value['manifest']; expectations = value['expectations']; expectations.validate()
    gate.require(signed_info.get('signature_verified') is True
                 and signed_info['executable_sha256'] == m['binary_sha256']
                 and signed_info['version'] == m['build_identity']['configuration']['version'],
                 'Native signed App differs from release identity')
    gate.require(gate.files_in(app) == sorted(m['files'], key=lambda item: item['path']),
                 'Installed App differs from independently pinned manifest')
    info = gate.product_info(app / m['executable'], m['build_identity'], m['binary_sha256'])
    gate.require(info == m['product_info'], 'Compiled product identity differs from sealed manifest')
    return {'compiled_build_identity_verified': True, 'installed_manifest_inventory_verified': True,
            'source_commit': expectations.commit, 'build_identity_sha256': expectations.identity_sha256,
            'manifest_sha256': expectations.manifest_sha256, 'release_authorized': False}


def verify_backend(app: Path, value: dict, state: dict, plist: dict, label: str, authenticated: dict) -> dict:
    executable = app / value['manifest']['executable']; arguments = [str(executable), '--background-service']
    gate.require(state.get('loaded') is True and state.get('running') is True
                 and type(state.get('pid')) is int and state['pid'] > 0
                 and state.get('program') == str(executable) and state.get('arguments') == arguments,
                 'Actual launchd backend executable/arguments evidence is missing or mismatched')
    gate.require(plist.get('Label') == label and plist.get('ProgramArguments') == arguments,
                 'Current launch-agent backend role differs from actual job')
    gate.require(authenticated.get('background_role_verified') is True
                 and authenticated.get('pid') == state['pid']
                 and authenticated.get('executable_sha256') == value['manifest']['binary_sha256'],
                 'Authenticated lifecycle owner differs from launchd backend')
    expectations = value['expectations']; expectations.validate()
    native = gate.verify_running_macos(state['pid'], executable, value['manifest']['binary_sha256'],
                                       Path(expectations.observer), expectations.observer_sha256)
    gate.require(native.get('actual_loaded_image_verified') is True, 'Actual native backend image is unverified')
    return dict(native, backend_entry_verified=True, authenticated_backend_owner_verified=True,
                frontend_running_image_verified=False, release_authorized=False)
