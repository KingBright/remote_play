#!/usr/bin/env python3
"""Read-only combined root/delivery transition proposal. There is deliberately no apply.

The installed public helper has no current-identity transition API. This planner
does not edit that helper, authorities, registry, artifacts, Git or directories.
"""
import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
from types import SimpleNamespace
import tomllib
import build_identity_gate as gate

METADATA = ('docs/plans/ARTIFACT-LIFECYCLE-ADOPTION.json', '.rust-storage/adoption.json',
            '.rust-storage/release-binding.json', '.rust-storage/release-deliveries.json')
COMPONENT = 'macos-arm64-release'


def require(ok, message):
    if not ok: raise ValueError(message)


def sha(raw): return hashlib.sha256(raw).hexdigest()


def top_value_span(text, wanted):
    decoder=json.JSONDecoder();i=text.index('{')+1
    while True:
        while text[i].isspace() or text[i]==',':i+=1
        if text[i]=='}':return None
        key,i=decoder.raw_decode(text,i)
        while text[i].isspace():i+=1
        require(text[i]==':','Invalid metadata object');i+=1
        while text[i].isspace():i+=1
        start=i;_,i=decoder.raw_decode(text,i)
        if key==wanted:return start,i


def render(value, original):
    text=json.dumps(value,indent=2,ensure_ascii=False)+'\n'
    original_text=original.decode();old=top_value_span(original_text,'observed')
    if old:
        new=top_value_span(text,'observed')
        text=text[:new[0]]+original_text[old[0]:old[1]]+text[new[1]:]
    return text.encode()


def relocate(value, old, new):
    if isinstance(value,dict):
        return {k:(copy.deepcopy(v) if k=='observed' else relocate(v,old,new)) for k,v in value.items()}
    if isinstance(value,list):return [relocate(v,old,new) for v in value]
    if isinstance(value,str) and (value==old or value.startswith(old+'/')):return new+value[len(old):]
    return value


def installed_signature():
    folder=Path.home()/'.codex/skills/rust-project-lifecycle/scripts'
    helper=folder/'rust_storage.py';companion=folder/'rust_release.py'
    for p in [helper,companion]:
        gate.regular(p);m=p.stat()
        require(m.st_uid==os.getuid() and not m.st_mode&0o022,'Installed lifecycle code is not trusted/user-owned')
    spec=importlib.util.spec_from_file_location('weavelo_readonly_release',companion)
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    signature=lambda path:module.signature(SimpleNamespace(Refusal=ValueError),path)
    return signature,{str(p):gate.hash_file(p) for p in [helper,companion]}


