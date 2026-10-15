#!/usr/bin/env python3
"""Fixed native Windows identity build over the existing primary-main cache."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

import build_identity_gate as gate

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--expected-commit', required=True)
    parser.add_argument('--expected-patch')
    parser.add_argument('--ffmpeg', type=Path, required=True)
    parser.add_argument('--with-input-helper', action='store_true')
    args = parser.parse_args()
    gate.require(sys.platform == 'win32', 'Native Windows only')
    repo = Path(__file__).resolve().parents[1]
    gate.require(gate.git(repo, 'branch', '--show-current').strip() == b'main', 'Primary main required')
    env = gate.build_environment()
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--locked', '--offline', '--no-deps', '--format-version=1'], cwd=repo, env=env))
    target = Path(metadata['target_directory']).resolve(strict=True)
    gate.require(target == (repo / 'target').resolve(strict=True)
                 and Path(metadata.get('build_directory') or target).resolve() == target,
                 'Preserve the existing native Windows project-local cache')
    gate.require(shutil.disk_usage(target).free >= 4 * 1024**3, 'Four GiB reserve required')
    gate.regular(args.ffmpeg)
    source = gate.source_snapshot(repo, args.expected_commit, expected_patch=args.expected_patch)
    config = gate.configuration('windows', 'x86_64', (repo/'VERSION').read_text().strip(),
        'x86_64-pc-windows-msvc', 'release', gate.toolchain_digest(), gate.environment_digest())
    expected = gate.identity(source, config)
    lock = target / '.remoteplay-windows-current.lock'
    inputs = [target/'.remoteplay-build-identity.json', target/'.remoteplay-build-identity.rs']
    payloads = [json.dumps(expected, sort_keys=True, indent=2).encode()+b'\n', gate.compiled_module(expected)]
    gate.require(not any(p.exists() or p.is_symlink() for p in [lock, *inputs]),
                 'Existing producer/input state must be reconciled; never overwrite it')
    owned = {}; child = None; settled = True
    try:
        with lock.open('xb') as output:
            output.write(gate.canonical({'pid':os.getpid(),'source':str(repo),'identity':expected['identity_sha256']}))
        owned[lock] = gate.hash_file(lock)
        for path, raw in zip(inputs, payloads):
            with path.open('xb') as output:
                output.write(raw)
            owned[path] = hashlib.sha256(raw).hexdigest()
        compiler_path = lambda path: '\\\\?\\' + str(path)
        build_env = dict(env, REMOTEPLAY_BUILD_IDENTITY_FILE=compiler_path(inputs[0]), REMOTEPLAY_BUILD_IDENTITY_RS=compiler_path(inputs[1]))
        command = ['cargo','build','--release','-p','remote_play_app','--bin','remote_play',
            '--no-default-features','--features','gpui-restoration,native-windows-video','--locked','--offline','-j','2']
        settled = False
        child = subprocess.Popen(command, cwd=repo, env=build_env, creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
        result = child.wait(); settled = True
        gate.require(result == 0, 'Build failed; no receipt minted')
        gate.require(gate.source_snapshot(repo,args.expected_commit,expected_patch=args.expected_patch)==source,
                     'Source changed during compilation')
        gate.require(gate.toolchain_digest()==config['toolchain_sha256']
                     and gate.environment_digest()==config['environment_sha256'], 'Compiler environment changed')
        binary = target/'release/remote_play.exe'
        binary_hash = gate.hash_file(binary)
        info = gate.product_info(binary, expected, binary_hash)
        helper_hash = None
        if args.with_input_helper:
            helper_command = ['cargo','build','--release','-p','remote_play_app','--example',
                'window_scoped_acceptance_guarded','--no-default-features','--features',
                'gpui-restoration,native-windows-video','--locked','--offline','-j','2']
            settled = False
            child = subprocess.Popen(helper_command, cwd=repo, env=env, creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
            result = child.wait(); settled = True
            gate.require(result == 0, 'Controlled input helper failed; no receipt minted')
            helper_hash = gate.hash_file(target/'release/examples/window_scoped_acceptance_guarded.exe')
        receipt = {'schema':2,'kind':'observed-build','build_identity':expected,
            'compiler_binary_sha256':binary_hash,'compiler_binary_bytes':binary.stat().st_size,
            'product_info':info,'build_command':command,'fresh_target':False,
            'build_storage':{'mode':'shared-incremental','source_directory':str(repo),'target_directory':str(target)},
            'visual_acceptance':'not_evaluated','stream_acceptance':'not_evaluated',
            'controlled_input_helper_sha256':helper_hash}
        package = target/'package/windows';package.mkdir(parents=True,exist_ok=True)
        receipt_path = package/'windows-current-build-receipt.json'
        pending = receipt_path.with_suffix('.pending')
        with pending.open('x',encoding='utf-8') as output:
            json.dump(receipt,output,indent=2);output.write('\n')
        os.replace(pending,receipt_path)
        subprocess.run(['powershell.exe','-NoProfile','-File',str(repo/'scripts/package_windows_unified.ps1'),
            '-SkipBuild','-BuildReceipt',str(receipt_path),'-FfmpegPath',str(args.ffmpeg)],cwd=repo,env=env,check=True)
        gate.require(gate.source_snapshot(repo,args.expected_commit,expected_patch=args.expected_patch)==source,
                     'Source changed during packaging')
        print(json.dumps({'build_identity_sha256':expected['identity_sha256'],'commit':args.expected_commit,
            'binary_sha256':binary_hash,'receipt':str(receipt_path),'package':str(package/'RemotePlay-Windows-x64.zip')}))
    finally:
        if child is not None and child.poll() is None:
            subprocess.run(['taskkill.exe','/PID',str(child.pid),'/T','/F'],capture_output=True)
            try:child.wait(timeout=20);settled=True
            except subprocess.TimeoutExpired:pass
        if settled:
            for path,value in reversed(list(owned.items())):
                if path.exists() and not path.is_symlink() and gate.hash_file(path)==value:
                    path.unlink()
        else:
            print('Compiler exit unknown: fixed owned inputs preserved',file=sys.stderr)

if __name__ == '__main__':
    main()
