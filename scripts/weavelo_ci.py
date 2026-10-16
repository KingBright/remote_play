#!/usr/bin/env python3
"""Pinned local entry points over existing Weavelo release tools; default dry-run."""
import argparse
import hashlib
import json
import os
from contextlib import contextmanager
from pathlib import Path
import platform
import subprocess
import sys
import time

import build_identity_gate as identity
from verify_prebuilt_binary import verified_path

STEPS = ('build', 'test', 'package', 'deploy-plan', 'deploy', 'verify', 'rollback', 'publish-plan', 'publish')
EXTERNAL = {'deploy', 'rollback', 'publish'}


def require(ok, reason):
    if not ok:
        raise ValueError(reason)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def checked(spec, step):
    require(spec.get('schema') == 1, 'spec schema must be 1')
    require(set(spec) <= {'schema', 'machine', 'architecture', 'execution_role', 'repo', 'source_commit',
                         'platform', 'source_snapshot_sha256', 'artifact', 'build', 'stage_dir'}, 'unknown spec fields; do not put secrets in CI specs')
    require(spec['machine'] == platform.node(), 'wrong execution machine')
    require(spec['architecture'] == platform.machine(), 'wrong execution architecture')
    require(spec['execution_role'] in ('builder', 'nas-runtime'), 'unknown execution role')
    require(set(spec.get('build', {})) <= {'package', 'example', 'features', 'profile'}, 'unknown build fields; secrets are not CI spec inputs')
    require(not (spec['execution_role'] == 'nas-runtime' and step in ('build', 'test', 'package')),
            'NAS accepts running artifacts only; compilation/test/package forbidden')
    repo = Path(spec['repo']).resolve(strict=True)
    head = subprocess.check_output(['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
    require(head == spec['source_commit'], 'source commit differs from reviewed spec')
    require(spec['platform'] in ('macos', 'linux', 'windows'), 'unknown target platform')
    if step in ('build', 'test', 'package', 'deploy-plan', 'deploy', 'rollback'):
        require(spec['platform'] == {'Darwin': 'macos', 'Linux': 'linux', 'Windows': 'windows'}.get(platform.system()),
                'wrong native target platform; cross-builds require the existing platform-specific owner')
    if step in ('build', 'test', 'package'):
        source = identity.source_snapshot(repo, spec['source_commit'], inspect_only=True)
        require(source['snapshot_sha256'] == spec.get('source_snapshot_sha256'),
                'source bytes/dirty patch differ; inspect-source and explicitly pin snapshot first')
    artifact = spec.get('artifact')
    if step not in ('build', 'test'):
        require(isinstance(artifact, dict), 'exact artifact required')
        require(set(artifact) <= {'path', 'sha256', 'purpose', 'version', 'build', 'identity'}, 'unknown artifact fields')
        require(set(artifact.get('identity', {})) <= {'source_commit', 'identity_sha256', 'manifest_sha256', 'observer', 'observer_sha256'}, 'unknown release identity fields')
        verified_path(Path(artifact['path']), artifact['sha256'])
        if artifact.get('purpose') == 'diagnostic':
            with Path(artifact['path']).open('rb') as f:
                header = f.read(24)
            formats = {'macos': (b'\xcf\xfa\xed\xfe', b'\xca\xfe\xba\xbe'), 'linux': (b'\x7fELF',), 'windows': (b'MZ',)}
            require(any(header.startswith(magic) for magic in formats[spec['platform']]), 'artifact format differs from target platform')
            if header.startswith(b'\xcf\xfa\xed\xfe'):
                cpu = int.from_bytes(header[4:8], 'little')
                expected_cpu = {'arm64': 0x100000c, 'aarch64': 0x100000c, 'x86_64': 0x1000007, 'AMD64': 0x1000007}.get(spec['architecture'])
                require(cpu == expected_cpu, 'Mach-O architecture differs from the pinned machine')
    return repo


def command(spec, step, repo):
    scripts = repo / 'scripts'
    python = [sys.executable, '-B']
    artifact = spec.get('artifact', {})
    env = {}
    if step in ('build', 'test'):
        require((repo / '.rust-storage/policy.json').is_file(),
                'automatic compile needs the existing owned Rust contract; do not replace observe-only/shared targets')
        build = spec.get('build', {})
        require(build.get('package', 'weavelo_app') == 'weavelo_app', 'unsupported build package')
        args = ['cargo', step, '--offline', '--locked', '-p', 'weavelo_app', '-j', '2']
        if build.get('example'):
            require(build['example'] == 'window_scoped_acceptance_guarded', 'unsupported diagnostic target')
            args += ['--example', build['example']]
        else:
            args += ['--bin', 'weavelo'] if step == 'build' else ['--lib']
        features = build.get('features', ['gpui-native-video'])
        require(all(x in ('gpui-restoration', 'gpui-native-video', 'native-linux-video', 'native-windows-video') for x in features), 'unknown feature')
        if features:
            args += ['--features', ','.join(features)]
        if build.get('profile', 'dev') == 'release':
            args += ['--release']
        else:
            require(build.get('profile', 'dev') == 'dev', 'unknown profile')
        return python + [str(repo / '.rust-storage/rust_storage.py'), 'run', '--project', str(repo), '--'] + args, env
    if step == 'verify' and artifact.get('purpose') == 'diagnostic':
        return python + [str(scripts / 'verify_prebuilt_binary.py'), '--binary', artifact['path'], '--sha256', artifact['sha256']], env
    require(artifact.get('purpose') == 'release', 'diagnostic helper must not be installed/published as product')
    require(spec['platform'] == 'macos', 'use the existing platform owner for non-macOS packaging/installation')
    if step == 'package':
        env = {'WEAVELO_PREBUILT_BINARY': artifact['path'], 'WEAVELO_PREBUILT_SHA256': artifact['sha256'],
               'WEAVELO_VERSION': artifact['version'], 'WEAVELO_BUILD_NUMBER': artifact['build']}
        return ['bash', str(scripts / 'package_macos_unified.sh')], env
    expected = artifact.get('identity', {})
    pins = ['--expected-commit', expected['source_commit'], '--expected-identity-sha256', expected['identity_sha256'],
            '--expected-manifest-sha256', expected['manifest_sha256']]
    if step in ('deploy-plan', 'deploy'):
        args = python + [str(scripts / 'install_macos_unified.py'), artifact['path']] + pins
        args += ['--native-observer', expected['observer'], '--native-observer-sha256', expected['observer_sha256']]
        if step == 'deploy':
            args += ['--apply']
        return args, env
    if step == 'rollback':
        return python + [str(scripts / 'install_macos_unified.py'), '--recover'], env
    if step == 'verify':
        return python + [str(scripts / 'build_identity_gate.py'), 'package', '--archive', artifact['path']] + pins, env
    if step in ('publish-plan', 'publish'):
        return ['bash', str(scripts / 'publish_desktop_nas.sh'), spec['stage_dir']] + (['--dry-run'] if step == 'publish-plan' else []), env
    raise ValueError('unsupported stage')


def atomic(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    part = path.with_name(path.name + '.new')
    with part.open('w') as stream:
        json.dump(value, stream, indent=2); stream.write('\n')
    os.replace(part, path)


@contextmanager
def exclusive(journal):
    journal.parent.mkdir(parents=True, exist_ok=True)
    lock = journal.with_name(journal.name + '.lock')
    # Atomic directory creation works on both POSIX and Windows. Never steal an
    # abandoned/unknown lock: observe the saved stage before explicit recovery.
    try:
        lock.mkdir()
    except FileExistsError:
        raise ValueError('CI journal is locked; observe the original run, do not start another')
    try:
        yield
    finally:
        lock.rmdir()


def run(spec, step, journal, apply=False):
    if apply:
        with exclusive(journal):
            return run_locked(spec, step, journal, True)
    return run_locked(spec, step, journal, False)


def run_locked(spec, step, journal, apply=False):
    repo = checked(spec, step)
    argv, extra_env = command(spec, step, repo)
    key = digest({'spec': spec, 'step': step})
    state = json.loads(journal.read_text()) if journal.exists() else {'schema': 1, 'stages': {}}
    prior = state['stages'].get(key)
    if prior:
        require(prior['state'] == 'completed', 'prior stage is failed/running/outcome_unknown; inspect its original receipt/journal, never replay')
        return {'stage': step, 'state': 'already_completed', 'receipt': prior, 'current_external_state_verified': False}
    plan = {'stage': step, 'state': 'dry_run', 'machine': spec['machine'], 'source_commit': spec['source_commit'],
            'platform': spec['platform'], 'architecture': spec['architecture'], 'artifact': spec.get('artifact'),
            'argv': argv, 'NAS_compilation': False}
    if not apply:
        return plan
    # --apply expresses execution intent, not additional device/signing/OS authority.
    record = dict(plan, state='running', started_at=time.time(), log=str(journal.with_name(key + '.log')))
    state['stages'][key] = record; atomic(journal, state)
    try:
        with Path(record['log']).open('xb') as log:
            result = subprocess.run(argv, cwd=repo, env=dict(os.environ, **extra_env), stdout=log, stderr=subprocess.STDOUT)
        record.update(exit_code=result.returncode, finished_at=time.time(), state='completed' if result.returncode == 0 else ('outcome_unknown' if step in EXTERNAL else 'failed'))
    except BaseException:
        record.update(state='outcome_unknown', finished_at=time.time())
        atomic(journal, state)
        raise
    atomic(journal, state)
    require(record['state'] == 'completed', 'stage stopped; preserve log and inspect original operation before any recovery')
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('step', choices=STEPS)
    parser.add_argument('--spec', type=Path, required=True)
    parser.add_argument('--journal', type=Path, required=True)
    parser.add_argument('--apply', action='store_true')
    args = parser.parse_args()
    print(json.dumps(run(json.loads(args.spec.read_text()), args.step, args.journal, args.apply), indent=2))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError, identity.IdentityRejected) as error:
        print(json.dumps({'state': 'stopped', 'error': str(error)}), file=sys.stderr)
        sys.exit(1)
