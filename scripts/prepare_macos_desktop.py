#!/usr/bin/env python3
"""Prepare the independent bundle through the existing producer/public ledger.

This explicit mode never signs, installs, launches or mints a signed-release
receipt. The signing guard and both production installers reject its ZIP.
"""
import json
import os
from datetime import datetime, timezone
from pathlib import Path
import plistlib
import zipfile

import build_identity_gate as gate
import desktop_identity as desktop
import macos_release_guard as guard
import release_artifact_lifecycle as lifecycle

COMPONENT = 'macos-desktop-arm64-preparation'
NAME = 'Weavelo-desktop-macos-arm64.prepared.zip'
MARKER = 'weavelo-desktop-preparation.json'


def verify_prepared(archive, record):
    lifecycle.require(record.get('kind') == 'unsigned-desktop-package-preparation'
                      and record.get('installable') is False and record.get('signature_verified') is False
                      and record.get('desktop_identity') == desktop.identity(), 'Not desktop preparation evidence')
    value, size = lifecycle.hashed(archive)
    lifecycle.require(value == record['archive_sha256'] and size == record['size_bytes'],
                      'Prepared archive changed')
    with zipfile.ZipFile(archive) as source:
        entries = source.infolist()
        lifecycle.require(len(entries) == len(record['files']) + 1, 'Unexpected preparation payload')
        lifecycle.require(json.loads(source.read(MARKER))['installable'] is False, 'Preparation marker changed')
        for row in record['files']:
            entry = source.getinfo(row['path'])
            lifecycle.require(entry.file_size == row['bytes']
                              and gate.digest(source.read(entry)) == row['sha256'], 'Prepared file differs')
        info = plistlib.loads(source.read('Weavelo.app/Contents/Info.plist'))
        lifecycle.require(info['CFBundleIdentifier'] == desktop.identity()['application_id'], 'Old bundle ID')
    return value


def main():
    _, state = lifecycle.check_lock('macos')
    lifecycle.require(state.get('transition') is None, 'Preparation cannot consume a signed identity transition')
    # Independently pinned observed-build inputs; no import of an unobserved binary.
    required = ['WEAVELO_BUILD_RECEIPT', 'WEAVELO_BUILD_RECEIPT_SHA256', 'WEAVELO_EXPECTED_COMMIT',
                'WEAVELO_EXPECTED_IDENTITY_SHA256', 'WEAVELO_PREBUILT_BINARY', 'WEAVELO_PREBUILT_SHA256']
    lifecycle.require(all(os.environ.get(key) for key in required), 'Observed build handoff required')
    observed = gate.read_json(Path(os.environ['WEAVELO_BUILD_RECEIPT']), os.environ['WEAVELO_BUILD_RECEIPT_SHA256'])
    lifecycle.require(observed.get('kind') == 'observed-build', 'Actual product build receipt required')
    gate.validate_identity(observed['build_identity'], os.environ['WEAVELO_EXPECTED_COMMIT'],
                           os.environ['WEAVELO_EXPECTED_IDENTITY_SHA256'])
    binary = Path(os.environ['WEAVELO_PREBUILT_BINARY'])
    lifecycle.require(gate.hash_file(binary) == observed['compiler_binary_sha256'] == os.environ['WEAVELO_PREBUILT_SHA256'],
                      'Compiler output differs from independent handoff')
    info = gate.product_info(binary, observed['build_identity'], observed['compiler_binary_sha256'])
    lifecycle.require(info == observed['product_info'] and info.get('desktop_identity') == desktop.identity(),
                      'Actual binary has no independent desktop identity')
    output = lifecycle.macos_output_root()
    archive = output / NAME; receipt = archive.with_suffix('.verification.json')
    files = [archive, receipt]
    current = None
    if any(p.exists() or p.is_symlink() for p in files):
        recorded, receipt_sha = lifecycle.document(receipt)
        current = [verify_prepared(archive, recorded), receipt_sha]
    admission = lifecycle.admit(state, COMPONENT, files, current=current,
        aux_stage=output / '.weavelo-desktop-preparation.pending')
    stage = Path(admission['owned_aux_stages'][0])
    staged_archive, staged_receipt = map(Path, admission['staged_files'])
    version = observed['build_identity']['configuration']['version']
    build = os.environ.get('WEAVELO_BUILD_NUMBER') or datetime.now(timezone.utc).strftime('%Y%m%d') + '.1'
    app = guard.assemble_app(stage, binary, version, build, guard.load_policy(guard.DESKTOP_POLICY_FILE))
    rows = [{'path':str(p.relative_to(stage)), 'sha256':gate.hash_file(p), 'bytes':p.stat().st_size}
            for p in sorted(app.rglob('*')) if p.is_file()]
    marker = {'schema':1, 'kind':'unsigned-desktop-package-preparation', 'installable':False,
              'signature_verified':False, 'desktop_identity':desktop.identity(),
              'source_commit':os.environ['WEAVELO_EXPECTED_COMMIT'],
              'identity_sha256':os.environ['WEAVELO_EXPECTED_IDENTITY_SHA256']}
    with zipfile.ZipFile(staged_archive, 'x', compression=zipfile.ZIP_DEFLATED) as outgoing:
        for row in rows: outgoing.write(stage / row['path'], row['path'])
        outgoing.writestr(MARKER, json.dumps(marker, indent=2) + '\n')
    record = dict(marker, archive=str(archive), archive_sha256=gate.hash_file(staged_archive),
                  size_bytes=staged_archive.stat().st_size, files=rows,
                  observed_build_receipt_sha256=os.environ['WEAVELO_BUILD_RECEIPT_SHA256'],
                  retained_installation_modified=False)
    with staged_receipt.open('x') as outgoing: json.dump(record, outgoing, indent=2); outgoing.write('\n')
    verify_prepared(staged_archive, record)
    args = ['--component', COMPONENT, '--producer-pid', state['producer_pid'], '--validation-kind', 'unsigned-dev',
            '--validation-file', staged_receipt, '--validation-digest', gate.hash_file(staged_receipt)]
    for path, value in zip(files, [gate.hash_file(staged_archive), gate.hash_file(staged_receipt)]):
        args.extend(['--file', path, '--expected-sha256', value])
    result = lifecycle.public('release-publish', *args)
    lifecycle.require(result.get('current_committed') is True and result.get('paths') == list(map(str, files)),
                      'Preparation publication unconfirmed; observe original transaction')
    print(json.dumps({'prepared':True,'installable':False,'archive':str(archive),
        'archive_sha256':record['archive_sha256'],'receipt':str(receipt),
        'current_committed':True,'cleanup_status':result.get('cleanup_status')}))


if __name__ == '__main__':
    main()
