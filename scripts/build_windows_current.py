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

def packaging_environment(build_env, inherited=None):
    """Respect an already chosen process policy; never choose or change one."""
    result = dict(build_env)
    inherited = os.environ if inherited is None else inherited
    policy = inherited.get('PSExecutionPolicyPreference')
    if policy:
        gate.require(policy in ('Restricted', 'AllSigned', 'RemoteSigned',
                     'Unrestricted', 'Bypass', 'Undefined', 'Default'),
                     'Invalid inherited PowerShell process policy')
        result['PSExecutionPolicyPreference'] = policy
    return result


def validate_packaging_policy(report):
    gate.require(isinstance(report, dict) and isinstance(report.get('scripts'), list) and len(report['scripts']) == 2,
                 'PowerShell packaging policy observation unavailable')
    policy = report.get('effective')
    gate.require(policy in ('Restricted', 'AllSigned', 'RemoteSigned', 'Unrestricted', 'Bypass'),
                 'Unknown effective PowerShell execution policy')
    gate.require(policy != 'Restricted',
                 'PowerShell Restricted blocks the formal package scripts. No settings changed. '
                 'Use --build-only for a build receipt, or obtain approval for the package process policy.')
    for script in report['scripts']:
        gate.require(isinstance(script, dict) and isinstance(script.get('path'), str)
                     and isinstance(script.get('signature'), str)
                     and type(script.get('zone')) is int and 0 <= script['zone'] <= 4,
                     'Incomplete package script policy observation')
        signed = script.get('signature') == 'Valid'
        gate.require(policy != 'AllSigned' or signed,
                     'AllSigned requires trusted signatures; no policy override attempted')
        gate.require(policy != 'RemoteSigned' or script.get('zone', 0) < 3 or signed,
                     'RemoteSigned blocks an unsigned Internet-zone package script; nothing unblocked')
    return report


def packaging_preflight(repo, env):
    scripts = [repo/'scripts/package_windows_unified.ps1', repo/'scripts/verify_desktop_gui.ps1']
    for path in scripts:
        gate.regular(path)
    literals = ','.join("'" + str(path).replace("'", "''") + "'" for path in scripts)
    command = (
        "$ErrorActionPreference='Stop';$items=@();foreach($p in @(" + literals + ")){"
        "$zone=0;$lines=Get-Content -LiteralPath $p -Stream Zone.Identifier -ErrorAction SilentlyContinue;"
        "foreach($line in $lines){if($line -match '^ZoneId=(\\d+)$'){$zone=[int]$Matches[1]}};"
        "$items+=@{path=$p;signature=(Get-AuthenticodeSignature -LiteralPath $p).Status.ToString();zone=$zone}};"
        "@{effective=(Get-ExecutionPolicy).ToString();scripts=$items}|ConvertTo-Json -Depth 4 -Compress")
    result = subprocess.run(['powershell.exe', '-NoProfile', '-Command', command],
                            env=env, cwd=repo, capture_output=True, text=True, timeout=30)
    gate.require(result.returncode == 0, 'Cannot inspect the existing packaging policy; no build started')
    return validate_packaging_policy(json.loads(result.stdout))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--expected-commit', required=True)
    parser.add_argument('--expected-patch')
    parser.add_argument('--ffmpeg', type=Path)
    parser.add_argument('--build-only', action='store_true', help='Compile and verify identity without invoking a package script')
    parser.add_argument('--with-input-helper', action='store_true')
    args = parser.parse_args()
    gate.require(sys.platform == 'win32', 'Native Windows only')
    repo = Path(__file__).resolve().parents[1]
    gate.require(gate.git(repo, 'branch', '--show-current').strip() == b'main', 'Primary main required')
    env = gate.build_environment()
    package_env = packaging_environment(env)
    if not args.build_only:
        gate.require(args.ffmpeg is not None, '--ffmpeg is required for a formal package')
        packaging_preflight(repo, package_env)  # Fail before expensive compilation.
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--locked', '--offline', '--no-deps', '--format-version=1'], cwd=repo, env=env))
    target = Path(metadata['target_directory']).resolve(strict=True)
    gate.require(target == (repo / 'target').resolve(strict=True)
                 and Path(metadata.get('build_directory') or target).resolve() == target,
                 'Preserve the existing native Windows project-local cache')
    gate.require(shutil.disk_usage(target).free >= 4 * 1024**3, 'Four GiB reserve required')
    if args.ffmpeg is not None:
        gate.regular(args.ffmpeg)
    source = gate.source_snapshot(repo, args.expected_commit, expected_patch=args.expected_patch)
    config = gate.configuration('windows', 'x86_64', (repo/'VERSION').read_text().strip(),
        'x86_64-pc-windows-msvc', 'release', gate.toolchain_digest(), gate.environment_digest())
    expected = gate.identity(source, config)
    lock = target / '.weavelo-windows-current.lock'
    inputs = [target/'.weavelo-build-identity.json', target/'.weavelo-build-identity.rs']
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
        build_env = dict(env, WEAVELO_BUILD_IDENTITY_FILE=compiler_path(inputs[0]), WEAVELO_BUILD_IDENTITY_RS=compiler_path(inputs[1]))
        command = ['cargo','build','--release','-p','weavelo_app','--bin','weavelo',
            '--no-default-features','--features','gpui-restoration,native-windows-video','--locked','--offline','-j','2']
        settled = False
        child = subprocess.Popen(command, cwd=repo, env=build_env, creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
        result = child.wait(); settled = True
        gate.require(result == 0, 'Build failed; no receipt minted')
        gate.require(gate.source_snapshot(repo,args.expected_commit,expected_patch=args.expected_patch)==source,
                     'Source changed during compilation')
        gate.require(gate.toolchain_digest()==config['toolchain_sha256']
                     and gate.environment_digest()==config['environment_sha256'], 'Compiler environment changed')
        binary = target/'release/weavelo.exe'
        binary_hash = gate.hash_file(binary)
        info = gate.product_info(binary, expected, binary_hash)
        helper_hash = None
        if args.with_input_helper:
            helper_command = ['cargo','build','--release','-p','weavelo_app','--example',
                'window_scoped_acceptance_guarded','--no-default-features','--features',
                'gpui-restoration,native-windows-video','--locked','--offline','-j','2']
            settled = False
            child = subprocess.Popen(helper_command, cwd=repo, env=build_env, creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
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
        if args.build_only:
            print(json.dumps({'stage':'build-only','package_generated':False,
                'commit':args.expected_commit,'binary_sha256':binary_hash,'receipt':str(receipt_path)}))
            return
        subprocess.run(['powershell.exe','-NoProfile','-File',str(repo/'scripts/package_windows_unified.ps1'),
            '-SkipBuild','-BuildReceipt',str(receipt_path),'-FfmpegPath',str(args.ffmpeg)],cwd=repo,env=package_env,check=True)
        gate.require(gate.source_snapshot(repo,args.expected_commit,expected_patch=args.expected_patch)==source,
                     'Source changed during packaging')
        print(json.dumps({'build_identity_sha256':expected['identity_sha256'],'commit':args.expected_commit,
            'binary_sha256':binary_hash,'receipt':str(receipt_path),'package':str(package/'Weavelo-Windows-x64.zip')}))
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
