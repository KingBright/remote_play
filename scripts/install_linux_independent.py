#!/usr/bin/env python3
"""Verify and plan a fresh independent Linux install. No service or pairing actions.

Four independent package/source pins are mandatory. --apply additionally binds the
reviewed plan. A partial journal blocks replay; --status only reports that journal.
No retained client, configuration, desktop entry or service is ever replaced.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import shlex
import stat
import sys
import zipfile

import build_identity_gate as gate
import desktop_identity as desktop

PAYLOAD = {
    'weavelo', 'scripts/install_linux_independent.py', 'scripts/build_identity_gate.py',
    'scripts/verify_desktop_gui.py', 'scripts/desktop_identity.py',
    'deploy/desktop/product-identity.json', 'deploy/linux/weavelo-desktop',
    'deploy/linux/weavelo-desktop.service',
    'deploy/linux/fun.hackerlife.weavelo.desktop.desktop', 'assets/icon.svg',
}


def require(ok, reason):
    if not ok:
        raise ValueError(reason)


def paths(home):
    home = Path(home)
    require(home.is_absolute() and home == home.resolve() and '..' not in home.parts,
            'Canonical home required')
    require(not any(c.isspace() or c in '\\"%$`' for c in str(home)), 'Unsupported home path')
    app = home / '.local/opt/weavelo/desktop'
    profile = home / '.config/weavelo/desktop'
    return {'app':app, 'profile':profile,
        'launcher':home/'.local/bin/weavelo-desktop',
        'service':home/'.config/systemd/user/weavelo-desktop.service',
        'desktop':home/'.local/share/applications/fun.hackerlife.weavelo.desktop.desktop',
        'icon':home/'.local/share/icons/hicolor/scalable/apps/fun.hackerlife.weavelo.desktop.svg',
        'work':home/'.local/state/weavelo-desktop-install-independent',
        'stage':app.parent/'.desktop-install.pending'}


def check_paths(home):
    selected = paths(home)
    for target in selected.values():
        for p in (target, *target.parents):
            if p == Path(home).parent:
                break
            try:
                info = p.lstat()
            except FileNotFoundError:
                continue
            require(not stat.S_ISLNK(info.st_mode), 'Linked destination rejected')
            require(info.st_uid == os.getuid() and not info.st_mode & 0o022,
                    'Destination must be privately controlled by this user')
            require(p == target or stat.S_ISDIR(info.st_mode), 'Invalid destination ancestor')
        require(not target.exists(), 'Independent destination collision: ' + str(target))
    return selected


def verify(archive, archive_sha, manifest_sha, commit, identity_sha):
    require(gate.hash_file(archive) == gate.sha(archive_sha), 'Archive differs from pinned SHA256')
    result = gate.verify_package(archive, manifest_sha, commit, identity_sha)
    with zipfile.ZipFile(archive) as source:
        manifest = gate.manifest_document(source.read(gate.MANIFEST), manifest_sha, commit, identity_sha)
        require(manifest['build_identity']['configuration']['platform'] == 'linux'
                and manifest['build_identity']['configuration']['architecture'] == 'x86_64',
                'Native Linux x64 package required')
        require({row['path'] for row in manifest['files']} == PAYLOAD, 'Unexpected independent package files')
        info = manifest['product_info']
        require(info.get('desktop_identity') == desktop.identity(), 'Retained application namespace rejected')
        require(info.get('desktop_runtime') == {'control_port':39273,'discovery_port':38119,
            'implicit_relay':False,'implicit_p2p':False}, 'Unexpected runtime defaults')
        require(json.loads(source.read('Weavelo/deploy/desktop/product-identity.json')) == desktop.identity(),
                'Package identity disagrees with compiled identity')
        unit=source.read('Weavelo/deploy/linux/weavelo-desktop.service').decode()
        for required in ('ExecStart=%h/.local/opt/weavelo/desktop/weavelo --background-service',
            'Environment=WEAVELO_DEVICE_GROUP_DIR=%h/.config/weavelo/desktop',
            'Environment=WEAVELO_HOST_BIND_ADDR=0.0.0.0:39273',
            'Environment=WEAVELO_DISCOVERY_PORT=38119',
            'Environment=WEAVELO_RELAY=0','Environment=WEAVELO_P2P=0'):
            require(unit.splitlines().count(required)==1, 'Package service profile/defaults differ')
        keys=[line.split('=',1)[0]+'='+line.split('=',2)[1] for line in unit.splitlines()
              if line.startswith('Environment=')]
        require(len(keys)==len(set(keys)), 'Duplicate service environment setting')
    return manifest


def plan(home, archive, archive_sha, manifest_sha, commit, identity_sha):
    archive = Path(archive).absolute()
    manifest = verify(archive, archive_sha, manifest_sha, commit, identity_sha)
    selected = check_paths(home)
    return {'schema':1,'kind':'weavelo-independent-linux-first-install','home':str(home),
        'archive':str(archive),'archive_sha256':archive_sha,'manifest_sha256':manifest_sha,
        'source_commit':commit,'identity_sha256':identity_sha,'binary_sha256':manifest['binary_sha256'],
        'paths':{k:str(v) for k,v in selected.items()},'empty_profile':True,
        'pairing_imported':False,'service_started':False,'network_configured':False,
        'retained_installation_modified':False}


def plan_sha(plan):
    return gate.digest(gate.canonical(plan))


def launcher(home):
    p = paths(home)
    # Fixed namespace, even when started from a retained client or conflicting XDG environment.
    values = {'WEAVELO_DEVICE_GROUP_DIR':str(p['profile']), 'WEAVELO_HOST_BIND_ADDR':'0.0.0.0:39273',
              'WEAVELO_DISCOVERY_PORT':'38119','WEAVELO_RELAY':'0','WEAVELO_P2P':'0'}
    return ('#!/bin/sh\n' + ''.join('export '+k+'='+shlex.quote(v)+'\n' for k,v in values.items())
            + 'exec '+shlex.quote(str(p['app']/'weavelo'))+' "$@"\n').encode()


def payloads(home, archive):
    p = paths(home)
    with zipfile.ZipFile(archive) as source:
        icon = source.read('Weavelo/assets/icon.svg')
        service = source.read('Weavelo/deploy/linux/weavelo-desktop.service')
    entry = ('[Desktop Entry]\nType=Application\nName=Weavelo\nExec="'+str(p['launcher'])+'"\n'
        'Icon=fun.hackerlife.weavelo.desktop\nStartupWMClass=fun.hackerlife.weavelo.desktop\n'
        'Terminal=false\nCategories=Network;RemoteAccess;\n').encode()
    return {'launcher':(launcher(home),0o755),'service':(service,0o644),
            'desktop':(entry,0o644),'icon':(icon,0o644)}


def write_new(path, data, mode):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW, mode)
    with os.fdopen(fd,'wb') as output:
        output.write(data); output.flush(); os.fsync(output.fileno())


def save(journal, report):
    # Only the newly owned private journal is replaced; public files are exclusive creates.
    pending = journal.with_suffix('.pending')
    write_new(pending, (json.dumps(report,indent=2)+'\n').encode(),0o600)
    os.replace(pending,journal)


def apply(reviewed, expected_plan_sha):
    require(plan_sha(reviewed) == gate.sha(expected_plan_sha), 'Reviewed plan SHA256 required')
    home = Path(reviewed['home']); archive = Path(reviewed['archive'])
    current = plan(home, archive, reviewed['archive_sha256'], reviewed['manifest_sha256'],
                   reviewed['source_commit'],reviewed['identity_sha256'])
    require(current == reviewed, 'Installation plan changed')
    selected = paths(home)
    require(sys.platform.startswith('linux') and platform.machine() in ('x86_64','AMD64'), 'Native Linux x64 apply only')
    selected['work'].parent.mkdir(parents=True,exist_ok=True)
    selected['work'].mkdir(mode=0o700)  # Exclusive ownership: concurrent/replayed applies stop here.
    journal = selected['work']/'state.json'
    state = {'phase':'preparing','plan':reviewed,'plan_sha256':expected_plan_sha,'created':[]}
    save(journal,state)
    try:
        selected['stage'].parent.mkdir(parents=True,exist_ok=True)
        selected['stage'].mkdir(mode=0o700)
        manifest = verify(archive,reviewed['archive_sha256'],reviewed['manifest_sha256'],
                          reviewed['source_commit'],reviewed['identity_sha256'])
        with zipfile.ZipFile(archive) as source:
            for row in manifest['files']:
                data = source.read('Weavelo/'+row['path'])
                require(len(data)==row['bytes'] and gate.digest(data)==row['sha256'], 'Payload changed during extraction')
                write_new(selected['stage']/row['path'],data,0o755 if row['path']=='weavelo' else 0o644)
        require(gate.files_in(selected['stage']) == manifest['files'], 'Staged package differs')
        # Native no-replace move: a concurrently created target is never replaced.
        exclusive_rename(selected['stage'],selected['app'])
        state['created'].append(str(selected['app'])); save(journal,state)
        selected['profile'].parent.mkdir(parents=True,exist_ok=True)
        selected['profile'].mkdir(mode=0o700)
        state['created'].append(str(selected['profile'])); save(journal,state)
        for key,(data,mode) in payloads(home,archive).items():
            # Recheck ancestors after staging; do not follow a newly introduced link.
            for parent in selected[key].parents:
                if parent == home.parent: break
                require(not parent.is_symlink(), 'Destination ancestor changed')
            write_new(selected[key],data,mode)
            state['created'].append(str(selected[key])); save(journal,state)
        state['phase']='installed-not-started';save(journal,state)
        return state
    except BaseException:
        state['phase']='partial-inspect-original';save(journal,state)
        raise


def exclusive_rename(source,destination):
    import ctypes
    libc=ctypes.CDLL(None,use_errno=True)
    require(hasattr(libc,'renameat2'),'Native no-replace rename required')
    fn=libc.renameat2;fn.argtypes=[ctypes.c_int,ctypes.c_char_p,ctypes.c_int,ctypes.c_char_p,ctypes.c_uint]
    if fn(-100,os.fsencode(source),-100,os.fsencode(destination),1):
        raise OSError(ctypes.get_errno(),'Exclusive install rename failed',str(destination))


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive',type=Path,nargs='?')
    for name in ('archive-sha256','manifest-sha256','commit','identity-sha256'):
        parser.add_argument('--expected-'+name)
    parser.add_argument('--apply',action='store_true')
    parser.add_argument('--expected-plan-sha256')
    parser.add_argument('--status',action='store_true')
    args=parser.parse_args(); home=Path.home().resolve()
    if args.status:
        require(not args.apply and args.archive is None,'Status cannot perform an install')
        journal=paths(home)['work']/'state.json'
        require(not journal.is_symlink(),'Linked journal rejected')
        print(json.dumps(gate.read_json(journal) if journal.exists() else {'phase':'not-started'},indent=2));return
    if not args.archive or not all((args.expected_archive_sha256,args.expected_manifest_sha256,
                                   args.expected_commit,args.expected_identity_sha256)):
        parser.error('archive and four independent package/source pins are required')
    if args.apply and not args.expected_plan_sha256:
        parser.error('--apply requires --expected-plan-sha256')
    reviewed=plan(home,args.archive,args.expected_archive_sha256,args.expected_manifest_sha256,
                  args.expected_commit,args.expected_identity_sha256)
    result=apply(reviewed,args.expected_plan_sha256) if args.apply else {'plan':reviewed,'plan_sha256':plan_sha(reviewed)}
    print(json.dumps(result,indent=2))


if __name__=='__main__':
    main()