def build_plan(repo, destination, signature, helper_identity):
    require(repo.is_absolute() and repo==repo.resolve(strict=True),'Source root must be canonical')
    require(destination.is_absolute() and '..' not in destination.parts
            and destination.parent==repo.parent and not destination.exists() and not destination.is_symlink(),
            'Destination must be the absent sibling, never a second checkout')
    require(repo.stat().st_dev==destination.parent.stat().st_dev,'Rename must retain the same filesystem/inode')
    raw={n:gate.regular(repo/n).read_bytes() for n in METADATA}
    data={n:gate.json_bytes(v) for n,v in raw.items()}
    authority,binding,release_binding,registry=(data[n] for n in METADATA)
    require(not registry.get('current_update') and not registry.get('pending_delivery'),'Active/unknown delivery transaction')
    for n in ['release-setup-transaction.json','release-retention-transaction.json']:
        require(not (repo/'.rust-storage'/n).exists(),'Unfinished official transaction: '+n)
    require(registry.get('entries')==[] and registry.get('archived_entries')==[],
            'Nonempty legacy ownership requires a separate explicit mapping, never bulk rewrite')
    record=copy.deepcopy(registry['current_components'][COMPONENT]);paths=list(map(Path,record['files']))
    require(len(paths)==2 and paths[0].name=='RemotePlay-macos-arm64.zip'
            and paths[1]==paths[0].with_suffix('.verification.json'),'Unexpected old current group')
    signatures=[signature(p) for p in paths]
    require(signatures==record['signatures'],'Official old current signatures drifted')
    old_files=[{'path':str(p),'sha256':gate.hash_file(p),'bytes':p.stat().st_size,
                'official_signature':sig} for p,sig in zip(paths,signatures)]
    receipt=gate.read_json(paths[1])
    require(record['validation_file']==str(paths[1]) and record['validation_digest']==old_files[1]['sha256']
            and receipt['archive']==str(paths[0]) and receipt['archive_sha256']==old_files[0]['sha256']
            and receipt['size_bytes']==old_files[0]['bytes'] and receipt['signature_verified'] is True
            and receipt['bundle_id']=='com.remoteplay.unified','Original signer receipt does not bind the old pair')
    proposed={n:relocate(d,str(repo),str(destination)) for n,d in data.items()}
    proposed[METADATA[0]]['project']='Weavelo'
    if proposed[METADATA[0]].get('owner')=='RemotePlay':proposed[METADATA[0]]['owner']='Weavelo'
    # Official declaration derives delivery project_id from the canonical root.
    new_id='delivery-'+sha(str(destination).encode())[:24]
    proposed[METADATA[0]]['release_retention']['project_id']=new_id
    proposed[METADATA[2]]['declaration']=copy.deepcopy(proposed[METADATA[0]]['release_retention'])
    proposed[METADATA[3]]['project_id']=new_id
    authority_after=render(proposed[METADATA[0]],raw[METADATA[0]])
    proposed[METADATA[1]]['authority_inventory']['sha256']=sha(authority_after)
    assert proposed[METADATA[0]]['observed']==authority['observed']
    assert proposed[METADATA[3]]['current_components']==registry['current_components']
    for key in ['budgets','coverage','selected_artifact_refs','owned_budget_refs','owner_marker_sha256','contract_sha256']:
        assert proposed[METADATA[1]][key]==binding[key]
    rows=[]
    for n in METADATA:
        after=render(proposed[n],raw[n])
        rows.append({'path':n,'before_sha256':sha(raw[n]),'after_root_sha256':sha(after),
                     'before_text':raw[n].decode(),'after_root_text':after.decode(),
                     'writer':'official combined relocation/identity transition extension required'})
    old_record_sha=sha(gate.canonical(record));new_paths=[paths[0].with_name('Weavelo-macos-arm64.zip'),
                                                  paths[1].with_name('Weavelo-macos-arm64.verification.json')]
    require(all(not p.exists() and not p.is_symlink() for p in new_paths),'Unowned new default exists')
    for n in METADATA:require((repo/n).read_bytes()==raw[n],'Metadata changed while planning')
    require([signature(p) for p in paths]==signatures,'Old pair changed while planning')
    package=tomllib.loads((repo/'app/Cargo.toml').read_text())['package']
    git_dir=repo/'.git';g=git_dir.stat()
    plan={'schema':1,'mode':'proposal-only-no-apply','official_transition_api_available':False,
          'ready_for_mutation':False,'old_root':str(repo),'new_root':str(destination),
          'root_inode':[repo.stat().st_dev,repo.stat().st_ino],'git_inode':[g.st_dev,g.st_ino],
          'head':gate.git(repo,'rev-parse','HEAD').decode().strip(),
          'branch':gate.git(repo,'branch','--show-current').decode().strip(),
          'status_sha256':sha(gate.git(repo,'status','--porcelain=v1','-uall','-z')),
          'official_helper':helper_identity,'metadata':rows,
          'observed_preserved_byte_for_byte':True,
          'project_id_transition':{'before':registry['project_id'],'after':new_id,
                                   'rule':'official delivery- + SHA256(canonical root) first 24 hex; prior ID retained as provenance'},
          'current_observation_after_move':{'status':'pending official adopt binding',
               'package':package['name'],'version':package['version'],
               'manifest_sha256':gate.hash_file(repo/'app/Cargo.toml')},
          'delivery_transition':{'component':COMPONENT,'previous_record':record,
               'previous_record_sha256':old_record_sha,'previous_files':old_files,
               'previous_source':receipt.get('build_identity_expectations'),
               'new_files':list(map(str,new_paths)),
               'new_identity':{'bundle_id':'fun.hackerlife.weavelo','certificate_sha1':receipt['certificate_sha1']},
               'new_artifact_proof':{'status':'not_built','identity_sha256':None,'manifest_sha256':None,'archive_sha256':None},
               'history':'append complete previous record, source pins, file hashes and old/new root IDs; do not clear or relabel old records',
               'retention':'public entries mechanism: two unique_delivery rollback references to unchanged old paths; no generations/copies',
               'commit':'official publish atomically changes current only after verified new candidate; old pair retained unchanged',
               'rollback':'precommit abort retains old current; postcommit explicit CAS rollback requires unchanged new/old tuples and preserves both pairs'},
          'locks_in_order':['existing project release-producer.lock','existing fixed-validation.lock',
                            'official project lock + shared cleanup exclusion'],
          'phases':['lock and revalidate all preimages/inodes/HEAD','same-inode root rename + four metadata CAS',
                    'official observation refresh','official explicit transition admission preserving old current',
                    'formal build/sign/verify new package','official publish with history and rollback references'],
          'registry_overlap':'release-deliveries.json appears once in metadata; publish is a later journaled CAS overlay on its root-postimage, never replay the four-file patch',
          'blocked_by':['installed helper has no explicit identity-transition/relocation API',
                        'new signed artifact proof is not available','real mutation has not been approved/executed']}
    require(plan['branch']=='main','Only the existing main checkout')
    return {'plan':plan,'plan_sha256':sha(gate.canonical(plan))}


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--repo',type=Path,required=True);p.add_argument('--destination',type=Path,required=True)
    a=p.parse_args();signature,identity=installed_signature()
    print(json.dumps(build_plan(a.repo,a.destination,signature,identity),indent=2,ensure_ascii=False))


if __name__=='__main__':main()
