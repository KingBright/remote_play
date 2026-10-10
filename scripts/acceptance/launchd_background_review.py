#!/usr/bin/env python3
"""Opt-in signed fixtures + real, independently labelled user launchd review.

Never touches the installed app, production launch agent, Remote Hosts or TCC.
Only the label created by this invocation is bootstrapped, crashed and removed.
Raw receipts stay in the explicitly selected private evidence directory.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import uuid
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import installer_scope
import install_macos_unified as installer
import installer_build_identity as identity_adapter
import build_identity_gate as identity_gate
from macos_release_guard import load_policy, verified_archive, verify_app, ReleaseRejected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--old-archive', type=Path, required=True)
    parser.add_argument('--new-archive', type=Path, required=True)
    parser.add_argument('--failed-upgrade-archive', type=Path, required=True)
    parser.add_argument('--evidence-dir', type=Path, required=True)
    parser.add_argument('--execute', action='store_true')
    parser.add_argument('--native-observer', type=Path)
    parser.add_argument('--native-observer-sha256')
    args = parser.parse_args()
    if bool(args.native_observer) != bool(args.native_observer_sha256):
        parser.error('Native observer and independently reviewed digest must be specified together')
    if not args.execute:
        print(json.dumps({'applied': False, 'scope': 'fresh private fixture + independent launchd label',
                          'manual_click_login_reboot': 'not_tested', 'production_modified': False}))
        return 0
    if sys.platform != 'darwin': parser.error('This real launchd review requires macOS')
    evidence = args.evidence_dir.resolve(); evidence.mkdir(mode=0o700, parents=True, exist_ok=False)
    root = Path(tempfile.mkdtemp(prefix='rp-ld.', dir='/tmp')).resolve()
    label = 'com.remoteplay.lifecycle-test.' + uuid.uuid4().hex
    scope = installer_scope.initialize(root, label)
    profile = installer.profile_directory(scope.home); profile.mkdir(mode=0o700, parents=True)
    workdir = root / 'work'; workdir.mkdir(mode=0o700)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]; sock.close()
    target = scope.home/'Applications/RemotePlay.app'; executable = target/'Contents/MacOS/remote_play'
    plist = scope.home/('Library/LaunchAgents/'+label+'.plist'); plist.parent.mkdir(mode=0o700, parents=True)
    environment = {'REMOTE_PLAY_DEVICE_GROUP_DIR': str(profile), 'REMOTE_PLAY_DISPLAY_NAME': 'Private lifecycle fixture',
                   'REMOTE_PLAY_HEADLESS': '1', 'REMOTE_PLAY_HOST_BIND_ADDR': f'127.0.0.1:{port}',
                   'REMOTE_PLAY_CLIENT_BIND_ADDR': '127.0.0.1:0', 'REMOTE_PLAY_DISCOVERY_PORT': '49157',
                   'REMOTE_PLAY_P2P': '0', 'REMOTE_PLAY_RELAY': '0', 'REMOTE_PLAY_DISCOVERY': '0',
                   'REMOTE_PLAY_CLIENT_RECEIVER': '0', 'REMOTE_PLAY_VIEWER_MEDIA': '0',
                   'REMOTE_PLAY_CLIPBOARD_SYNC': '0', 'REMOTE_PLAY_FILE_TRANSFER': '0', 'REMOTE_PLAY_TALKBACK': '0'}
    original = plistlib.dumps({'Label': label, 'ProgramArguments': [str(executable)],
                              'KeepAlive': True, 'RunAtLoad': True, 'ThrottleInterval': 3,
                              'WorkingDirectory': str(workdir), 'EnvironmentVariables': environment,
                              'StandardOutPath': str(evidence/'background.log'), 'StandardErrorPath': str(evidence/'background.log')})
    # A public-readable plist beside private runtime markers exercises metadata
    # preservation without copying the markers' more restrictive mode.
    plist.write_bytes(original); plist.chmod(0o644)
    (evidence/'background.log').touch(mode=0o600)
    receipt = {'scope_root': str(root), 'test_label': label, 'checks': {}, 'production_modified': False,
               'manual_click_verified': False, 'login_reboot_verified': False, 'pid_observations': [],
               'state': 'preparing'}
    receipt['product_build_identity_chain_verified'] = False
    gui = None
    def save():
        path=evidence/'launchd-receipt.json'; path.write_text(json.dumps(receipt,indent=2)+'\n'); path.chmod(0o600)
    def launchctl(action):
        # Scope validation is repeated at every external mutation.
        scope.validate()
        suffix = str(plist) if action == 'bootstrap' else f'gui/{os.getuid()}/{label}'
        command=['/bin/launchctl', action]
        if action=='bootstrap':command += [f'gui/{os.getuid()}',suffix]
        else:command += [suffix]
        p=subprocess.run(command,capture_output=True,text=True,timeout=20)
        if p.returncode:raise RuntimeError('Owned launchd fixture command failed: '+action)
    def state():return installer.service_state(f'gui/{os.getuid()}')
    def wait_running(old_pid=None):
        deadline=time.monotonic()+35
        while time.monotonic()<deadline:
            value=state()
            if value.get('running') and value.get('pid') and value['pid'] != old_pid and value.get('program')==str(executable):
                receipt['pid_observations'].append(value['pid']);save();return value
            time.sleep(.2)
        raise RuntimeError('Owned launchd fixture did not become running')
    def run(*arguments, timeout=20):
        env={k:v for k,v in os.environ.items() if not k.startswith('REMOTE_PLAY_')}; env.update(environment)
        # A manual GUI launch does not inherit the launch agent's headless role.
        if not arguments: env.pop('REMOTE_PLAY_HEADLESS', None)
        return subprocess.run([str(executable),*arguments],cwd=workdir,env=env,capture_output=True,text=True,timeout=timeout)
    def connection():
        p=run('--check-loopback-host',f'127.0.0.1:{port}')
        return p.returncode==0
    def wait_connection():
        for _ in range(20):
            if connection():return True
            time.sleep(.2)
        return False
    def identity_hashes():
        return {name:hashlib.sha256((profile/name).read_bytes()).hexdigest() for name in ('mesh.conf','mesh.secret')}
    def native_image(observed):
        result = identity_gate.verify_running_macos(observed['pid'], executable,
                    verify_app(target, load_policy())['executable_sha256'],
                    args.native_observer, args.native_observer_sha256)
        return result.get('actual_loaded_image_verified') is True
    def reject_manifest_only_claims():
        # A deliberately synthetic external record tests that a correctly signed
        # old product cannot acquire compiled identity from a new sidecar. This
        # is a negative fixture, never an observed-build or release attestation.
        negative=evidence/'manifest-only-negative.zip'
        with verified_archive(args.new_archive.resolve(),load_policy()) as (app,signed):
            app=app.resolve()
            metadata=subprocess.run([str(app/'Contents/MacOS/remote_play'),'--product-info-json'],
                                    capture_output=True,check=True,timeout=15)
            info=json.loads(metadata.stdout)
            config=identity_gate.configuration('macos',info['architecture'],signed['version'],
                    info['architecture']+'-apple-darwin','dev','a'*64,'b'*64)
            synthetic=identity_gate.identity(dict(commit='c'*40,git_tree='d'*40,state='clean',
                    patch_sha256=None,snapshot_sha256='e'*64,file_count=1),config)
            info['build_identity']=synthetic
            manifest=dict(schema=1,kind='remoteplay-release',build_identity=synthetic,
                    artifact_root='RemotePlay.app',executable='Contents/MacOS/remote_play',
                    product_info=info,files=identity_gate.files_in(app),
                    binary_sha256=signed['executable_sha256'],compiler_binary_sha256=signed['executable_sha256'],
                    signing={k:signed[k] for k in ('bundle_id','certificate_sha1','designated_requirement','version','signature_verified')})
            raw=json.dumps(manifest,sort_keys=True).encode()
            # The old ditto archive carries AppleDouble metadata. Build the
            # negative view from the verified App inventory so archive-layout
            # rejection cannot mask the missing compiled identity assertion.
            with zipfile.ZipFile(negative,'x') as output:
                for path in app.rglob('*'):
                    if path.is_file():output.write(path,'RemotePlay.app/'+path.relative_to(app).as_posix())
                output.writestr(identity_gate.MANIFEST,raw)
        expected=identity_adapter.Expectations('c'*40,synthetic['identity_sha256'],identity_gate.digest(raw),
                                              str(args.native_observer),args.native_observer_sha256)
        old_pid=state()['pid']; old_app=verify_app(target,load_policy());old_plist=plist.read_bytes()
        try:installer.install(negative.resolve(),True,separate_background=True,expected_release=expected)
        except identity_gate.IdentityRejected as error:
            rejected='expected compiled source/build identity' in str(error)
            receipt['negative_rejection_reason']=str(error)
        else:rejected=False
        receipt['checks']['manifest_only_cannot_retrofit_missing_compiled_identity']=rejected
        receipt['checks']['identity_preflight_rejection_preserves_running_app_and_plist']=(
                state()['pid']==old_pid and verify_app(target,load_policy())==old_app and plist.read_bytes()==old_plist)
    save()
    with installer_scope.isolated(root,label):
        try:
            with verified_archive(args.old_archive.resolve(),load_policy()) as (source,info):
                shutil.copytree(source,target);receipt['old_signed_identity']=installer.identity(info)
            launchctl('bootstrap');initial=wait_running()
            receipt['checks']['legacy_background_connects']=wait_connection()
            identities=identity_hashes()
            if args.native_observer is not None:
                reject_manifest_only_claims()
            original_metadata=(plist.stat().st_uid,plist.stat().st_gid,plist.stat().st_mode&0o777)
            # The exact guarded production transaction runs under explicit fixture scope.
            receipt['plan']=installer.install(args.new_archive.resolve(),False,separate_background=True)
            receipt['upgrade']=installer.install(args.new_archive.resolve(),True,separate_background=True)
            upgraded=wait_running(initial['pid'])
            receipt['checks']['migration_verified_background_role']=receipt['upgrade']['service'].get('background_role_verified') is True
            receipt['checks']['migration_preserves_identity']=identity_hashes()==identities
            receipt['checks']['migration_preserves_launch_agent_permissions']=(plist.stat().st_uid,plist.stat().st_gid,plist.stat().st_mode&0o777)==original_metadata
            receipt['checks']['migration_loopback_connects']=wait_connection()
            if args.native_observer is not None:
                receipt['checks']['native_backend_mapped_image_after_migration']=native_image(upgraded)
            duplicate=run('--background-service')
            receipt['checks']['second_host_rejected']=duplicate.returncode!=0
            # Existing integration hook exits the real GPUI loop normally. It is
            # not a substitute for a manually clicked close button.
            gui_env={k:v for k,v in os.environ.items() if not k.startswith('REMOTE_PLAY_')};gui_env.update(environment)
            gui_env.update(REMOTE_PLAY_RESTORED_TEST_OUTPUT=str(evidence/'gui-render.json'),REMOTE_PLAY_RESTORED_TEST_SECONDS='5')
            with (evidence/'gui.log').open('wb') as log:
                os.chmod(evidence/'gui.log',0o600)
                gui=subprocess.Popen([str(executable),'--gui'],cwd=workdir,env=gui_env,stdout=log,stderr=subprocess.STDOUT)
                gui.wait(timeout=30)
            rendered=json.loads((evidence/'gui-render.json').read_text())
            receipt['checks']['gui_normal_exit']=gui.returncode==0 and rendered['render_calls']>0
            receipt['checks']['gui_exit_does_not_restart_service']=state().get('pid')==upgraded['pid']
            receipt['checks']['background_connects_after_gui_exit']=wait_connection()
            observations=subprocess.check_output(['/bin/ps','-axo','pid=,comm='],text=True)
            same=[int(line.strip().split(None,1)[0]) for line in observations.splitlines()
                  if len(line.strip().split(None,1))==2 and line.strip().split(None,1)[1]==str(executable)]
            receipt['checks']['gui_not_respawned']=same==[upgraded['pid']]
            # Verify the exact owned executable/PID again before inducing a crash.
            if state().get('pid')!=upgraded['pid'] or not receipt['checks']['gui_not_respawned']:
                raise RuntimeError('Fixture process ownership changed before crash test')
            os.kill(upgraded['pid'],signal.SIGKILL)
            recovered=wait_running(upgraded['pid'])
            installer.background_health(executable,profile,recovered['pid'],receipt['upgrade']['candidate']['executable_sha256'],installer.Deadline(30))
            receipt['checks']['launchd_recovers_only_background_after_crash']=wait_connection()
            if args.native_observer is not None:
                receipt['checks']['native_backend_mapped_image_after_launchd_recovery']=native_image(recovered)
            # Controlled unloaded gap tests attachment refusal, not a user reboot.
            launchctl('bootout')
            with installer.idle_profile(profile,installer.Deadline(10)):pass
            offline=run(timeout=10)
            receipt['checks']['restart_gap_gui_never_becomes_host']=offline.returncode!=0
            launchctl('bootstrap');gap_recovery=wait_running(recovered['pid'])
            receipt['checks']['service_reconnects_after_gap']=wait_connection()
            # Inject one known post-bootstrap health rejection to exercise real
            # signed app+plist rollback, never an uncertain launchctl replay.
            old_info=verify_app(target,load_policy());before_plist=plist.read_bytes()
            before_metadata=(plist.stat().st_uid,plist.stat().st_gid,plist.stat().st_mode&0o777)
            old_health=installer.background_health
            def fail_once(*a,**k):raise ReleaseRejected('Injected disposable fixture health rejection')
            installer.background_health=fail_once
            try:
                try:installer.install(args.failed_upgrade_archive.resolve(),True,separate_background=True)
                except ReleaseRejected as error:
                    receipt['checks']['failed_upgrade_reports_restored_previous']='previous app was restored' in str(error)
                else:raise RuntimeError('Fixture health failure was not injected')
            finally:installer.background_health=old_health
            receipt['checks']['failure_restores_signed_app']=installer.identity(verify_app(target,load_policy()))==installer.identity(old_info)
            receipt['checks']['failure_restores_exact_plist']=plist.read_bytes()==before_plist
            receipt['checks']['failure_preserves_launch_agent_permissions']=(plist.stat().st_uid,plist.stat().st_gid,plist.stat().st_mode&0o777)==before_metadata
            receipt['checks']['failure_preserves_identity']=identity_hashes()==identities
            receipt['checks']['rollback_background_connects']=wait_connection()
            if args.native_observer is not None:
                receipt['checks']['native_backend_mapped_image_after_rollback']=native_image(state())
            receipt['state']='complete' if all(receipt['checks'].values()) else 'failed_checks';save()
        except Exception as error:
            receipt.update(state='failed',error_type=type(error).__name__,error_summary=str(error)[:300]);save()
        finally:
            if gui is not None and gui.poll() is None:gui.send_signal(signal.SIGINT);gui.wait(timeout=10)
            observed=state()
            if observed.get('loaded'):launchctl('bootout')
            receipt['test_service_removed']=not state().get('loaded');save()
    print(json.dumps({'state':receipt['state'],'checks':receipt['checks'],'test_service_removed':receipt.get('test_service_removed'),
                      'error_summary':receipt.get('error_summary'),'receipt':str(evidence/'launchd-receipt.json')}),flush=True)
    return 0 if receipt['state']=='complete' and all(receipt['checks'].values()) else 1


if __name__=='__main__':raise SystemExit(main())
