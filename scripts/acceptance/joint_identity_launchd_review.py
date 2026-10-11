#!/usr/bin/env python3
"""Real signed joint-product identity transactions in a disposable launchd scope.

No validation hooks are replaced. SIGSTOP/SIGCONT only target an independently
verified candidate backend created by this test. Canonical daily paths and CUA
are never used. Additional signed build numbers are local acceptance fixtures,
sealed from the independently handed observed compiler output and receipt.
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
import threading
import time
import uuid
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_identity_gate as gate
import installer_build_identity as adapter
import installer_scope
import install_macos_unified as installer
from macos_release_guard import load_policy, package, verified_archive, verify_app, ReleaseRejected

ARTIFACTS = [('archive', 'archive_sha256'), ('compiler_binary', 'compiler_binary_sha256'),
             ('manifest', 'manifest_sha256'), ('native_observer', 'native_observer_sha256'),
             ('observed_build_receipt', 'observed_build_receipt_sha256'),
             ('production_packager_archive', 'production_packager_archive_sha256')]


def verify_handoff(path, expected_sha):
    h = gate.read_json(path, expected_sha)
    checks = {name: gate.hash_file(Path(h[name])) == h[key] for name, key in ARTIFACTS}
    gate.require(all(checks.values()), 'Independent artifact handoff mismatch')
    manifest = gate.manifest_document(Path(h['manifest']).read_bytes(), h['manifest_sha256'],
                                      h['source_commit'], h['build_identity_sha256'])
    build = gate.read_json(Path(h['observed_build_receipt']), h['observed_build_receipt_sha256'])
    source = gate.source_snapshot(Path(h['source_worktree']), h['source_commit'])
    checks.update(observed_receipt=build.get('kind') == 'observed-build' and build.get('fresh_target') is True,
                  observed_identity=build['build_identity'] == manifest['build_identity'],
                  compiler_binding=build['compiler_binary_sha256'] == h['compiler_binary_sha256'],
                  source_snapshot=source['snapshot_sha256'] == h['source_snapshot_sha256'] == manifest['build_identity']['source']['snapshot_sha256'],
                  signed_binary=manifest['binary_sha256'] == h['signed_binary_sha256'])
    checks['archive_inventory'] = gate.verify_package(Path(h['archive']), h['manifest_sha256'],
                    h['source_commit'], h['build_identity_sha256'])['package_integrity_verified']
    gate.require(all(checks.values()), 'Joint observed-build chain differs from handoff')
    return h, build, checks


def fixture_package(h, build, output, number):
    output.mkdir(mode=0o700)
    signed = package(Path(h['compiler_binary']), output, h['signing']['version'], number, load_policy())
    stage = output / 'sealed'; stage.mkdir(mode=0o700)
    with verified_archive(Path(signed['archive']), load_policy()) as (app, _):
        shutil.copytree(app, stage / 'RemotePlay.app')
    sealed = gate.seal_tree(stage, build, h['source_commit'], h['build_identity_sha256'],
                            compiler_binary=Path(h['compiler_binary']))
    archive = output / 'manifest-fixture.zip'
    with zipfile.ZipFile(archive, 'x') as z:
        for p in sorted(stage.rglob('*')):
            if p.is_file(): z.write(p, p.relative_to(stage).as_posix())
    checked = gate.verify_package(archive, sealed['manifest_sha256'], h['source_commit'], h['build_identity_sha256'])
    expectations = adapter.Expectations(h['source_commit'], h['build_identity_sha256'], sealed['manifest_sha256'],
                                        h['native_observer'], h['native_observer_sha256'])
    return {'archive': str(archive), 'archive_sha256': checked['archive_sha256'],
            'manifest_sha256': sealed['manifest_sha256'], 'build': number,
            'expectations': expectations, 'signing': signed}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--handoff', type=Path, required=True); p.add_argument('--handoff-sha256', required=True)
    p.add_argument('--probe', type=Path, required=True); p.add_argument('--probe-sha256', required=True)
    p.add_argument('--evidence-dir', type=Path, required=True); p.add_argument('--execute', action='store_true')
    args = p.parse_args()
    h, build, preflight = verify_handoff(args.handoff.resolve(), args.handoff_sha256)
    probe = args.probe.resolve(); gate.require(gate.hash_file(probe) == args.probe_sha256, 'Unverified external loopback probe')
    if not args.execute:
        print(json.dumps({'preflight': preflight, 'applied': False, 'production_modified': False})); return 0
    gate.require(sys.platform == 'darwin', 'Native launchd acceptance requires macOS')
    evidence = args.evidence_dir.resolve(); evidence.mkdir(mode=0o700, exist_ok=False)
    receipt = {'checks': {}, 'handoff_sha256': args.handoff_sha256, 'handoff_checks': preflight,
               'source_commit': h['source_commit'], 'build_identity_sha256': h['build_identity_sha256'],
               'production_modified': False, 'manual_CUA': False, 'login_reboot': False,
               'native_signal_fault': {}, 'state': 'preparing'}
    def save():
        destination = evidence / 'joint-launchd-receipt.json'
        fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'w') as out: json.dump(receipt, out, indent=2); out.write('\n')
    save()
    components = h['signing']['build'].split('.')
    gate.require(len(components) == 2 and all(part.isdecimal() for part in components)
                 and int(components[1]) > 0, 'Independent handed build must have a positive numeric fixture generation')
    prefix, number = components[0], int(components[1])
    old = fixture_package(h, build, evidence / 'old-build', f'{prefix}.{number-1}')
    failing = fixture_package(h, build, evidence / 'failure-build', f'{prefix}.{number+1}')
    receipt['fixture_variants'] = [{k:v for k,v in item.items() if k != 'expectations'} for item in (old, failing)]
    current = adapter.Expectations(h['source_commit'], h['build_identity_sha256'], h['manifest_sha256'],
                                   h['native_observer'], h['native_observer_sha256'])
    # Darwin's sockaddr_un has a 104-byte limit including the terminator.
    # The real installer profile suffix is fixed, so keep the temporary root
    # short enough for its authenticated background socket.
    root = Path(tempfile.mkdtemp(prefix='rpj.', dir='/tmp')).resolve()
    label = 'com.remoteplay.lifecycle-test.' + uuid.uuid4().hex
    scope = installer_scope.initialize(root, label)
    target = scope.home / 'Applications/RemotePlay.app'; executable = target / 'Contents/MacOS/remote_play'
    profile = installer.profile_directory(scope.home); profile.mkdir(mode=0o700, parents=True)
    gate.require(len(os.fsencode(profile / 'background.sock')) < 104,
                 'Fixture socket path exceeds the Darwin native limit')
    work = root / 'work'; work.mkdir(mode=0o700)
    plist = scope.home / ('Library/LaunchAgents/' + label + '.plist'); plist.parent.mkdir(mode=0o700, parents=True)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]; sock.close()
    env = dict(REMOTE_PLAY_DEVICE_GROUP_DIR=str(profile), REMOTE_PLAY_DISPLAY_NAME='Private joint identity fixture',
               REMOTE_PLAY_HOST_BIND_ADDR=f'127.0.0.1:{port}', REMOTE_PLAY_CLIENT_BIND_ADDR='127.0.0.1:0',
               REMOTE_PLAY_DISCOVERY_PORT='49158', REMOTE_PLAY_HEADLESS='1')
    env.update({key:'0' for key in ('REMOTE_PLAY_P2P','REMOTE_PLAY_RELAY','REMOTE_PLAY_DISCOVERY',
                  'REMOTE_PLAY_CLIENT_RECEIVER','REMOTE_PLAY_VIEWER_MEDIA','REMOTE_PLAY_CLIPBOARD_SYNC',
                  'REMOTE_PLAY_FILE_TRANSFER','REMOTE_PLAY_TALKBACK')})
    (evidence / 'background.log').touch(mode=0o600)
    document = dict(Label=label, ProgramArguments=[str(executable),'--background-service'], KeepAlive=True,
                    RunAtLoad=True, ThrottleInterval=3, WorkingDirectory=str(work), EnvironmentVariables=env,
                    StandardOutPath=str(evidence/'background.log'), StandardErrorPath=str(evidence/'background.log'))
    plist.write_bytes(plistlib.dumps(document)); plist.chmod(0o644)
    receipt.update(scope_root=str(root), test_label=label); save()
    gui = None; worker = None; fault_done = threading.Event(); resume_done = threading.Event()
    def state():
        # Thread contexts do not inherit the fixture scope automatically.
        with installer_scope.isolated(root, label): return installer.service_state(f'gui/{os.getuid()}')
    def launchctl(action):
        scope.validate()
        command = ['/bin/launchctl', action]
        command += [f'gui/{os.getuid()}',str(plist)] if action == 'bootstrap' else [f'gui/{os.getuid()}/{label}']
        subprocess.run(command, capture_output=True, check=True, timeout=20)
    def wait_running(previous=None):
        until = time.monotonic()+40
        while time.monotonic()<until:
            observed = state()
            if observed.get('running') and observed.get('pid') and observed['pid'] != previous:
                return observed
            time.sleep(.1)
        raise RuntimeError('Owned fixture backend did not become running')
    def clean_env():
        result = {k:v for k,v in os.environ.items() if not k.startswith('REMOTE_PLAY_')}; result.update(env); return result
    def connection():
        result = subprocess.run([str(probe),'--check-loopback-host',f'127.0.0.1:{port}'], env=clean_env(),
                                cwd=work, capture_output=True, timeout=15)
        return result.returncode == 0
    def wait_connection():
        for _ in range(20):
            if connection(): return True
            time.sleep(.2)
        return False
    def identity_files():
        return {name:gate.hash_file(profile/name) for name in ('mesh.conf','mesh.secret')}
    def backend(value):
        observed = wait_running()
        result = installer.verified_backend_identity(target, value, observed, plist, profile, installer.Deadline(60))
        receipt.setdefault('backend_observations',[]).append(dict(observed, **result)); save(); return observed
    def public_start(pid):
        gate.require(gate.hash_file(Path(h['native_observer']))==h['native_observer_sha256'], 'Native observer changed before owned-process observation')
        result = subprocess.run([h['native_observer'], str(pid), str(executable)], capture_output=True, check=True, timeout=10)
        return gate.json_bytes(result.stdout)['start_unix_ns']
    def observe_gone(pid):
        # kill(pid, 0) performs a kernel existence/permission check; it sends
        # no signal. An observer error alone is not proof of process exit.
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            receipt['native_signal_fault']['kernel_confirmed_process_exited'] = True
            save()
            return True
        return False
    def resume_owned(pid, start):
        if observe_gone(pid):
            return True
        try:
            observed_start = public_start(pid)
        except subprocess.CalledProcessError:
            if observe_gone(pid):
                return True
            raise
        gate.require(observed_start == start, 'Fault PID generation changed; never signal its replacement')
        os.kill(pid, signal.SIGCONT)
        receipt['native_signal_fault']['real_SIGCONT'] = True
        save()
        return True
    def signal_fault():
        stopped_pid = None; start = None; resumed = False
        try:
            until=time.monotonic()+120
            journal=target.parent/'.remoteplay-install-journal.json'
            while time.monotonic()<until and not fault_done.is_set():
                try: phase=json.loads(journal.read_text())['phase']
                except (OSError,ValueError,KeyError): phase=None
                if stopped_pid is None and phase in ('started','health_requested'):
                    observed=state()
                    if observed.get('pid') and observed.get('arguments')==[str(executable),'--background-service']:
                        gate.verify_running_macos(observed['pid'],executable,failing['signing']['executable_sha256'],
                                                  Path(h['native_observer']),h['native_observer_sha256'])
                        # A successful authenticated status reply proves the
                        # canonical profile lock has been claimed. This keeps
                        # rollback from moving the paused image before resume.
                        installer.background_health(executable,profile,observed['pid'],
                                                    failing['signing']['executable_sha256'],installer.Deadline(10))
                        if json.loads(journal.read_text())['phase'] not in ('started','health_requested'):
                            break
                        start=public_start(observed['pid']);stopped_pid=observed['pid']
                        os.kill(stopped_pid,signal.SIGSTOP)
                        receipt['native_signal_fault'].update(stopped_pid=stopped_pid,start_unix_ns=start,
                                                               candidate_build=failing['build'],real_SIGSTOP=True)
                        save()
                if stopped_pid is not None and phase in ('rollback_stop_requested','service_stopped','recovery_required'):
                    resumed=resume_owned(stopped_pid,start);resume_done.set()
                    break
                time.sleep(.01)
        except Exception as error:
            receipt['native_signal_fault']['error_type']=type(error).__name__;receipt['native_signal_fault']['error']=str(error)[:200];save()
        finally:
            if stopped_pid is not None and not resumed:
                try:
                    resume_owned(stopped_pid,start);resume_done.set()
                except (OSError,subprocess.SubprocessError,gate.IdentityRejected):pass
    with installer_scope.isolated(root,label):
        try:
            with adapter.verified_identity_archive(Path(old['archive']),load_policy(),old['expectations'],runner=installer.run,check=lambda:None) as (app,signed,value):
                shutil.copytree(app,target)
            adapter.write_new(installer.release_state_path(target),adapter.state_bytes(value))
            installer.ensure_role_marker(profile)
            launchctl('bootstrap'); initial=backend(value)
            receipt['checks']['old_identity_bound_backend_connects']=wait_connection()
            membership=identity_files(); metadata=(plist.stat().st_uid,plist.stat().st_gid,plist.stat().st_mode&0o777)
            before_state=adapter.private_bytes(installer.release_state_path(target));before_plist=plist.read_bytes()
            receipt['plan']=installer.install(Path(h['archive']),False,separate_background=True,expected_release=current)
            receipt['checks']['joint_readonly_plan_verified']=receipt['plan']['build_identity_preflight_verified'] and not receipt['plan']['applied']
            receipt['checks']['readonly_plan_preserves_prior_tuple']=adapter.private_bytes(installer.release_state_path(target))==before_state and plist.read_bytes()==before_plist and state()['pid']==initial['pid']
            receipt['upgrade']=installer.install(Path(h['archive']),True,separate_background=True,expected_release=current)
            upgraded_value=adapter.read_state(installer.release_state_path(target)); upgraded=backend(upgraded_value)
            receipt['checks']['native_identity_bound_upgrade']=receipt['upgrade']['backend_running_image_verified'] and upgraded['pid']!=initial['pid']
            receipt['checks']['new_release_manifest_persisted']=upgraded_value['raw_manifest']==Path(h['manifest']).read_bytes()
            receipt['checks']['upgrade_preserves_membership']=identity_files()==membership
            receipt['checks']['upgrade_preserves_plist_permissions']=(plist.stat().st_uid,plist.stat().st_gid,plist.stat().st_mode&0o777)==metadata
            receipt['checks']['new_backend_connects']=wait_connection()
            good_state=adapter.private_bytes(installer.release_state_path(target));good_plist=plist.read_bytes();good_app=verify_app(target,load_policy())
            # Actual old-build and wrong-anchor calls use real archive/signature
            # metadata; neither is a mocked return or an expected success default.
            try:installer.install(Path(old['archive']),True,separate_background=True,expected_release=old['expectations'])
            except ReleaseRejected as error:receipt['checks']['old_build_rejected']='rollback rejected' in str(error)
            else:receipt['checks']['old_build_rejected']=False
            wrong=adapter.Expectations(current.commit,current.identity_sha256,'0'*64,current.observer,current.observer_sha256)
            try:installer.install(Path(h['archive']),True,separate_background=True,expected_release=wrong)
            except gate.IdentityRejected as error:receipt['checks']['wrong_manifest_rejected']='independently pinned' in str(error)
            else:receipt['checks']['wrong_manifest_rejected']=False
            receipt['checks']['rejections_preserve_running_release_tuple']=state()['pid']==upgraded['pid'] and adapter.private_bytes(installer.release_state_path(target))==good_state and plist.read_bytes()==good_plist and verify_app(target,load_policy())==good_app
            gui_env=clean_env();gui_env.pop('REMOTE_PLAY_HEADLESS',None)
            gui_env.update(REMOTE_PLAY_RESTORED_TEST_OUTPUT=str(evidence/'gui-render.json'),REMOTE_PLAY_RESTORED_TEST_SECONDS='5')
            with (evidence/'gui.log').open('wb') as log:
                os.chmod(evidence/'gui.log',0o600)
                gui=subprocess.Popen([str(executable),'--gui'],cwd=work,env=gui_env,stdout=log,stderr=subprocess.STDOUT)
                gui.wait(timeout=35)
            rendering=json.loads((evidence/'gui-render.json').read_text())
            receipt['checks']['actual_joint_GUI_normal_exit']=gui.returncode==0 and rendering['render_calls']>0
            receipt['checks']['GUI_exit_keeps_same_backend']=state()['pid']==upgraded['pid']
            receipt['checks']['backend_connects_after_GUI_exit']=wait_connection()
            def only_backend(observed):
                lines=subprocess.check_output(['/bin/ps','-axo','pid=,comm='],text=True,errors='replace').splitlines()
                owners=[int(line.strip().split(None,1)[0]) for line in lines if len(line.strip().split(None,1))==2 and line.strip().split(None,1)[1]==str(executable)]
                return owners==[observed['pid']]
            receipt['checks']['GUI_not_respawned']=only_backend(upgraded)
            gate.require(receipt['checks']['GUI_not_respawned'],'Fixture ownership changed before crash')
            gate.verify_running_macos(upgraded['pid'],executable,h['signed_binary_sha256'],Path(h['native_observer']),h['native_observer_sha256'])
            os.kill(upgraded['pid'],signal.SIGKILL)
            recovered=wait_running(upgraded['pid']);backend(upgraded_value)
            receipt['checks']['launchd_recovers_verified_backend_only']=only_backend(recovered) and wait_connection()
            # Stop only the independently observed later-build process once. The real
            # background IPC health check fails, then the real installer rolls
            # back App/plist/manifest/expectations and verifies restored backend.
            worker=threading.Thread(target=signal_fault,daemon=True);worker.start()
            try:installer.install(Path(failing['archive']),True,separate_background=True,expected_release=failing['expectations'])
            except ReleaseRejected as error:
                receipt['checks']['real_native_failure_reports_rollback']='previous app was restored' in str(error)
                receipt['native_failure_reason']=str(error)[-1000:]
            else:receipt['checks']['real_native_failure_reports_rollback']=False
            finally:fault_done.set();worker.join(timeout=15)
            receipt['checks']['real_process_fault_exercised']=receipt['native_signal_fault'].get('real_SIGSTOP') is True and (
                receipt['native_signal_fault'].get('real_SIGCONT') is True or
                receipt['native_signal_fault'].get('kernel_confirmed_process_exited') is True)
            receipt['checks']['rollback_restores_exact_release_state']=adapter.private_bytes(installer.release_state_path(target))==good_state
            receipt['checks']['rollback_restores_exact_plist']=plist.read_bytes()==good_plist
            receipt['checks']['rollback_restores_signed_joint_App']=verify_app(target,load_policy())==good_app
            restored_value=adapter.read_state(installer.release_state_path(target));restored=backend(restored_value)
            receipt['checks']['rollback_native_backend_and_connectivity']=only_backend(restored) and wait_connection()
            receipt['checks']['rollback_preserves_membership']=identity_files()==membership
            receipt['checks']['rollback_preserves_plist_permissions']=(plist.stat().st_uid,plist.stat().st_gid,plist.stat().st_mode&0o777)==metadata
            receipt['state']='complete' if all(receipt['checks'].values()) else 'failed_checks';save()
        except Exception as error:
            receipt.update(state='failed',error_type=type(error).__name__,error_summary=str(error)[-1000:]);save()
        finally:
            fault_done.set()
            if worker is not None:worker.join(timeout=15)
            if gui is not None and gui.poll() is None:gui.send_signal(signal.SIGINT);gui.wait(timeout=10)
            if state().get('loaded'):launchctl('bootout')
            receipt['test_service_removed']=not state().get('loaded');save()
    print(json.dumps({'state':receipt['state'],'checks':receipt['checks'],'test_service_removed':receipt.get('test_service_removed'),
                      'error_summary':receipt.get('error_summary'),'receipt':str(evidence/'joint-launchd-receipt.json')}),flush=True)
    return 0 if receipt['state']=='complete' and receipt.get('test_service_removed') else 1


if __name__=='__main__':raise SystemExit(main())
