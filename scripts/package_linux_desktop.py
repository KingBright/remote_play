#!/usr/bin/env python3
"""Archive a pinned, already observed native Linux ELF. Never build or install.

Uses the existing producer lock and sole public current-output authority.
The original observed-build/source identity remains unchanged by packaging.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import zipfile

import build_identity_gate as gate
import desktop_identity as desktop
import install_linux_independent as install
import release_artifact_lifecycle as lifecycle

COMPONENT='linux-desktop-x64-release'
NAME='Weavelo-desktop-linux-x64.zip'


def observed(args):
    receipt=gate.read_json(args.build_receipt,args.build_receipt_sha256)
    expected=gate.validate_identity(receipt['build_identity'],args.expected_commit,args.expected_identity_sha256)
    lifecycle.require(receipt.get('kind')=='observed-build' and receipt.get('schema')==2
        and expected['configuration']['platform']=='linux'
        and expected['configuration']['architecture']=='x86_64','Observed native Linux x64 receipt required')
    lifecycle.require(gate.hash_file(args.binary)==args.binary_sha256==receipt['compiler_binary_sha256'],
                      'Pinned compiler output changed')
    info=gate.product_info(args.binary,expected,args.binary_sha256)
    lifecycle.require(info==receipt['product_info'] and info.get('desktop_identity')==desktop.identity(),
                      'Independent native product identity required')
    lifecycle.require(info.get('desktop_runtime')=={'control_port':39273,'discovery_port':38119,
        'implicit_relay':False,'implicit_p2p':False},'Independent runtime defaults required')
    return receipt


def assemble(stage,binary,receipt,commit,identity_sha):
    app=stage/'Weavelo';app.mkdir(mode=0o700)
    for name in sorted(install.PAYLOAD):
        source = binary if name=='weavelo' else (
            desktop.ROOT/'assets/brand/weavelo/app-square.svg' if name=='assets/icon.svg' else desktop.ROOT/name)
        gate.regular(source)
        target=app/name;target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copyfile(source,target)
        target.chmod(0o755 if name=='weavelo' else 0o644)
    return gate.seal_tree(stage,receipt,commit,identity_sha)


def archive(stage,destination):
    with zipfile.ZipFile(destination,'x',compression=zipfile.ZIP_DEFLATED) as output:
        for path in sorted(stage.rglob('*')):
            if path.is_file():output.write(path,path.relative_to(stage).as_posix())


def verify_record(archive,receipt):
    lifecycle.require(receipt.get('kind')=='weavelo-independent-linux-package'
        and receipt.get('desktop_identity')==desktop.identity(),'Independent Linux package receipt required')
    manifest=install.verify(archive,receipt['archive_sha256'],receipt['manifest_sha256'],
        receipt['source_commit'],receipt['identity_sha256'])
    lifecycle.require(manifest['binary_sha256']==receipt['binary_sha256'], 'Package compiler binding changed')
    return gate.hash_file(archive)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--build-receipt',type=Path,required=True)
    for name in ('binary-sha256','build-receipt-sha256','expected-commit','expected-identity-sha256'):
        parser.add_argument('--'+name,required=True)
    args=parser.parse_args()
    receipt=observed(args)  # Reject bad pins before creating output/transaction state.
    with lifecycle.producer('linux-desktop') as (_,state):
        root=lifecycle.linux_output_root();final=root/NAME;report=final.with_suffix('.verification.json')
        current=None
        if any(p.exists() or p.is_symlink() for p in (final,report)):
            old,old_sha=lifecycle.document(report)
            current=[verify_record(final,old),old_sha]
        admission=lifecycle.admit(state,COMPONENT,[final,report],current=current,
            aux_stage=root/'.weavelo-desktop-linux.pending')
        stage=Path(admission['owned_aux_stages'][0]);staged_zip,staged_report=map(Path,admission['staged_files'])
        sealed=assemble(stage,args.binary,receipt,args.expected_commit,args.expected_identity_sha256)
        archive(stage,staged_zip)
        record={'schema':1,'kind':'weavelo-independent-linux-package','archive':str(final),
            'archive_sha256':gate.hash_file(staged_zip),'archive_bytes':staged_zip.stat().st_size,
            'manifest_sha256':sealed['manifest_sha256'],'source_commit':args.expected_commit,
            'identity_sha256':args.expected_identity_sha256,'binary_sha256':args.binary_sha256,
            'observed_build_receipt_sha256':args.build_receipt_sha256,'desktop_identity':desktop.identity(),
            'packaging_source_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=desktop.ROOT,text=True).strip(),
            'signature_verified':False,'installation_applied':False,'service_started':False,
            'visual_acceptance':'not_evaluated','stream_acceptance':'not_evaluated'}
        gate.write_new(staged_report,record)
        verify_record(staged_zip,record)
        # Hashes bind the observed source and exact archive. They are not a signature.
        arguments=['--component',COMPONENT,'--producer-pid',state['producer_pid'],
            '--validation-kind','unsigned-dev','--validation-file',staged_report,
            '--validation-digest',gate.hash_file(staged_report)]
        for path,value in zip([final,report],[gate.hash_file(staged_zip),gate.hash_file(staged_report)]):
            arguments.extend(['--file',path,'--expected-sha256',value])
        result=lifecycle.public('release-publish',*arguments)
        lifecycle.require(result.get('current_committed') is True
            and result.get('paths')==list(map(str,[final,report])), 'Package commit unconfirmed; inspect original transaction')
        print(json.dumps(dict(record,receipt=str(report),receipt_sha256=gate.hash_file(report),
            current_committed=True,cleanup_status=result.get('cleanup_status')),indent=2))


if __name__=='__main__':main()
