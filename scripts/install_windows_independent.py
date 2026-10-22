#!/usr/bin/env python3
"""First independent Windows Desktop install from the original formal package.

Never packages, changes execution policy, stops an old client, imports membership,
starts a GUI/service or repairs existing permissions. A journal blocks replay.
"""
import argparse
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys

import build_identity_gate as gate
import desktop_identity as desktop
import install_native_first_identity as original
import windows_private_profile as private

ROOT = Path(__file__).resolve().parents[1]


def paths(home):
    home = Path(home).absolute()
    return {'app': home/'AppData/Local/Weavelo/Desktop',
            'profile': home/'AppData/Roaming/Weavelo/Desktop',
            'state': home/'AppData/Local/Weavelo/DesktopInstall',
            'stage': home/'AppData/Local/Weavelo/.desktop-install.pending'}


def destinations(home):
    selected = paths(home)
    for path in selected.values():
        private.ordinary_path(path)
        if path.exists():
            raise RuntimeError('Independent destination or journal exists; inspect --status')
    return selected


def revalidate(bound):
    gate.require(gate.hash_file(bound['receipt']) == bound['receipt_sha256'],
                 'Observed producer receipt changed')
    receipt = gate.read_json(bound['receipt'], bound['receipt_sha256'])
    expected = gate.validate_identity(receipt['build_identity'], bound['commit'], bound['identity_sha256'])
    gate.require(gate.product_info(bound['compiler'], expected, bound['binary_sha256']) == receipt['product_info'],
                 'Actual compiler differs from observed receipt')
    gate.require(receipt['product_info'].get('desktop_identity') == desktop.identity(),
                 'Independent desktop runtime required')
    for path in bound['candidate'].rglob('*'):
        private.ordinary_path(path)
    files = original.reviewed_windows_payload(gate, bound['candidate'], bound['archive'], receipt,
        {'archive':bound['archive_sha256'], 'binary':bound['binary_sha256'],
         'receipt':bound['receipt_sha256']}, independent=True)
    return files


def reviewed(args):
    gate.require(gate.git(ROOT, 'branch', '--show-current').strip() == b'main',
                 'Use the primary main installer checkout')
    gate.require(not gate.git(ROOT, 'status', '--porcelain=v1', '-uall').strip(),
                 'Reviewed installer source must be committed')
    receipt_path = ROOT/'target/package/windows/windows-current-build-receipt.json'
    receipt = gate.read_json(receipt_path, args.build_receipt_sha256)
    gate.require(receipt.get('schema') == 2 and receipt.get('kind') == 'observed-build',
                 'Native observed Windows build required')
    expected = gate.validate_identity(receipt['build_identity'], args.expected_commit, args.expected_identity_sha256)
    gate.require(expected['configuration']['platform'] == 'windows'
                 and expected['configuration']['architecture'] == 'x86_64'
                 and expected['configuration']['target'] == 'x86_64-pc-windows-msvc'
                 and expected['configuration']['profile'] == 'release', 'Wrong Windows producer')
    bound = {'receipt':receipt_path, 'receipt_sha256':gate.sha(args.build_receipt_sha256),
             'archive':ROOT/'target/package/windows/desktop/Weavelo-Windows-x64.zip',
             'candidate':ROOT/'target/package/windows/desktop/Weavelo',
             'compiler':ROOT/'target/release/weavelo.exe',
             'archive_sha256':gate.sha(args.expected_archive_sha256),
             'binary_sha256':gate.sha(receipt['compiler_binary_sha256']),
             'identity_sha256':gate.sha(args.expected_identity_sha256),
             'commit':gate.sha(args.expected_commit, 40)}
    bound['files'] = revalidate(bound)
    return bound


def plan(home, bound):
    selected = destinations(home)
    body = {'schema':1, 'kind':'independent-weavelo-windows-desktop-install',
            'application_id':desktop.identity()['application_id'],
            'installer_source_commit':gate.git(ROOT, 'rev-parse', 'HEAD').decode().strip(),
            'paths':{key:str(value) for key,value in selected.items()},
            'runtime_commit':bound['commit'], 'identity_sha256':bound['identity_sha256'],
            'binary_sha256':bound['binary_sha256'], 'archive_sha256':bound['archive_sha256'],
            'build_receipt_sha256':bound['receipt_sha256'], 'payload':bound['files'],
            'installer_files_sha256':{name:gate.hash_file(ROOT/'scripts'/name) for name in
                ('install_windows_independent.py','install_native_first_identity.py',
                 'windows_private_profile.py','desktop_identity.py','build_identity_gate.py')},
            'retained_clients_modified':False, 'membership_imported':False,
            'security_policy_changed':False, 'service_or_GUI_started':False,
            'startup_modified':False, 'applied':False}
    return dict(body, plan_sha256=hashlib.sha256(original.canonical(body)).hexdigest())


