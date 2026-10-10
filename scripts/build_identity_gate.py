#!/usr/bin/env python3
"""Fail-closed source/build/package/install/launch identity checks.

No GUI, service management, permission edits, signing-key access or deployment.
Digests are integrity bindings, not independent proof or release authorization.
Package expectations must come from a separately verified builder/signer receipt.
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import zipfile

from verify_desktop_gui import GuiReleaseRejected, validate as validate_gui

MANIFEST = 'remoteplay-release-manifest.json'
RENDERER = 'restored-original-gpui'
LIMIT = 1024 ** 3
MAX_FILES = 20000
LAYOUTS = {
    'macos': ('RemotePlay.app', 'Contents/MacOS/remote_play'),
    'linux': ('RemotePlay', 'remote_play'),
    'windows': ('RemotePlay', 'remote_play.exe'),
}
FEATURES = {'macos': ['gpui-restoration'],
            'linux': ['gpui-restoration', 'native-linux-video'],
            'windows': ['gpui-restoration', 'native-windows-video']}
BUILD_ENV = ('PATH', 'HOME', 'TMPDIR', 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN', 'SDKROOT',
             'MACOSX_DEPLOYMENT_TARGET', 'CC', 'CXX', 'AR', 'CPATH', 'LIBRARY_PATH',
             'PKG_CONFIG_PATH', 'PKG_CONFIG_LIBDIR', 'RP_VERIFIED_FFMPEG_INCLUDE',
             'SYSTEMROOT', 'WINDIR', 'TEMP', 'TMP', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA',
             'INCLUDE', 'LIB', 'LIBPATH')


class IdentityRejected(RuntimeError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise IdentityRejected(message)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def canonical(data) -> bytes:
    return json.dumps(data, sort_keys=True, separators=(',', ':'),
                      ensure_ascii=True, allow_nan=False).encode()


def sha(value: str, length: int = 64) -> str:
    require(type(value) is str and bool(re.fullmatch(r'[0-9a-f]{' + str(length) + '}', value)),
            'Exact lowercase digest is required')
    return value


def relative(name: str) -> PurePosixPath:
    require(type(name) is str and name and '\\' not in name and ':' not in name,
            'Unsafe relative artifact path')
    p = PurePosixPath(name)
    require(not p.is_absolute() and p.as_posix() == name and
            all(part not in ('.', '..') and not part.endswith(('.', ' '))
                for part in p.parts), 'Noncanonical artifact path')
    require(all(not re.fullmatch(r'(?i)(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\..*)?', part)
                for part in p.parts), 'Nonportable artifact path')
    require(not any(ord(c) < 32 for c in name), 'Control character in artifact path')
    return p


def regular(path: Path) -> Path:
    require(path.is_absolute(), 'An absolute file path is required')
    # No symlink in any component, including an enclosing app/target directory.
    for p in (path, *path.parents):
        require(not p.is_symlink(), 'Linked path is not accepted')
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode), 'Expected a regular file')
    return path


def hash_file(path: Path) -> str:
    regular(path)
    before = path.stat()
    with path.open('rb') as incoming:
        value = hashlib.file_digest(incoming, 'sha256').hexdigest()
        opened = os.fstat(incoming.fileno())
    after = path.stat()
    keys = ('st_dev', 'st_ino', 'st_size', 'st_mtime_ns', 'st_ctime_ns')
    require(all(getattr(before, k) == getattr(opened, k) == getattr(after, k) for k in keys),
            'Artifact changed while hashing')
    return value


def json_bytes(raw: bytes) -> dict:
    require(len(raw) <= 2 * 1024 ** 2, 'Identity document too large')
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, 'Duplicate JSON key')
            result[key] = value
        return result
    try:
        result = json.loads(raw, object_pairs_hook=unique,
                            parse_constant=lambda _: (_ for _ in ()).throw(IdentityRejected('Nonfinite JSON')))
    except (ValueError, UnicodeError) as error:
        raise IdentityRejected('Invalid identity JSON') from error
    require(type(result) is dict, 'Identity must be an object')
    return result


def read_json(path: Path, expected_sha: str | None = None) -> dict:
    regular(path)
    with path.open('rb') as incoming:
        raw = incoming.read(2 * 1024 ** 2 + 1)
    if expected_sha is not None:
        require(digest(raw) == sha(expected_sha), 'Document differs from the independent expectation')
    return json_bytes(raw)


def write_new(path: Path, data: dict) -> str:
    require(path.is_absolute() and path.parent.is_dir(), 'Existing absolute output directory required')
    for p in (path.parent, *path.parent.parents):
        require(not p.is_symlink(), 'Linked output directory rejected')
    raw = json.dumps(data, sort_keys=True, indent=2, allow_nan=False).encode() + b'\n'
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as out:
        out.write(raw); out.flush(); os.fsync(out.fileno())
    return digest(raw)


def git(repo: Path, *args: str) -> bytes:
    process = subprocess.run(['git', '--no-optional-locks', '-C', str(repo), *args], capture_output=True, timeout=30)
    require(process.returncode == 0, 'Git source identity could not be determined')
    return process.stdout


def source_snapshot(repo: Path, expected_commit: str, *, expected_patch: str | None = None,
                    inspect_only: bool = False) -> dict:
    sha(expected_commit, 40)
    repo = repo.absolute()
    require(repo.is_dir() and not repo.is_symlink(), 'Source root must be a real directory')
    require(git(repo, 'rev-parse', '--show-toplevel').decode().strip() == str(repo), 'Expected repository root')
    require(git(repo, 'rev-parse', 'HEAD').decode().strip() == expected_commit, 'Source commit mismatch')
    before = git(repo, 'status', '--porcelain=v1', '-uall', '-z')
    baseline = {}
    for row in git(repo, 'ls-tree', '-r', '-z', 'HEAD').split(b'\0'):
        if not row: continue
        fields, name = row.split(b'\t', 1)
        mode, kind, object_id = fields.decode().split()
        require(mode in ('100644', '100755') and kind == 'blob', 'Linked/submodule source unsupported')
        baseline[name.decode()] = (mode, object_id)
    untracked = [p.decode() for p in git(repo, 'ls-files', '--others', '--exclude-standard', '-z').split(b'\0') if p]
    require(len(baseline) + len(untracked) <= MAX_FILES, 'Source inventory too large')
    require(not untracked or expected_patch is not None or inspect_only,
            'Untracked source requires an exact approved patch identity')
    records, changes = [], []
    for name in sorted(set(baseline) | set(untracked)):
        p = relative(name)
        require(p.parts[0] not in ('.git', '.aws', '.codex', '.agents', 'target') and
                '__pycache__' not in p.parts and
                not name.lower().endswith(('.pem', '.key', '.p12', '.pfx', '.pyc')) and
                p.name not in ('mesh.conf', 'mesh.secret'), 'Private/generated material cannot enter source identity')
        path = repo.joinpath(*p.parts)
        if not path.exists():
            require(name in baseline, 'Missing untracked source')
            changes.append({'path': name, 'deleted': True})
            continue
        require(path.stat().st_size <= 32 * 1024 ** 2, 'Source file exceeds bounds')
        value = hash_file(path)
        mode = '100755' if path.stat().st_mode & 0o111 else '100644'
        record = {'path': name, 'mode': mode, 'sha256': value}
        records.append(record)
        if name not in baseline:
            changes.append(record)
        elif mode != baseline[name][0] or value != digest(git(repo, 'cat-file', 'blob', baseline[name][1])):
            changes.append(record)
    require(git(repo, 'status', '--porcelain=v1', '-uall', '-z') == before,
            'Source status changed while snapshotting')
    # Raw bytes are part of identity even when Git normalizes CRLF as clean.
    dirty = bool(before)
    patch = digest(canonical(changes)) if dirty else None
    if dirty and not inspect_only:
        require(expected_patch is not None and sha(expected_patch) == patch, 'Dirty source patch not explicitly approved')
    elif not dirty:
        require(expected_patch is None, 'Patch identity supplied for a clean checkout')
    return {'commit': expected_commit, 'git_tree': git(repo, 'rev-parse', 'HEAD^{tree}').decode().strip(),
            'state': 'patch' if dirty else 'clean', 'patch_sha256': patch,
            'snapshot_sha256': digest(canonical(records)), 'file_count': len(records), 'files': records}


def configuration(platform: str, architecture: str, version: str, target: str, profile: str,
                  toolchain_sha256: str, environment_sha256: str) -> dict:
    require(platform in LAYOUTS and architecture in ('x86_64', 'aarch64'), 'Unsupported build platform')
    require(bool(re.fullmatch(r'\d+\.\d+\.\d+(?:-alpha\.\d+)?', version)), 'Invalid product version')
    require(profile in ('dev', 'release'), 'Unsupported build profile')
    targets = {'macos': ('apple-darwin',), 'linux': ('unknown-linux-gnu',),
               'windows': ('pc-windows-msvc', 'pc-windows-gnu')}
    require(target in {architecture + '-' + suffix for suffix in targets[platform]}, 'Target/config mismatch')
    return {'platform': platform, 'architecture': architecture, 'version': version,
            'target': target, 'profile': profile, 'features': list(FEATURES[platform]),
            'default_features': False, 'locked': True, 'offline': True,
            'gui_entry': RENDERER, 'binary': 'remote_play', 'package': 'remote_play_app',
            'gui_arguments': ['--gui'] if platform == 'macos' else [],
            'toolchain_sha256': sha(toolchain_sha256),
            'environment_sha256': sha(environment_sha256)}


def build_environment() -> dict[str, str]:
    # Never collect token/password/key variables. Unknown Cargo/compiler overrides
    # are rejected rather than silently attributed to the reviewed configuration.
    unknown = [key for key in os.environ if key.startswith(('CARGO_', 'RUST', 'RP_')) and
               key not in BUILD_ENV and key not in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS')]
    require(not unknown and not os.environ.get('RUSTFLAGS') and
            not os.environ.get('CARGO_ENCODED_RUSTFLAGS'), 'Unrecorded build override rejected')
    return {key: os.environ[key] for key in BUILD_ENV if key in os.environ}


def environment_digest() -> str:
    return digest(canonical(build_environment()))


def identity(source: dict, config: dict) -> dict:
    summary = {k: source[k] for k in ('commit', 'git_tree', 'state', 'patch_sha256', 'snapshot_sha256', 'file_count')}
    sha(summary['commit'], 40); sha(summary['git_tree'], 40); sha(summary['snapshot_sha256'])
    require(summary['state'] in ('clean', 'patch'), 'Invalid source state')
    require(type(summary['file_count']) is int and summary['file_count'] > 0, 'Empty source inventory')
    if summary['state'] == 'clean': require(summary['patch_sha256'] is None, 'Clean source has a patch')
    else: sha(summary['patch_sha256'])
    expected = configuration(config['platform'], config['architecture'], config['version'],
                             config['target'], config['profile'], config['toolchain_sha256'],
                             config['environment_sha256'])
    require(canonical(config) == canonical(expected), 'Unapproved build configuration or GUI entry')
    body = {'schema': 1, 'source': summary, 'configuration': config,
            'configuration_sha256': digest(canonical(config))}
    return dict(body, identity_sha256=digest(canonical(body)))


def validate_identity(data: dict, expected_commit: str, expected_identity: str) -> dict:
    require(type(data) is dict and data.get('schema') == 1 and type(data.get('schema')) is int,
            'Missing compiled build identity')
    try:
        rebuilt = identity(data['source'], data['configuration'])
    except (KeyError, TypeError) as error:
        raise IdentityRejected('Incomplete build identity') from error
    require(data == rebuilt, 'Build identity fields or self-digest disagree')
    require(data['source']['commit'] == sha(expected_commit, 40) and
            data['identity_sha256'] == sha(expected_identity), 'Build differs from independent source/config expectation')
    return data


def validate_product_info(info: dict, expected: dict) -> None:
    config = expected['configuration']
    try:
        validate_gui(info, config['platform'], config['version'])
    except GuiReleaseRejected as error:
        raise IdentityRejected(str(error)) from error
    require(info['architecture'] == config['architecture'], 'Compiled architecture mismatch')
    if config['platform'] == 'macos':
        require(info.get('separated_lifecycle') is True and info.get('frontend_entry') == '--gui' and
                info.get('background_entry') == '--background-service', 'Compiled daily GUI/backend entry mismatch')
    require(info.get('build_identity') == expected, 'Binary lacks the expected compiled source/build identity')


def product_info(binary: Path, expected: dict, expected_binary_sha: str) -> dict:
    require(hash_file(binary) == sha(expected_binary_sha), 'Unexpected binary; metadata execution refused')
    process = subprocess.run([str(binary), '--product-info-json'], capture_output=True, timeout=15)
    require(process.returncode == 0, 'No-window product metadata failed')
    info = json_bytes(process.stdout)
    validate_product_info(info, expected)
    require(hash_file(binary) == expected_binary_sha, 'Binary changed during metadata query')
    return info


def authorized_mac_app(app: Path) -> dict:
    from macos_release_guard import load_policy, verify_app, ReleaseRejected
    try:
        return verify_app(app, load_policy())
    except ReleaseRejected as error:
        raise IdentityRejected('Native authorized macOS signature verification failed') from error


def toolchain_digest() -> str:
    outputs = []
    for cmd in (['rustc', '-vV'], ['cargo', '--version']):
        p = subprocess.run(cmd, capture_output=True, timeout=15)
        require(p.returncode == 0, 'Toolchain identity unavailable')
        outputs.append(p.stdout.decode())
    return digest(canonical(outputs))


def compiled_module(expected: dict) -> bytes:
    """Generated outside source; ProductInfo consumes only these immutable bytes."""
    raw = json.dumps(expected, sort_keys=True, indent=2).encode() + b'\n'
    require(len(raw) <= 65536, 'Compiled identity exceeds native bounds')
    text = ('#[used]\n'
            '#[cfg_attr(target_os = "windows", unsafe(link_section = ".rpbuild"))]\n'
            '#[cfg_attr(target_os = "macos", unsafe(link_section = "__TEXT,__rpbuild"))]\n'
            f'pub static BUILD_IDENTITY_BYTES: [u8; {len(raw)}] = [' +
            ','.join(str(b) for b in raw) + '];\n'
            'pub fn build_identity_bytes() -> &\'static [u8] { &BUILD_IDENTITY_BYTES }\n')
    return text.encode()


def observed_build(repo: Path, expected: dict, target: Path) -> dict:
    """Only this fixed build path may mint a build receipt; no loose-binary import.

    The app must compile REMOTEPLAY_BUILD_IDENTITY_FILE into ProductInfo. Until
    coordinated source integration exists, the build is rejected after linkage.
    """
    validate_identity(expected, expected['source']['commit'], expected['identity_sha256'])
    source = source_snapshot(repo, expected['source']['commit'], expected_patch=expected['source']['patch_sha256'])
    require(identity(source, expected['configuration']) == expected, 'Source differs from reviewed snapshot')
    require(toolchain_digest() == expected['configuration']['toolchain_sha256'], 'Toolchain changed')
    require(environment_digest() == expected['configuration']['environment_sha256'], 'Build environment changed')
    target = target.absolute()
    require(not target.exists() and not target.is_symlink() and target.parent.is_dir(),
            'Build target must be fresh; never clean/reuse an unproven cache or signed build')
    require(not target.is_relative_to(repo.absolute()), 'Isolated target must be outside source')
    require(shutil.disk_usage(target.parent).free >= 4 * 1024 ** 3, 'Four GiB free-space reserve required')
    target.mkdir(mode=0o700)
    source_file = target / 'compiled-build-identity.json'
    write_new(source_file, expected)
    module = target / 'compiled-build-identity.rs'
    module.write_bytes(compiled_module(expected))
    module_hash = hash_file(module)
    config = expected['configuration']
    command = ['cargo', 'build', '-p', 'remote_play_app', '--bin', 'remote_play',
               '--no-default-features', '--features', ','.join(config['features']),
               '--profile', config['profile'], '--target', config['target'], '--locked', '--offline', '-j', '2']
    env = build_environment()
    env.update(CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL='0',
               REMOTEPLAY_BUILD_IDENTITY_FILE=str(source_file), REMOTEPLAY_BUILD_IDENTITY_RS=str(module))
    process = subprocess.run(command, cwd=repo, env=env)
    require(process.returncode == 0, 'Build failed; no receipt minted')
    after = source_snapshot(repo, expected['source']['commit'], expected_patch=expected['source']['patch_sha256'])
    require(after == source and read_json(source_file) == expected and hash_file(module) == module_hash,
            'Source/identity changed during build')
    require(environment_digest() == config['environment_sha256'] and
            toolchain_digest() == config['toolchain_sha256'], 'Compiler environment/toolchain changed during build')
    require(shutil.disk_usage(target).free >= 4 * 1024 ** 3, 'Build exhausted required disk reserve')
    profile = 'debug' if config['profile'] == 'dev' else 'release'
    binary = target / config['target'] / profile / ('remote_play.exe' if config['platform'] == 'windows' else 'remote_play')
    value = hash_file(binary)
    info = product_info(binary, expected, value)
    return {'schema': 1, 'kind': 'observed-build', 'build_identity': expected,
            'compiler_binary_sha256': value, 'compiler_binary_bytes': binary.stat().st_size,
            'product_info': info, 'build_command': command, 'fresh_target': True,
            'visual_acceptance': 'not_evaluated', 'stream_acceptance': 'not_evaluated'}


def files_in(root: Path) -> list[dict]:
    require(root.is_absolute() and root.is_dir() and not root.is_symlink(), 'Real absolute artifact directory required')
    records, folded, size = [], set(), 0
    for p in sorted(root.rglob('*')):
        require(not p.is_symlink(), 'Linked artifact rejected')
        name = p.relative_to(root).as_posix(); relative(name)
        require(name.casefold() not in folded, 'Case-colliding artifact rejected')
        folded.add(name.casefold())
        if p.is_dir(): continue
        value = hash_file(p)
        size += p.stat().st_size
        records.append({'path': name, 'sha256': value, 'bytes': p.stat().st_size})
        require(len(records) <= MAX_FILES and size <= LIMIT, 'Artifact exceeds limits')
    require(bool(records), 'Empty artifact')
    return records


def seal_tree(stage: Path, build_receipt: dict, expected_commit: str, expected_identity: str,
              *, compiler_binary: Path | None = None) -> dict:
    require(build_receipt.get('kind') == 'observed-build' and build_receipt.get('fresh_target') is True,
            'An independently pinned observed-build receipt is required')
    expected = validate_identity(build_receipt.get('build_identity'), expected_commit, expected_identity)
    platform = expected['configuration']['platform']
    top, executable = LAYOUTS[platform]
    require({p.name for p in stage.iterdir()} == {top}, 'Unexpected staging content or existing manifest')
    app = stage / top; binary = app / executable
    compiler_sha = sha(build_receipt['compiler_binary_sha256'])
    final_sha = hash_file(binary)
    signing = None
    if platform == 'macos':
        require(sys.platform == 'darwin' and compiler_binary is not None,
                'Native pinned signing verification and original compiler artifact required')
        require(hash_file(compiler_binary) == compiler_sha, 'Compiler handoff differs from observed build')
        product_info(compiler_binary, expected, compiler_sha)
        signing = authorized_mac_app(app)
        require(signing['executable_sha256'] == final_sha, 'Signed artifact changed')
        require(signing['version'] == expected['configuration']['version'], 'Signed version/build contract mismatch')
        signing = {k: signing[k] for k in ('bundle_id', 'certificate_sha1', 'designated_requirement',
                                         'version', 'build', 'signature_verified')}
    else:
        require(final_sha == compiler_sha, 'Staged binary differs from observed compiler output')
    info = product_info(binary, expected, final_sha)
    records = files_in(app)
    manifest = {'schema': 1, 'kind': 'remoteplay-release', 'build_identity': expected,
                'artifact_root': top, 'executable': executable, 'files': records,
                'compiler_binary_sha256': compiler_sha, 'binary_sha256': final_sha,
                'product_info': info, 'signing': signing,
                'visual_acceptance': 'not_evaluated', 'stream_acceptance': 'not_evaluated'}
    # The final byte hash lives OUTSIDE a signed .app; no circular post-sign edit.
    require(files_in(app) == records, 'Staged files changed before sealing')
    manifest_sha = write_new(stage / MANIFEST, manifest)
    return {'manifest_sha256': manifest_sha, 'manifest': manifest,
            'release_authorized': False}


def manifest_document(raw: bytes, expected_sha: str, expected_commit: str, expected_identity: str) -> dict:
    require(digest(raw) == sha(expected_sha), 'Package manifest differs from independently pinned release receipt')
    m = json_bytes(raw)
    require(type(m.get('schema')) is int and m['schema'] == 1 and m.get('kind') == 'remoteplay-release',
            'Missing release manifest contract')
    expected = validate_identity(m.get('build_identity'), expected_commit, expected_identity)
    platform = expected['configuration']['platform']
    top, executable = LAYOUTS[platform]
    require(m.get('artifact_root') == top and m.get('executable') == executable, 'Package GUI entry mismatch')
    validate_product_info(m['product_info'], expected)
    require(type(m.get('files')) is list and 0 < len(m['files']) <= MAX_FILES, 'Missing artifact inventory')
    names, folded, total = set(), set(), 0
    for entry in m['files']:
        require(type(entry) is dict and set(entry) == {'path', 'sha256', 'bytes'}, 'Invalid artifact record')
        name = relative(entry['path']).as_posix(); sha(entry['sha256'])
        require(type(entry['bytes']) is int and entry['bytes'] >= 0, 'Invalid artifact size')
        require(name not in names and name.casefold() not in folded, 'Duplicate artifact inventory')
        names.add(name); folded.add(name.casefold()); total += entry['bytes']
    require(total <= LIMIT and executable in names, 'Invalid/incomplete package inventory')
    main = next(e for e in m['files'] if e['path'] == executable)
    require(main['sha256'] == sha(m['binary_sha256']), 'Binary/hash binding differs')
    sha(m['compiler_binary_sha256'])
    if platform != 'macos':
        require(m['binary_sha256'] == m['compiler_binary_sha256'] and m.get('signing') is None,
                'Unverified binary transformation')
    else:
        require(type(m.get('signing')) is dict and m['signing'].get('signature_verified') is True,
                'Missing authorized signer verification')
        from macos_release_guard import load_policy
        policy = load_policy()
        require(m['signing'].get('bundle_id') == policy.bundle_id and
                m['signing'].get('certificate_sha1') == policy.certificate_sha1 and
                m['signing'].get('designated_requirement') == policy.requirement,
                'Package claims an unauthorized macOS identity')
        require(m['signing'].get('version') == expected['configuration']['version'], 'Signed/package version mismatch')
    return m


@contextmanager
def archive_entries(path: Path):
    regular(path)
    if zipfile.is_zipfile(path):
        with zipfile.ZipFile(path) as z:
            entries = []
            for e in z.infolist():
                require(not e.flag_bits & 1 and stat.S_IFMT(e.external_attr >> 16) in (0, stat.S_IFREG, stat.S_IFDIR),
                        'Encrypted/linked/special ZIP entry rejected')
                entries.append((e.filename.rstrip('/') if e.is_dir() else e.filename,
                                e.is_dir(), e.file_size, lambda e=e: z.open(e)))
            yield entries
    else:
        try:
            with tarfile.open(path) as t:
                entries = []
                for e in t:
                    require(e.isdir() or e.isfile(), 'Linked/special TAR entry rejected')
                    entries.append((e.name.rstrip('/') if e.isdir() else e.name, e.isdir(), e.size,
                                    lambda e=e: t.extractfile(e)))
                    require(len(entries) <= MAX_FILES, 'Archive entry limit exceeded')
                yield entries
        except tarfile.TarError as error:
            raise IdentityRejected('Unsupported/corrupt package') from error


def verify_package(path: Path, expected_manifest: str, expected_commit: str, expected_identity: str) -> dict:
    before = hash_file(path)
    with archive_entries(path) as entries:
        require(len(entries) <= MAX_FILES and sum(e[2] for e in entries) <= LIMIT + 2 * 1024 ** 2,
                'Archive exceeds limits')
        names, folded, prefixes, kinds = set(), set(), {}, {}
        for name, directory, size, _ in entries:
            parts = relative(name).parts
            require(name not in names and name.casefold() not in folded and size >= 0,
                    'Duplicate/case-colliding archive member')
            names.add(name); folded.add(name.casefold())
            kinds[name] = directory
            for n in range(1, len(parts) + 1):
                prefix = '/'.join(parts[:n])
                require(prefix.casefold() not in prefixes or prefixes[prefix.casefold()] == prefix,
                        'Case-colliding archive ancestor')
                prefixes[prefix.casefold()] = prefix
        for name in names:
            parts = PurePosixPath(name).parts
            require(all(kinds.get('/'.join(parts[:n]), True) for n in range(1, len(parts))),
                    'Archive file used as a directory')
        matches = [e for e in entries if e[0] == MANIFEST and not e[1]]
        require(len(matches) == 1 and matches[0][2] <= 2 * 1024 ** 2, 'Package manifest missing')
        with matches[0][3]() as f: raw = f.read(2 * 1024 ** 2 + 1)
        m = manifest_document(raw, expected_manifest, expected_commit, expected_identity)
        files = {m['artifact_root'] + '/' + e['path']: e for e in m['files']}
        actual = {}
        for name, directory, size, opener in entries:
            if directory:
                require(name == m['artifact_root'] or name.startswith(m['artifact_root'] + '/'), 'Unexpected archive directory')
                continue
            if name == MANIFEST: continue
            require(name in files and size == files[name]['bytes'], 'Unlisted/wrong-size package member')
            with opener() as f: value = hashlib.file_digest(f, 'sha256').hexdigest()
            require(value == files[name]['sha256'], 'Package payload/hash mismatch')
            actual[name] = value
        require(set(actual) == set(files), 'Package payload missing')
    require(hash_file(path) == before, 'Package changed during verification')
    return {'archive_sha256': before, 'manifest_sha256': expected_manifest,
            'package_integrity_verified': True, 'native_signing_verified_here': False,
            'release_authorized': False, 'manifest': m}


def verify_install(app: Path, manifest_path: Path, expected_manifest: str, expected_commit: str,
                   expected_identity: str, canonical_executable: Path, launch_executable: Path,
                   *, launch_arguments: list[str] | None = None) -> dict:
    with regular(manifest_path).open('rb') as incoming:
        raw_manifest = incoming.read(2 * 1024 ** 2 + 1)
    m = manifest_document(raw_manifest, expected_manifest, expected_commit, expected_identity)
    expected = app / m['executable']
    require((launch_arguments or []) == m['build_identity']['configuration']['gui_arguments'],
            'Daily GUI launch arguments differ from the compiled entry')
    require(expected.absolute() == canonical_executable.absolute(), 'Installation differs from canonical daily executable')
    regular(expected); regular(launch_executable)
    require(expected == launch_executable.absolute() and os.path.samefile(expected, launch_executable),
            'Daily entry still targets another/old application')
    require(files_in(app) == sorted(m['files'], key=lambda e: e['path']), 'Installed files differ from package manifest')
    platform = m['build_identity']['configuration']['platform']
    if platform == 'macos':
        require(sys.platform == 'darwin', 'Native macOS signer/upgrade verification unavailable')
        actual = authorized_mac_app(app)
        require(actual['executable_sha256'] == m['binary_sha256'], 'Installed signed executable changed')
    info = product_info(expected, m['build_identity'], m['binary_sha256'])
    require(info == m['product_info'], 'Installed product metadata differs from sealed artifact')
    return {'installation_identity_verified': True, 'daily_entry_verified': True,
            'binary_sha256': m['binary_sha256'], 'build_identity': m['build_identity'],
            'visual_acceptance': 'not_evaluated', 'stream_acceptance': 'not_evaluated',
            'release_authorized': False}


def verify_running_linux(pid: int, canonical_executable: Path, expected_binary: str) -> dict:
    """Kernel-loaded inode/hash check, not ps argv or a sidecar process claim."""
    require(sys.platform.startswith('linux') and type(pid) is int and pid > 0,
            'Native loaded-image verification unsupported; authenticated lifecycle adapter required')
    regular(canonical_executable); sha(expected_binary)
    process = Path('/proc') / str(pid)
    try:
        before = (process / 'stat').read_bytes()
        start = before[before.rfind(b')') + 2:].split()[19]  # Linux stat field 22.
        path = os.readlink(process / 'exe')
        require(path == str(canonical_executable), 'Actual process uses another/deleted executable')
        with (process / 'exe').open('rb') as image:
            live = os.fstat(image.fileno()); disk = canonical_executable.stat()
            require((live.st_dev, live.st_ino) == (disk.st_dev, disk.st_ino), 'Old image still owns daily entry')
            require(hashlib.file_digest(image, 'sha256').hexdigest() == expected_binary, 'Loaded image is another build')
        after = (process / 'stat').read_bytes()
        require(after[after.rfind(b')') + 2:].split()[19] == start and
                os.readlink(process / 'exe') == path and hash_file(canonical_executable) == expected_binary,
                'Process/artifact changed during observation')
    except (OSError, IndexError) as error:
        raise IdentityRejected('Actual process image unavailable; no fallback to argv or disk-only claims') from error
    return {'actual_loaded_image_verified': True, 'binary_sha256': expected_binary,
            'runtime_build_identity': 'bound through independently verified artifact', 'release_authorized': False}


def verify_running_macos(pid: int, canonical_executable: Path, expected_binary: str,
                         observer: Path, observer_sha256: str) -> dict:
    require(sys.platform == 'darwin' and type(pid) is int and pid > 0, 'Invalid native macOS process request')
    require(hash_file(observer) == sha(observer_sha256), 'Unverified native observer refused')
    require(hash_file(canonical_executable) == sha(expected_binary), 'Installed image changed')
    before = canonical_executable.stat()
    observations = []
    for _ in range(2):
        p = subprocess.run([str(observer), str(pid), str(canonical_executable)], capture_output=True, timeout=10)
        require(p.returncode == 0, 'Native mapped-image evidence denied/unavailable; no fallback')
        data = json_bytes(p.stdout)
        require(type(data.get('schema')) is int and data['schema'] == 1 and data.get('verified') is True and
                type(data.get('pid')) is int and data['pid'] == pid, 'Native observer returned no process evidence')
        for k in ('start_unix_ns', 'device', 'inode', 'change_unix_ns'):
            require(type(data.get(k)) is int and data[k] >= 0, 'Incomplete native process evidence')
        require((data['device'], data['inode']) == (before.st_dev, before.st_ino) and
                data['change_unix_ns'] == before.st_ctime_ns and before.st_ctime_ns <= data['start_unix_ns'],
                'Old/mutated mapped image still owns the entry')
        observations.append(data)
    require(observations[0] == observations[1] and hash_file(canonical_executable) == expected_binary and
            hash_file(observer) == observer_sha256 and canonical_executable.stat().st_ctime_ns == before.st_ctime_ns,
            'Process/image/observer changed during verification')
    return {'actual_loaded_image_verified': True, 'binary_sha256': expected_binary,
            'runtime_build_identity': 'bound through native mapped inode and unchanged verified artifact',
            'release_authorized': False}


def verify_running_windows(pid: int, canonical_executable: Path, expected_binary: str, expected: dict) -> dict:
    require(sys.platform == 'win32', 'Native Windows observation unavailable')
    require(hash_file(canonical_executable) == sha(expected_binary), 'Installed binary changed')
    from windows_process_identity import observe
    raw = json.dumps(expected, sort_keys=True, indent=2).encode() + b'\n'
    try:
        result = observe(pid, canonical_executable, raw)
    except (OSError, RuntimeError, ValueError) as error:
        raise IdentityRejected('Native compiled process image unavailable; no fallback') from error
    require(hash_file(canonical_executable) == expected_binary, 'Installed image changed during observation')
    return dict(result, binary_sha256=expected_binary)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    inspect = commands.add_parser('inspect-source')
    inspect.add_argument('--repo', type=Path, required=True)
    inspect.add_argument('--expected-commit', required=True)
    config = commands.add_parser('configuration')
    config.add_argument('--platform', choices=tuple(LAYOUTS), required=True)
    config.add_argument('--architecture', choices=('aarch64', 'x86_64'), required=True)
    config.add_argument('--version', required=True)
    config.add_argument('--target', required=True)
    config.add_argument('--profile', choices=('dev', 'release'), required=True)
    config.add_argument('--output', type=Path, required=True)
    prepare = commands.add_parser('prepare')
    prepare.add_argument('--repo', type=Path, required=True)
    prepare.add_argument('--expected-commit', required=True)
    prepare.add_argument('--expected-patch-sha256')
    prepare.add_argument('--expected-snapshot-sha256', required=True)
    prepare.add_argument('--configuration', type=Path, required=True)
    prepare.add_argument('--output', type=Path, required=True)
    build = commands.add_parser('build')
    build.add_argument('--repo', type=Path, required=True)
    build.add_argument('--identity', type=Path, required=True)
    build.add_argument('--identity-sha256', required=True, help='SHA-256 of the prepared JSON file from independent handoff')
    build.add_argument('--target', type=Path, required=True)
    build.add_argument('--output', type=Path, required=True)
    seal = commands.add_parser('seal')
    seal.add_argument('--stage', type=Path, required=True)
    seal.add_argument('--build-receipt', type=Path, required=True)
    seal.add_argument('--build-receipt-sha256', required=True)
    seal.add_argument('--compiler-binary', type=Path)
    for name in ('package', 'installed', 'running'):
        p = commands.add_parser(name)
        p.add_argument('--expected-commit', required=True)
        p.add_argument('--expected-identity-sha256', required=True)
        p.add_argument('--expected-manifest-sha256', required=True)
        if name == 'package': p.add_argument('--archive', type=Path, required=True)
        else:
            p.add_argument('--app', type=Path, required=True)
            p.add_argument('--manifest', type=Path, required=True)
            p.add_argument('--canonical-executable', type=Path, required=True)
            p.add_argument('--launch-executable', type=Path, required=True)
            p.add_argument('--launch-argument', action='append', default=[])
            if name == 'running':
                p.add_argument('--pid', type=int, required=True)
                p.add_argument('--observer', type=Path)
                p.add_argument('--observer-sha256')
    seal.add_argument('--expected-commit', required=True)
    seal.add_argument('--expected-identity-sha256', required=True)
    args = parser.parse_args()
    try:
        if args.command == 'inspect-source':
            result = {'source': source_snapshot(args.repo, args.expected_commit, inspect_only=True),
                      'approved': False, 'compiled': False}
        elif args.command == 'configuration':
            result = configuration(args.platform, args.architecture, args.version, args.target,
                                   args.profile, toolchain_digest(), environment_digest())
            result = {'configuration_sha256': write_new(args.output, result), 'configuration': result}
        elif args.command == 'prepare':
            s = source_snapshot(args.repo, args.expected_commit, expected_patch=args.expected_patch_sha256)
            require(s['snapshot_sha256'] == sha(args.expected_snapshot_sha256), 'Snapshot differs from independently reviewed bytes')
            result = identity(s, read_json(args.configuration))
            output_sha = write_new(args.output, result)
            result = {'identity_file_sha256': output_sha, 'build_identity': result, 'compiled': False}
        elif args.command == 'build':
            result = observed_build(args.repo, read_json(args.identity, args.identity_sha256), args.target)
            result = {'build_receipt_sha256': write_new(args.output, result), 'build': result}
        elif args.command == 'seal':
            result = seal_tree(args.stage, read_json(args.build_receipt, args.build_receipt_sha256),
                               args.expected_commit, args.expected_identity_sha256, compiler_binary=args.compiler_binary)
        elif args.command == 'package':
            result = verify_package(args.archive, args.expected_manifest_sha256, args.expected_commit, args.expected_identity_sha256)
        else:
            result = verify_install(args.app, args.manifest, args.expected_manifest_sha256,
                                    args.expected_commit, args.expected_identity_sha256,
                                    args.canonical_executable, args.launch_executable,
                                    launch_arguments=args.launch_argument)
            if args.command == 'running':
                if sys.platform == 'darwin':
                    require(args.observer is not None and args.observer_sha256 is not None,
                            'Verified native macOS observer is required')
                    result.update(verify_running_macos(args.pid, args.canonical_executable, result['binary_sha256'],
                                                       args.observer, args.observer_sha256))
                elif sys.platform == 'win32':
                    result.update(verify_running_windows(args.pid, args.canonical_executable,
                                                        result['binary_sha256'], result['build_identity']))
                else:
                    result.update(verify_running_linux(args.pid, args.canonical_executable, result['binary_sha256']))
        print(json.dumps(result, sort_keys=True, indent=2))
    except (IdentityRejected, GuiReleaseRejected, OSError, KeyError, TypeError, ValueError, zipfile.BadZipFile,
            subprocess.SubprocessError) as error:
        # Do not echo private paths, raw native diagnostics, profile or key data.
        print(json.dumps({'identity_verified': False, 'reason': type(error).__name__,
                          'detail': str(error) if isinstance(error, (IdentityRejected, GuiReleaseRejected)) else 'Required evidence unavailable',
                          'release_authorized': False}), file=sys.stderr)
        raise SystemExit(1)


if __name__ == '__main__':
    main()
