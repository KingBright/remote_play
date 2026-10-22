#!/usr/bin/env python3
"""Fresh independent Weavelo install. No retained-app migration or rollback engine.

Default is a read-only plan. Apply accepts only an identity-bound, original-signer
release archive. Existing destinations/journals are reported, never overwritten.
Use --upgrade for the separately version-bound normal upgrade transaction.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import stat
import plistlib
import shutil
import sys
import tempfile
import zipfile

import build_identity_gate as gate
import desktop_identity as desktop
import macos_release_guard as guard


def plan(home):
    if os.getuid() == 0 or os.geteuid() == 0:
        raise guard.ReleaseRejected('Use the logged-in user; no root installer')
    selected = desktop.check_collision(home)
    return {'kind':'independent-weavelo-desktop-install','application_id':desktop.identity()['application_id'],
            'paths':{k:str(v) for k,v in selected.items()}, 'launch_agent':desktop.launch_agent(home),
            'retained_client_modified':False,'applied':False,'service_started':False,
            'pairing_imported':False,'os_permissions_changed':False}


def private_directory(path):
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    for parent in (path, *path.parents):
        if parent == Path.home().parent: break
        if parent.is_symlink(): raise guard.ReleaseRejected('Linked installer destination')


def verified_product(candidate, archive, pins):
    with zipfile.ZipFile(archive) as source:
        raw=source.read(gate.MANIFEST)
    manifest=gate.manifest_document(raw,pins['manifest_sha256'],pins['commit'],pins['identity_sha256'])
    product=gate.product_info(candidate/'Contents/MacOS/weavelo',manifest['build_identity'],manifest['binary_sha256'])
    if product!=manifest['product_info'] or product.get('desktop_identity')!=desktop.identity():
        raise guard.ReleaseRejected('Actual executable differs from pinned independent release manifest')
    return product


def apply(archive, home, pins, archive_sha256):
    # All source/signature checks precede any destination write or profile setup.
    if gate.hash_file(archive) != gate.sha(archive_sha256):
        raise guard.ReleaseRejected('Archive differs from independent digest')
    policy = guard.load_policy(guard.DESKTOP_POLICY_FILE)
    with guard.verified_archive(archive, policy, identity_expectations=pins) as (candidate, signed):
        initial = plan(home); selected = desktop.paths(home)
        product = verified_product(candidate,archive,pins)
        if product.get('desktop_identity') != desktop.identity():
            raise guard.ReleaseRejected('Candidate does not use the independent runtime namespace')
        # Fixed user-only lock outside any retained app/profile. A prior journal
        # blocks replay after any partial/uncertain install; --status reads it.
        parent = selected['state'].parent
        private_directory(parent)
        lock = parent / '.desktop-install.lock'
        fd = os.open(lock, os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'r+') as held:
            fcntl.flock(held, fcntl.LOCK_EX|fcntl.LOCK_NB)
            plan(home)  # Recheck under the fixed lock.
            selected['state'].mkdir(mode=0o700)
            journal = selected['state']/'install.json'
            record = dict(initial, archive_sha256=archive_sha256, source=pins, phase='verified')
            def checkpoint(phase):
                record['phase']=phase
                raw=json.dumps(record,indent=2)+'\n'
                descriptor=os.open(journal,os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_NOFOLLOW,0o600)
                with os.fdopen(descriptor,'w') as out:out.write(raw);out.flush();os.fsync(out.fileno())
            checkpoint('verified')
            private_directory(selected['app'].parent)
            with tempfile.TemporaryDirectory(prefix='.desktop-install-', dir=selected['app'].parent) as temporary:
                staged=Path(temporary)/'Weavelo.app'
                shutil.copytree(candidate,staged,symlinks=False)
                guard.verify_app(staged,policy)
                if gate.files_in(staged)!=gate.files_in(candidate):
                    raise guard.ReleaseRejected('Staged signed bundle differs')
                if gate.hash_file(archive)!=archive_sha256:
                    raise guard.ReleaseRejected('Archive changed before installation')
                if selected['app'].exists() or selected['app'].is_symlink():
                    raise guard.ReleaseRejected('Independent app appeared during installation')
                checkpoint('moving_new_app')
                # renamex_np(RENAME_EXCL) refuses a destination race atomically.
                import ctypes
                native=ctypes.CDLL('/usr/lib/libSystem.B.dylib',use_errno=True)
                native.renamex_np.argtypes=[ctypes.c_char_p,ctypes.c_char_p,ctypes.c_uint]
                native.renamex_np.restype=ctypes.c_int
                if native.renamex_np(os.fsencode(staged),os.fsencode(selected['app']),4)!=0:
                    raise OSError(ctypes.get_errno(),'Exclusive independent install failed')
            checkpoint('app_installed')
            selected['profile'].mkdir(mode=0o700)
            private_directory(selected['plist'].parent)
            descriptor=os.open(selected['plist'],os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
            with os.fdopen(descriptor,'wb') as out:out.write(plistlib.dumps(initial['launch_agent']));out.flush();os.fsync(out.fileno())
            record.update(applied=True, signed_application_verified=True, service_started=False)
            checkpoint('committed_not_started')
            return record


def main():
    if '--upgrade' in sys.argv[1:]:
        import upgrade_macos_desktop
        sys.argv.remove('--upgrade')
        return upgrade_macos_desktop.cli()
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive',nargs='?',type=Path)
    parser.add_argument('--apply',action='store_true')
    parser.add_argument('--status',action='store_true')
    for key in ('commit','identity-sha256','manifest-sha256','archive-sha256'):
        parser.add_argument('--expected-'+key)
    args=parser.parse_args();home=Path.home()
    if args.status:
        path=desktop.paths(home)['state']/'install.json'
        if path.is_symlink(): raise guard.ReleaseRejected('Linked installation state')
        if path.exists():
            gate.regular(path);info=path.stat()
            if info.st_uid!=os.getuid() or stat.S_IMODE(info.st_mode)!=0o600 or info.st_size>65536:
                raise guard.ReleaseRejected('Independent install journal must be private and bounded')
            result=json.loads(path.read_text())
        else:result={'state':'incomplete_installation_state' if path.parent.exists() else 'not_started'}
    elif args.apply:
        if sys.platform!='darwin': raise guard.ReleaseRejected('Native macOS install required')
        if not args.archive or not all((args.expected_commit,args.expected_identity_sha256,
                                       args.expected_manifest_sha256,args.expected_archive_sha256)):
            raise guard.ReleaseRejected('Independent signed archive/source/manifest pins required')
        pins={'commit':args.expected_commit,'identity_sha256':args.expected_identity_sha256,
              'manifest_sha256':args.expected_manifest_sha256}
        result=apply(args.archive,home,pins,args.expected_archive_sha256)
    else:result=plan(home)
    print(json.dumps(result,indent=2))


if __name__=='__main__':
    try:main()
    except (guard.ReleaseRejected,gate.IdentityRejected,ValueError,OSError) as error:
        print(json.dumps({'applied':None if '--apply' in sys.argv[1:] else False,
            'outcome':'not_confirmed' if '--apply' in sys.argv[1:] else 'not_applied',
            'error':type(error).__name__,'detail':str(error),
            'next_action':'inspect original --status before retrying an uncertain installation'}),file=sys.stderr)
        sys.exit(1)