def move_no_replace(source, destination):
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    move = kernel.MoveFileExW
    move.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD]
    move.restype = wintypes.BOOL
    if not move(str(source), str(destination), 0):
        raise ctypes.WinError(ctypes.get_last_error())


def apply(home, bound, expected_plan):
    initial = plan(home, bound)
    gate.require(initial['plan_sha256'] == gate.sha(expected_plan), 'Installation plan changed')
    gate.require(revalidate(bound) == bound['files'], 'Formal payload changed')
    selected = destinations(home)
    native = private.NativeSecurity()
    # Only missing structural parents are created; old app/profile ACLs stay intact.
    for parent in (selected['state'].parent, selected['profile'].parent):
        private.ordinary_path(parent)
        parent.mkdir(parents=True, exist_ok=True)
        private.ordinary_path(parent)
    native.create_private_directory(selected['state'])  # exclusive transaction owner
    journal = selected['state']/'install.json'
    record = dict(initial)
    def checkpoint(phase, initial_write=False):
        record['phase'] = phase
        original.atomic_state(journal, record, initial=initial_write)
    checkpoint('verified_not_started', True)
    shutil.copytree(bound['candidate'], selected['stage'], symlinks=False)
    gate.require(original.inventory(selected['stage'], gate) == bound['files'], 'Staged payload differs')
    receipt = gate.read_json(bound['receipt'], bound['receipt_sha256'])
    gate.require(gate.product_info(selected['stage']/'weavelo.exe', receipt['build_identity'],
                 bound['binary_sha256']) == receipt['product_info'], 'Staged executable differs')
    gate.require(revalidate(bound) == bound['files'], 'Producer changed before exclusive move')
    private.ordinary_path(selected['app'])
    checkpoint('moving_new_app')
    move_no_replace(selected['stage'], selected['app'])
    checkpoint('app_installed')
    native.create_private_directory(selected['profile'])  # never repair/adopt an existing profile
    record.update(applied=True, profile_private_at_creation=True)
    checkpoint('committed_not_started')
    gate.require(original.inventory(selected['app'], gate) == bound['files'], 'Installed payload differs')
    gate.require(json.loads(journal.read_text()) == record, 'Install journal readback differs')
    return record


def status(home):
    path = paths(home)['state']/'install.json'
    private.ordinary_path(path)
    if not path.exists():
        return {'state':'incomplete_installation_state' if path.parent.exists() else 'not_started'}
    gate.regular(path)
    gate.require(path.stat().st_size <= 65536, 'Install journal exceeds bound')
    native = private.NativeSecurity()
    parent = native.snapshot(path.parent)
    security = native.snapshot(path)
    allowed = {native.user, private.SYSTEM, private.ADMINISTRATORS}
    gate.require(parent['owner'] == native.user and parent['dacl_protected']
                 and security['owner'] == native.user
                 and all(ace['type'] in (0,1) and (ace['type'] == 1 or ace['sid'] in allowed)
                         for item in (parent,security) for ace in item['aces']),
                 'Install journal is not private')
    value = json.loads(path.read_text())
    gate.require(value.get('schema') == 1
                 and value.get('kind') == 'independent-weavelo-windows-desktop-install'
                 and value.get('paths') == {k:str(v) for k,v in paths(home).items()},
                 'Unknown installation journal; no replay')
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--status', action='store_true')
    parser.add_argument('--apply', action='store_true')
    for name in ('build-receipt-sha256','expected-archive-sha256','expected-commit',
                 'expected-identity-sha256','expected-plan-sha256'):
        parser.add_argument('--'+name)
    args = parser.parse_args()
    gate.require(sys.platform == 'win32', 'Native Windows only')
    if args.status:
        gate.require(not args.apply, 'Status cannot apply')
        result = status(Path.home())
    else:
        gate.require(all((args.build_receipt_sha256,args.expected_archive_sha256,
                          args.expected_commit,args.expected_identity_sha256)), 'Independent producer pins required')
        gate.require(not args.apply or bool(args.expected_plan_sha256), 'Apply requires a reviewed plan SHA256')
        bound = reviewed(args)
        result = apply(Path.home(), bound, args.expected_plan_sha256) if args.apply else plan(Path.home(), bound)
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    try:
        main()
    except (RuntimeError, ValueError, OSError, gate.IdentityRejected) as error:
        print(json.dumps({'applied':None if '--apply' in sys.argv else False,
                          'error':type(error).__name__, 'detail':str(error),
                          'next_action':'Inspect original --status before retrying an uncertain installation'}),
              file=sys.stderr)
        raise SystemExit(1)
