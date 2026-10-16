#!/usr/bin/env python3
"""Build and verify an original-GPUI candidate in an existing Cargo target.

No GUI, service changes, installation, signing, publication or new Cargo target.
The resulting candidate manifest is deliberately unusable as a release receipt.
"""
from __future__ import annotations

import argparse
from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_identity_gate as gate


def run(repo: Path, target: Path, stage: Path, commit: str,
        old_binary: Path, old_sha256: str) -> dict:
    gate.require(sys.platform.startswith('linux') or sys.platform == 'darwin',
                 'Only an explicitly authorized native Linux/macOS builder is supported')
    # Remote agent verbosity is a runtime setting, not a compiler override.
    os.environ.pop('RUST_LOG', None)
    target = gate.shared_target(repo, target)
    gate.require(old_binary.resolve() != target / 'debug/weavelo',
                 'Old-binary evidence must be separate from the compiler output being replaced')
    gate.require(gate.hash_file(old_binary) == gate.sha(old_sha256), 'Old-binary evidence changed')
    product_platform = 'macos' if sys.platform == 'darwin' else 'linux'
    architecture = {'AMD64': 'x86_64', 'arm64': 'aarch64'}.get(platform.machine(), platform.machine())
    host = subprocess.check_output(['rustc', '-vV'], text=True).split('host: ', 1)[1].splitlines()[0]
    gate.require(target.is_dir() and not target.is_symlink(), 'An existing Cargo target is required')
    gate.require(not stage.exists() and stage.parent.is_dir() and not stage.is_relative_to(repo),
                 'A new private evidence directory outside source is required')
    gate.require(shutil.disk_usage(target).free >= 4 * 1024 ** 3, 'Four GiB disk reserve required')
    source = gate.source_snapshot(repo, commit)
    config = gate.configuration(product_platform, architecture, '2.0.0-alpha.8', host, 'dev',
                                gate.toolchain_digest(), gate.environment_digest())
    expected = gate.identity(source, config)
    with gate.BuildInputs(repo, target, expected) as inputs:
        stage.mkdir(mode=0o700)
        gate.write_new(stage / 'source-inventory.json', source)
        identity_file = inputs.document
        identity_file_sha = gate.hash_file(identity_file)
        module = inputs.module
        original_module_sha = gate.hash_file(module)
        command = ['cargo', 'build', '-p', 'weavelo_app', '--bin', 'weavelo',
                   '--no-default-features', '--features', ','.join(config['features']),
                   '--profile', 'dev', '--locked', '--offline', '-j', '2',
                   '--message-format=json-render-diagnostics']
        env = inputs.environment()
        log = stage / 'cargo-build.jsonl'
        with log.open('xb') as output:
            log.chmod(0o600)
            returncode = inputs.run(command, env=env, stdout=output, stderr=subprocess.STDOUT)
        gate.require(inputs.settled, 'Compiler exit is unconfirmed; no acceptance receipt')
        gate.require(returncode == 0, 'Actual product compilation failed; inspect the private build log')
        messages = []
        for line in log.read_text().splitlines():
            try: messages.append(json.loads(line))
            except ValueError: continue
        artifacts = [m for m in messages if m.get('reason') == 'compiler-artifact' and
                     m.get('target', {}).get('name') == 'weavelo' and
                     'bin' in m.get('target', {}).get('kind', [])]
        gate.require(len(artifacts) == 1 and artifacts[0].get('fresh') is False,
                     'Product must actually be rebuilt; cached completion is insufficient')
        binary = Path(artifacts[0]['executable'])
        gate.require(binary == target / 'debug' / 'weavelo', 'Unexpected compiled product entry')
        gate.require(gate.source_snapshot(repo, commit) == source and
                     gate.read_json(identity_file, identity_file_sha) == expected and
                     gate.hash_file(module) == original_module_sha, 'Build source/identity changed')
        candidate = binary
        binary_sha = gate.hash_file(binary)
        receipt = gate.verify_candidate(repo, candidate, expected, binary_sha)
        receipt.update(build_command=command, cargo_product_artifact_fresh=False,
                       cargo_build_log_sha256=gate.hash_file(log), identity_file_sha256=identity_file_sha,
                       existing_target_preserved=True)
        cases = {'correct_candidate': True}
        def reject(name, action):
            try: action()
            except gate.IdentityRejected:
                cases[name] = True
            else: raise gate.IdentityRejected('Required candidate rejection did not occur')
        reject('old_binary', lambda: gate.verify_candidate(repo, old_binary, expected, old_sha256))
        changed_source = deepcopy(source); changed_source['snapshot_sha256'] = 'f' * 64
        reject('wrong_source_snapshot', lambda: gate.verify_candidate(
            repo, candidate, gate.identity(changed_source, config), binary_sha))
        changed_config = dict(config, environment_sha256='f' * 64)
        reject('wrong_configuration', lambda: gate.verify_candidate(
            repo, candidate, gate.identity(source, changed_config), binary_sha))
        # The actual compiled Cargo build script independently rejects wrong GUI and
        # feature/configuration inputs. This is not mocked ProductInfo output.
        hooks = [m for m in messages if m.get('reason') == 'compiler-artifact' and
                 m.get('target', {}).get('src_path') == str(repo / 'app' / 'build.rs') and
                 'custom-build' in m.get('target', {}).get('kind', [])]
        gate.require(len(hooks) == 1 and hooks[0].get('fresh') is False, 'No rebuilt product build-script evidence')
        hook = Path(hooks[0]['filenames'][0]); hook_sha = gate.hash_file(hook)
        hook_env = dict(env, CARGO_CFG_TARGET_OS=product_platform, CARGO_CFG_TARGET_ARCH=architecture,
                        CARGO_PKG_VERSION=config['version'], TARGET=host, PROFILE='debug',
                        CARGO_FEATURE_GPUI_RESTORATION='1')
        if product_platform == 'linux':
            hook_env.update(CARGO_FEATURE_NATIVE_LINUX_VIDEO='1', CARGO_FEATURE_GPUI_NATIVE_VIDEO='1')
        def hook_case(name, record):
            folder = stage / name; folder.mkdir(mode=0o700)
            document = folder / 'compiled-build-identity.json'; gate.write_new(document, record)
            generated = folder / 'compiled-build-identity.rs'; generated.write_bytes(gate.compiled_module(record))
            environment = dict(hook_env, OUT_DIR=str(folder), WEAVELO_BUILD_IDENTITY_FILE=str(document),
                               WEAVELO_BUILD_IDENTITY_RS=str(generated))
            result = subprocess.run([str(hook)], env=environment, capture_output=True)
            (folder / 'stdout.log').write_bytes(result.stdout)
            (folder / 'stderr.log').write_bytes(result.stderr)
            gate.require(result.returncode != 0 and b'Weavelo build identity rejected:' in result.stderr,
                         'Native build script accepted a forbidden product configuration')
            cases[name] = True
        wrong_gui = deepcopy(expected); wrong_gui['configuration']['gui_entry'] = 'egui-diagnostic'
        hook_case('wrong_gui_native_build_script', wrong_gui)
        wrong_features = deepcopy(expected); wrong_features['configuration']['features'] = []
        hook_case('wrong_features_native_build_script', wrong_features)
        sidecar_env = dict(env, WEAVELO_BUILD_IDENTITY_FILE=str(stage / 'wrong_gui_native_build_script' / 'compiled-build-identity.json'))
        changed_runtime = subprocess.run([str(candidate), '--product-info-json'], env=sidecar_env, capture_output=True)
        gate.require(changed_runtime.returncode == 0 and gate.json_bytes(changed_runtime.stdout) == receipt['product_info'],
                     'Runtime sidecar changed compiled identity')
        cases['runtime_sidecar_cannot_relabel_binary'] = True
        gate.require(gate.hash_file(candidate) == binary_sha and gate.hash_file(hook) == hook_sha and
                     gate.source_snapshot(repo, commit) == source, 'Acceptance altered source/artifacts')
        receipt.update(rejection_cases=cases, native_build_script_sha256=hook_sha)
        manifest_sha = gate.write_new(stage / 'weavelo-build-candidate-manifest.json', receipt)
        return {'source_commit': commit, 'source_snapshot_sha256': source['snapshot_sha256'],
                'configuration_sha256': expected['configuration_sha256'], 'identity_sha256': expected['identity_sha256'],
                'binary_sha256': binary_sha, 'candidate_manifest_sha256': manifest_sha,
                'actual_product_rebuilt': True, 'rejection_cases': cases, 'release_authorized': False,
                'product_gui_started': False, 'installed_app_modified': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('repo', 'target', 'stage', 'old-binary'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--expected-commit', required=True)
    parser.add_argument('--old-binary-sha256', required=True)
    args = parser.parse_args()
    result = run(args.repo.resolve(strict=True), args.target.resolve(strict=True),
                 args.stage.parent.resolve(strict=True) / args.stage.name,
                 args.expected_commit, args.old_binary.resolve(strict=True), args.old_binary_sha256)
    print(json.dumps(result, sort_keys=True, indent=2))


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    main()
