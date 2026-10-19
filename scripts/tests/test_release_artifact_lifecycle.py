"""Small current-output fixtures. No real project inventory, signer or builds."""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import signal
import time
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

SCRIPT = Path(__file__).resolve().parents[1] / 'release_artifact_lifecycle.py'
spec = importlib.util.spec_from_file_location('release_artifact_lifecycle_fixture', SCRIPT)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)


class ReleaseHookTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='rp-current-hook-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        (self.root / 'scripts').mkdir()
        self.addCleanup(patch.stopall)
        patch.object(hook, 'ROOT', self.root).start()
        patch.dict(os.environ, {}, clear=False).start()
        for key in (hook.LOCK_ENV, hook.PID_ENV, hook.ANDROID_UPGRADE_ENV): os.environ.pop(key, None)
        self.calls = []
        self.public = patch.object(hook, 'public', side_effect=self.public_call).start()
        self.target = patch.object(hook, 'macos_output_root', return_value=self.root / 'shared/package/macos').start()
        self.state = {'producer_pid': os.getpid()}

    def public_call(self, action, *arguments):
        args = list(map(str, arguments)); self.calls.append((action, args))
        def values(flag): return [args[i+1] for i, value in enumerate(args) if value == flag]
        if action == 'release-admit':
            files = values('--file'); aux = values('--aux-stage')
            for path in files: Path(path).parent.mkdir(parents=True, exist_ok=True)
            for path in aux: Path(path).mkdir()
            return {'admission': 'reserved', 'component': values('--component')[0], 'files': files,
                    'staged_files': list(map(str, hook.stage_paths(files))), 'owned_aux_stages': aux}
        if action == 'release-publish':
            return {'current_committed': True, 'paths': values('--file'), 'cleanup_status': 'completed'}
        return {'mode': 'synthetic'}

    def apk(self, path=None, **changes):
        apk = path or hook.android_files()[0]; receipt = apk.with_suffix('.verification.json')
        apk.parent.mkdir(parents=True, exist_ok=True); apk.write_bytes(b'synthetic signed APK')
        report = {'sha256': hashlib.sha256(apk.read_bytes()).hexdigest(), 'size_bytes': apk.stat().st_size,
                  'signer_sha256': hook.ANDROID_SIGNER, 'debuggable': False, 'abi': ['arm64-v8a'],
                  'native_16kb_aligned': True, 'version': '2.0.0-alpha.8', 'version_code': 9}
        report.update(changes); receipt.write_text(json.dumps(report))
        return apk, receipt, report

    def android_candidate(self):
        admission = hook.admit_android(self.state)
        apk, _, report = self.apk(hook.android_workspace() / 'release/app-release.apk')
        self.state['admission'] = admission
        return admission, apk, report

    def test_native_preflight_without_complete_release_reservation(self):
        with hook.producer('android-native') as (_, state):
            self.assertEqual(state['producer_pid'], os.getpid())
        self.assertEqual([c[0] for c in self.calls], ['release-setup', 'release-preflight'])
        self.target.assert_not_called()

    def test_release_finally_calls_abort_after_child_wait(self):
        events = []
        def call(action, *args):
            if '--abort-current' in args: events.append('abort')
            return self.public_call(action, *args)
        with patch.object(hook, 'public', side_effect=call), patch.object(hook.subprocess, 'Popen') as child:
            child.return_value.wait.side_effect = lambda: events.append('wait') or 7
            self.assertEqual(hook.run('android-release', []), 7)
        self.assertEqual(events, ['wait', 'abort'])

    def test_second_producer_refused_without_work(self):
        with hook.producer('android-native'):
            with self.assertRaisesRegex(hook.Refusal, 'Another Weavelo'):
                with hook.producer('android-native'): self.fail('overlap')
        self.assertEqual(len(self.calls), 2)

    def test_native_inherits_release_lock_through_bash(self):
        shutil.copy2(SCRIPT, self.root / 'scripts/release_artifact_lifecycle.py')
        with hook.producer('android-release') as (fd, _):
            result = subprocess.run(['bash', '-c', 'exec "$1" -B "$2" check-lock android-native',
                                     'fixture', sys.executable, str(self.root / 'scripts/release_artifact_lifecycle.py')],
                                    env=dict(os.environ, **{hook.LOCK_ENV: str(fd)}), pass_fds=(fd,), capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_wrong_inherited_scope_refused(self):
        with hook.producer('macos') as (fd, _), patch.dict(os.environ, {hook.LOCK_ENV: str(fd)}):
            with self.assertRaises(hook.Refusal): hook.check_lock('android-native')

    def test_failed_preflight_never_starts_child(self):
        with patch.object(hook, 'public', side_effect=hook.Refusal('identity')), patch.object(hook.subprocess, 'Popen') as child:
            with self.assertRaises(hook.Refusal): hook.run('android-release', [])
            child.assert_not_called()

    def test_unchanged_cargo_environment_and_child_success(self):
        env = {'CARGO_TARGET_DIR': '/synthetic/cache', 'CARGO_BUILD_BUILD_DIR': '/synthetic/build', 'RUSTFLAGS': '-Cdebuginfo=2'}
        with patch.dict(os.environ, env), patch.object(hook.subprocess, 'Popen') as child:
            child.return_value.wait.return_value = 0
            self.assertEqual(hook.run('android-native', []), 0)
            passed = child.call_args.kwargs['env']
            self.assertEqual({key: passed[key] for key in env}, env)
            self.assertEqual(passed[hook.PID_ENV], str(os.getpid()))

    def test_interrupt_waits_own_process_before_abort(self):
        events = []
        def call(action, *args):
            if '--abort-current' in args: events.append('abort')
            return self.public_call(action, *args)
        with patch.object(hook, 'public', side_effect=call), patch.object(hook.subprocess, 'Popen') as child, \
             patch.object(hook.os, 'killpg', side_effect=lambda pid, sig: events.append('terminate' if sig == signal.SIGTERM else 'kill')):
            child.return_value.pid = 12345; child.return_value.poll.return_value = None
            child.return_value.wait.side_effect = [KeyboardInterrupt(), 0, 0]
            with self.assertRaises(KeyboardInterrupt): hook.run('android-release', [])
            self.assertEqual(child.return_value.wait.call_count, 3)
        self.assertEqual(events, ['terminate', 'kill', 'abort'])

    def test_cancel_kills_owned_descendant_after_shell_already_exited(self):
        pidfile = self.root / 'descendant.pid'
        child = subprocess.Popen(['bash', '-c', 'sleep 60 & echo $! > "$1"', 'fixture', str(pidfile)],
                                 start_new_session=True)
        child.wait(timeout=3)
        descendant = int(pidfile.read_text())
        hook.stop_child_group(child)
        # The orphan may briefly be a zombie; a live process must not remain.
        status = subprocess.run(['/bin/ps', '-p', str(descendant), '-o', 'stat='], capture_output=True, text=True)
        self.assertTrue(status.returncode == 1 or status.stdout.strip().startswith('Z'), status.stdout)

    def test_zombie_only_group_permission_error_does_not_fail_completed_cleanup(self):
        child=Mock(pid=12345)
        child.wait.return_value=0
        with patch.object(hook.os,'killpg',side_effect=[None,PermissionError()]), \
             patch.object(hook.subprocess,'run',return_value=SimpleNamespace(returncode=0,stdout='12345 Z\n')):
            hook.stop_child_group(child)
        self.assertEqual(child.wait.call_count,2)

    def test_live_group_permission_error_still_blocks_cleanup(self):
        child=Mock(pid=12345)
        child.wait.return_value=0
        with patch.object(hook.os,'killpg',side_effect=[None,PermissionError()]), \
             patch.object(hook.subprocess,'run',return_value=SimpleNamespace(returncode=0,stdout='12345 S\n')):
            with self.assertRaises(PermissionError):hook.stop_child_group(child)

    def test_cleanup_deferred_does_not_change_child_success(self):
        def call(action, *args):
            if '--abort-current' in args: raise hook.Refusal('in-use temporary')
            return self.public_call(action, *args)
        with patch.object(hook, 'public', side_effect=call), patch.object(hook.subprocess, 'Popen') as child, contextlib.redirect_stderr(io.StringIO()) as output:
            child.return_value.wait.return_value = 0
            self.assertEqual(hook.run('android-release', []), 0)
            self.assertIn('CLEANUP_DEFERRED', output.getvalue())

    def test_signing_refusal_precedes_lifecycle(self):
        with patch.object(hook.subprocess, 'run', return_value=SimpleNamespace(returncode=1)):
            with self.assertRaises(hook.Refusal): hook.run('macos', [])
        self.assertEqual(self.calls, [])

    def test_mac_target_override_refused_before_child(self):
        with patch.object(hook.subprocess, 'run', return_value=SimpleNamespace(returncode=0)), \
             patch.object(hook, 'macos_output_root', side_effect=hook.Refusal('target override')), \
             patch.object(hook.subprocess, 'Popen') as child:
            with self.assertRaises(hook.Refusal): hook.run('macos', [])
            child.assert_not_called()
        self.assertEqual(self.calls, [])

    def test_existing_current_adoption_verifies_original_pair_without_copy(self):
        apk, receipt, _ = self.apk(); before = [p.read_bytes() for p in (apk, receipt)]
        with patch.object(hook, 'verify_existing_android_signer') as signer:
            hook.admit_android(self.state); signer.assert_called_once_with(apk)
        action, args = self.calls[-1]
        self.assertEqual(action, 'release-admit'); self.assertIn('--adopt-current', args)
        self.assertEqual(args[args.index('--current-validation-file')+1], str(receipt))
        self.assertEqual(args.count('--current-sha256'), 2)
        self.assertEqual([p.read_bytes() for p in (apk, receipt)], before)
        self.assertNotIn('--snapshot', args); self.assertNotIn('--generation', args)

    def test_upgrade_reserves_distinct_public_pair_without_touching_waiting_default(self):
        apk, receipt, _ = self.apk(); before = [(p.read_bytes(), p.stat().st_ino) for p in (apk, receipt)]
        with patch.dict(os.environ, {hook.ANDROID_UPGRADE_ENV: '1'}), patch.object(hook, 'verify_existing_android_signer') as signer:
            admission = hook.admit_android(self.state)
            signer.assert_not_called()
            self.assertEqual(admission['component'], 'android-arm64-upgrade')
            self.assertEqual(Path(admission['files'][0]).name, 'app-upgrade.apk')
            self.assertEqual(Path(admission['files'][1]).name, 'app-upgrade.verification.json')
            self.assertEqual(Path(admission['owned_aux_stages'][0]).name, '.weavelo-android-upgrade-package.pending')
        self.assertEqual([(p.read_bytes(), p.stat().st_ino) for p in (apk, receipt)], before)

    def test_upgrade_verified_publish_keeps_the_waiting_default_bytes_and_inode(self):
        apk, receipt, _ = self.apk(); before = [(p.read_bytes(), p.stat().st_ino) for p in (apk, receipt)]
        with patch.dict(os.environ, {hook.ANDROID_UPGRADE_ENV: '1'}):
            admission, candidate, report = self.android_candidate()
            with patch.dict(sys.modules, {'verify_release_apk': SimpleNamespace(verify=lambda *_: report)}), contextlib.redirect_stdout(io.StringIO()):
                hook.publish_android(self.state)
            action, args = self.calls[-1]
            self.assertEqual(action, 'release-publish')
            self.assertEqual(args[args.index('--component')+1], 'android-arm64-upgrade')
            self.assertTrue(all('app-upgrade' in p for p in admission['files']))
            self.assertFalse(candidate.exists())
        self.assertEqual([(p.read_bytes(), p.stat().st_ino) for p in (apk, receipt)], before)

    def test_upgrade_mode_cannot_change_inside_default_producer_lock(self):
        with hook.producer('android-release') as (fd, _):
            with patch.dict(os.environ, {hook.LOCK_ENV: str(fd), hook.ANDROID_UPGRADE_ENV: '1'}):
                with self.assertRaisesRegex(hook.Refusal, 'output mode differs'):
                    hook.check_lock('android-release')

    def test_upgrade_native_child_inherits_original_producer_and_exact_mode(self):
        shutil.copy2(SCRIPT, self.root/'scripts/release_artifact_lifecycle.py')
        with patch.dict(os.environ, {hook.ANDROID_UPGRADE_ENV: '1'}):
            with hook.producer('android-release') as (fd, _):
                process = subprocess.run(['bash', '-c', 'exec "$1" -B "$2" check-lock android-native',
                    'fixture', sys.executable, str(self.root/'scripts/release_artifact_lifecycle.py')],
                    env=dict(os.environ, **{hook.LOCK_ENV: str(fd)}), pass_fds=(fd,), capture_output=True, text=True)
                self.assertEqual(process.returncode, 0, process.stderr)

    def test_upgrade_gradle_guard_binds_real_public_reservation_not_mode_flag(self):
        with patch.dict(os.environ, {hook.ANDROID_UPGRADE_ENV: '1', hook.PID_ENV: str(os.getpid())}):
            result = {'staged_files': list(map(str, hook.stage_paths(hook.android_files()))),
                      'owned_aux_stages': [str(hook.android_workspace())]}
            validator = self.current_guard(result)
            hook.check_current(hook.android_workspace()/'release')
            validator.assert_called_with('api', str(self.root), 'android-arm64-upgrade',
                                         list(map(str, hook.android_files())), os.getpid())
            with self.assertRaises(hook.Refusal): hook.check_current(hook.android_files()[0].parent)

    def test_upgrade_mode_does_not_accept_arbitrary_output_paths(self):
        with patch.dict(os.environ, {hook.ANDROID_UPGRADE_ENV: '/tmp/other-output'}):
            with self.assertRaisesRegex(hook.Refusal, 'Invalid fixed Android'):
                hook.android_files()
        self.assertEqual(self.calls, [])

    def test_partial_old_current_preserved(self):
        apk, receipt, _ = self.apk(); receipt.unlink()
        with self.assertRaises(FileNotFoundError): hook.admit_android(self.state)
        self.assertTrue(apk.exists()); self.assertEqual(self.calls, [])

    def test_tampered_old_current_preserved(self):
        apk, _, _ = self.apk(); apk.write_bytes(b'changed')
        with self.assertRaises(hook.Refusal): hook.admit_android(self.state)
        self.assertEqual(apk.read_bytes(), b'changed'); self.assertEqual(self.calls, [])

    def test_wrong_old_signer_blocks_reservation(self):
        self.apk()
        with patch.object(hook, 'verify_existing_android_signer', side_effect=hook.Refusal('signer')):
            with self.assertRaises(hook.Refusal): hook.admit_android(self.state)
        self.assertEqual(self.calls, [])

    def test_rejected_apk_never_moves_or_publishes(self):
        admission, apk, _ = self.android_candidate()
        verifier = SimpleNamespace(verify=Mock(side_effect=ValueError('stale JNI')))
        with patch.dict(sys.modules, {'verify_release_apk': verifier}):
            with self.assertRaises(ValueError): hook.publish_android(self.state)
        self.assertTrue(apk.exists()); self.assertFalse(Path(admission['staged_files'][0]).exists())
        self.assertNotIn('release-publish', [c[0] for c in self.calls])

    def test_android_verify_then_rename_then_publish_exact_default_pair(self):
        admission, apk, report = self.android_candidate(); inode = apk.stat().st_ino
        def verify(candidate, root):
            self.assertEqual(candidate, apk); self.assertEqual(root, self.root)
            self.assertFalse(Path(admission['staged_files'][0]).exists()); return report
        with patch.dict(sys.modules, {'verify_release_apk': SimpleNamespace(verify=verify)}), contextlib.redirect_stdout(io.StringIO()):
            result = hook.publish_android(self.state)
        self.assertTrue(result['current_committed']); self.assertFalse(apk.exists())
        self.assertEqual(Path(admission['staged_files'][0]).stat().st_ino, inode)
        action, args = self.calls[-1]; self.assertEqual(action, 'release-publish')
        self.assertEqual(args.count('--expected-sha256'), 2)
        self.assertEqual(args[args.index('--validation-file')+1], admission['staged_files'][1])
        self.assertIn(admission['files'][0], args); self.assertNotIn('--snapshot', args)

    def test_apk_changed_after_verification_never_moved(self):
        _, apk, report = self.android_candidate()
        def verify(*_): apk.write_bytes(b'changed'); return report
        with patch.dict(sys.modules, {'verify_release_apk': SimpleNamespace(verify=verify)}):
            with self.assertRaises(hook.Refusal): hook.publish_android(self.state)
        self.assertTrue(apk.exists())

    def test_existing_stage_never_overwritten(self):
        admission, _, report = self.android_candidate()
        stage = Path(admission['staged_files'][0]); stage.write_bytes(b'unknown')
        with patch.dict(sys.modules, {'verify_release_apk': SimpleNamespace(verify=lambda *_: report)}):
            with self.assertRaises(hook.Refusal): hook.publish_android(self.state)
        self.assertEqual(stage.read_bytes(), b'unknown')

    def test_commit_confirmation_required(self):
        with patch.object(hook, 'public', return_value={'current_committed': False}):
            with self.assertRaises(hook.Refusal):
                hook.publish(self.state, {'component': 'mac', 'files': ['a','b'], 'staged_files': ['x','y']}, ['a'*64,'b'*64])

    def test_committed_current_and_deferred_cleanup_is_success(self):
        result = {'current_committed': True, 'paths': ['a','b'], 'cleanup_status': 'deferred', 'retention': {'reason': 'open'}}
        with patch.object(hook, 'public', return_value=result), contextlib.redirect_stderr(io.StringIO()) as output:
            self.assertIs(hook.publish(self.state, {'component': 'mac','files':['a','b'],'staged_files':['x','y']}, ['a'*64,'b'*64]), result)
        self.assertIn('CLEANUP_DEFERRED', output.getvalue())

    def test_mac_receipt_binds_eventual_current_path_and_exact_stage(self):
        files = [self.root/'Weavelo-macos-arm64.zip', self.root/'Weavelo-macos-arm64.verification.json']
        admission = hook.admit(self.state, 'macos-arm64-release', files, aux_stage=self.root/'.mac-work')
        archive, receipt = map(Path, admission['staged_files']); archive.write_bytes(b'zip')
        report = {'signature_verified': True, 'archive': str(files[0]), 'archive_sha256': hook.hashed(archive)[0], 'size_bytes': 3}
        receipt.write_text(json.dumps(report))
        self.assertTrue(hook.publish_macos(self.state, admission, report)['current_committed'])
        report['archive'] = str(archive)
        with self.assertRaises(hook.Refusal): hook.publish_macos(self.state, admission, report)

    def current_guard(self, result):
        shared = SimpleNamespace(companion=lambda _: SimpleNamespace(validate_producer=Mock(return_value=result)), release_api=lambda: 'api')
        validator = Mock(return_value=result); shared.companion = lambda _: SimpleNamespace(validate_producer=validator)
        spec = SimpleNamespace(loader=SimpleNamespace(exec_module=lambda _: None))
        patches = [patch.object(hook, 'regular'), patch.object(hook.importlib.util,'spec_from_file_location',return_value=spec),
                   patch.object(hook.importlib.util,'module_from_spec',return_value=shared)]
        for item in patches: item.start(); self.addCleanup(item.stop)
        return validator

    def test_current_guard_requires_real_shared_identity_not_env_boolean(self):
        validator = self.current_guard({})
        with self.assertRaises(hook.Refusal): hook.check_current(self.root)
        validator.assert_not_called()

    def test_current_guard_checks_aux_actual_output_and_producer(self):
        result = {'staged_files':list(map(str,hook.stage_paths(hook.android_files()))), 'owned_aux_stages':[str(hook.android_workspace())]}
        validator = self.current_guard(result)
        with patch.dict(os.environ,{hook.PID_ENV:str(os.getpid())}):
            hook.check_current(hook.android_workspace()/'release')
            with self.assertRaises(hook.Refusal): hook.check_current(hook.android_files()[0].parent)
        validator.assert_called_with('api',str(self.root),'android-arm64-release',list(map(str,hook.android_files())),os.getpid())

    def test_wrong_shared_aux_refused(self):
        result = {'staged_files':list(map(str,hook.stage_paths(hook.android_files()))), 'owned_aux_stages':[]}
        self.current_guard(result)
        with patch.dict(os.environ,{hook.PID_ENV:str(os.getpid())}):
            with self.assertRaises(hook.Refusal): hook.check_current(hook.android_workspace()/'release')


    def test_unsigned_preparation_cannot_be_published_as_release_evidence(self):
        apk, receipt, report = self.apk(signer_sha256=None, signing_status='unsigned')
        with self.assertRaisesRegex(hook.Refusal, 'signing receipt'):
            hook.android_evidence(apk, receipt)

    def test_unsigned_review_holds_original_stage_and_never_publishes(self):
        admission = hook.admit_android(self.state)
        apk, _, report = self.apk(hook.android_workspace() / 'release/app-release-unsigned.apk',
                                  signer_sha256=None, signing_status='unsigned')
        self.state['admission'] = admission
        verifier = SimpleNamespace(verify_unsigned=Mock(return_value=report))
        with patch.dict(sys.modules, {'verify_release_apk': verifier}), \
             patch.object(hook, 'check_lock', return_value=(1, self.state)) as lock, \
             patch.object(hook, 'check_current') as guard, \
             patch.object(hook.subprocess, 'check_output', side_effect=['a'*40+'\n', '']), \
             patch.object(hook.time, 'monotonic', side_effect=[0, 0, 1801]), \
             patch.object(hook.time, 'sleep') as sleep, \
             contextlib.redirect_stdout(io.StringIO()) as output:
            with self.assertRaisesRegex(hook.Refusal, 'review window'):
                hook.hold_unsigned()
        lock.assert_called_once_with('android-release')
        guard.assert_called_once_with(hook.android_workspace() / 'release')
        verifier.verify_unsigned.assert_called_once_with(apk, self.root)
        metadata = json.loads((apk.parent / 'unsigned-preparation.json').read_text())
        self.assertEqual(metadata['state'], 'unsigned-awaiting-signing')
        self.assertIsNone(metadata['signer_sha256'])
        self.assertEqual(metadata['retention'], 'live-public-owned-stage-only')
        self.assertIn('UNSIGNED_APK_READY', output.getvalue())
        self.assertFalse(any(action == 'release-publish' for action, _ in self.calls))
        self.assertFalse(any(Path(p).exists() for p in admission['staged_files']))
        self.assertTrue(apk.is_file())
        sleep.assert_called_once_with(1)



    def prepared_current(self):
        apk, receipt, report = self.apk(signer_sha256=None, signing_status='unsigned',
                                       source_commit_observed='a'*40)
        verifier = SimpleNamespace(verify_unsigned=Mock(return_value={
            key: value for key, value in report.items() if key != 'source_commit_observed'}))
        patch.dict(sys.modules, {'verify_release_apk': verifier}).start()
        (self.root / '.rust-storage').mkdir(exist_ok=True)
        registry = {'current_components': {hook.android_component(): {
            'validation_kind': 'unsigned-dev', 'files': list(map(str, (apk, receipt))),
            'validation_file': str(receipt),
            'validation_digest': hashlib.sha256(receipt.read_bytes()).hexdigest()}}}
        (self.root / '.rust-storage/release-deliveries.json').write_text(json.dumps(registry))
        return apk, receipt, report

    def test_local_prepared_stage_keeps_unsigned_current_and_original_reservation(self):
        apk, receipt, report = self.prepared_current()
        before = [(p.read_bytes(), p.stat().st_ino) for p in (apk, receipt)]
        self.state['admission'] = hook.admit_android(self.state)
        with patch.object(hook, 'check_current') as guard:
            target = hook.stage_unsigned_current(self.state)
        guard.assert_called_once_with(hook.android_workspace() / 'release')
        self.assertEqual(target.read_bytes(), apk.read_bytes())
        self.assertEqual([(p.read_bytes(), p.stat().st_ino) for p in (apk, receipt)], before)
        provenance = json.loads((target.parent / 'prepared-unsigned.json').read_text())
        self.assertEqual(provenance, {'source_commit_observed': 'a'*40, 'unsigned_sha256': report['sha256']})
        self.assertFalse(any(action == 'release-publish' for action, _ in self.calls))

    def test_local_prepared_signing_retains_build_source_and_rejects_mutated_unsigned(self):
        apk, receipt, original = self.prepared_current()
        self.state['admission'] = hook.admit_android(self.state)
        with patch.object(hook, 'check_current'):
            unsigned = hook.stage_unsigned_current(self.state)
        candidate = unsigned.parent / 'app-release.apk'
        candidate.write_bytes(b'synthetic locally signed APK')
        signed = dict(original, sha256=hashlib.sha256(candidate.read_bytes()).hexdigest(),
                      size_bytes=candidate.stat().st_size, signer_sha256=hook.ANDROID_SIGNER)
        signed.pop('signing_status')
        sys.modules['verify_release_apk'].verify = Mock(return_value=signed)
        unsigned.write_bytes(b'mutated')
        with self.assertRaisesRegex(hook.Refusal, 'provenance changed'):
            hook.publish_android(self.state)
        self.assertTrue(candidate.exists())
        self.assertFalse(any(action == 'release-publish' for action, _ in self.calls))
        unsigned.write_bytes(apk.read_bytes())
        with contextlib.redirect_stdout(io.StringIO()):
            hook.publish_android(self.state)
        staged_receipt = Path(self.state['admission']['staged_files'][1])
        recorded = json.loads(staged_receipt.read_text())
        self.assertEqual(recorded['source_commit_observed'], 'a'*40)
        self.assertEqual(recorded['signed_from_unsigned_sha256'], original['sha256'])
        self.assertEqual(recorded['signer_sha256'], hook.ANDROID_SIGNER)

    def test_save_unsigned_uses_normal_unsigned_dev_publication(self):
        data = b'synthetic signed APK'
        expected = hashlib.sha256(data).hexdigest()
        report = dict(sha256=expected, size_bytes=len(data), signer_sha256=None,
                      signing_status='unsigned', debuggable=False, abi=['arm64-v8a'],
                      native_16kb_aligned=True, version='2.0.0-alpha.8', version_code=9)
        verifier = SimpleNamespace(verify_unsigned=Mock(return_value=report))
        (self.root / '.rust-storage').mkdir()
        producer_lock = self.root / '.rust-storage/release-producer.lock'
        producer_lock.write_text('{}')
        producer_lock.chmod(0o600)
        with patch.object(hook, 'buffer_unsigned', return_value=(data, {})), \
             patch.object(hook.os, 'kill', side_effect=ProcessLookupError), \
             patch.object(hook.fcntl, 'flock', side_effect=[BlockingIOError(), None, None]), \
             patch.object(hook.time, 'sleep') as wait, \
             patch.object(hook.subprocess, 'check_output', return_value='b'*40), \
             patch.dict(sys.modules, {'verify_release_apk': verifier}), \
             contextlib.redirect_stdout(io.StringIO()) as output:
            result = hook.save_unsigned(expected, 12345, 'a'*40)
        self.assertTrue(result['current_committed'])
        wait.assert_called_once_with(1)
        calls = [args for action, args in self.calls if action == 'release-publish']
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][calls[0].index('--validation-kind')+1], 'unsigned-dev')
        apk, receipt = hook.stage_paths(hook.android_files())
        self.assertEqual(apk.read_bytes(), data)
        metadata = json.loads(receipt.read_text())
        self.assertIsNone(metadata['signer_sha256'])
        self.assertEqual(metadata['source_commit_observed'], 'a'*40)
        self.assertEqual(metadata['state'], 'unsigned-awaiting-local-key-import')
        self.assertIn('UNSIGNED_APK_SAVED', output.getvalue())

    def test_registered_unsigned_current_is_reused_without_fake_signing_adoption(self):
        apk, receipt, report = self.apk(signer_sha256=None, signing_status='unsigned')
        state = self.root / '.rust-storage'
        state.mkdir(exist_ok=True)
        (state / 'release-deliveries.json').write_text(json.dumps({'current_components': {
            hook.android_component(): {'validation_kind': 'unsigned-dev',
                'files': list(map(str, hook.android_files())), 'validation_file': str(receipt),
                'validation_digest': hashlib.sha256(receipt.read_bytes()).hexdigest()}}}))
        with patch.dict(sys.modules, {'verify_release_apk': SimpleNamespace(
                verify_unsigned=Mock(return_value=report))}), \
             patch.object(hook, 'verify_existing_android_signer') as signer:
            hook.admit_android(self.state)
        signer.assert_not_called()
        self.assertFalse(any('--adopt-current' in args for action, args in self.calls
                             if action == 'release-admit'))

    def test_unsigned_buffer_is_hash_bound_and_does_not_move_original(self):
        source = hook.android_workspace() / 'release/app-release-unsigned.apk'
        apk, _, report = self.apk(source, signer_sha256=None, signing_status='unsigned')
        recorded = dict(report, producer_pid=12345, source_commit_observed='a'*40,
                        state='unsigned-awaiting-signing')
        (source.parent / 'unsigned-preparation.json').write_text(json.dumps(recorded))
        state = self.root / '.rust-storage'
        state.mkdir(exist_ok=True)
        (state / 'release-producer.lock').write_text(json.dumps({
            'producer_pid':12345, 'kind':'android-release', 'android_upgrade_candidate':False}))
        with patch.dict(sys.modules, {'verify_release_apk': SimpleNamespace(
                verify_unsigned=Mock(return_value=report))}):
            data, checkpoint = hook.buffer_unsigned(report['sha256'], 12345, 'a'*40)
            self.assertEqual(data, source.read_bytes())
            self.assertTrue(source.exists())
            with self.assertRaises(hook.Refusal):
                hook.buffer_unsigned('0'*64, 12345, 'a'*40)
        self.assertFalse(any(p.exists() for p in hook.android_files()))


    def test_formal_gradle_gate_precedes_output_cleanup(self):
        stage = SCRIPT.parents[1]
        gradle = (stage/'android/app/build.gradle.kts').read_text()
        shell = (stage/'scripts/build_android_release.sh').read_text()
        gate = gradle[gradle.index('tasks.withType<com.android.build.gradle.tasks.PackageAndroidArtifact>'):gradle.index('\ndependencies {')]
        self.assertIn('if (name == "packageRelease") onlyIf',gate); self.assertNotIn('doFirst',gate)
        self.assertIn('outputDirectory.get().asFile.absolutePath',gate)
        self.assertIn('--no-daemon --no-configuration-cache',shell)
        self.assertIn('-Pandroid.injected.apk.location="$APK_WORKSPACE"',shell)
        self.assertNotIn('outputFileName =',gradle); self.assertNotIn('buildDir =',gradle)


class CargoContractTests(unittest.TestCase):
    def test_target_resolves_existing_shared_path_only(self):
        original = hook.macos_output_root
        with tempfile.TemporaryDirectory() as temp:
            home = Path(temp).resolve(); target = home/'rust-target'; target.mkdir()
            alias = home/'alias'; alias.symlink_to(target)
            with patch.object(hook.Path,'home',return_value=home), patch.object(hook.subprocess,'run') as command:
                command.return_value = SimpleNamespace(returncode=0,stdout=json.dumps({'target_directory':str(alias)}))
                self.assertEqual(original(),target/'package/macos')
                command.return_value.stdout = json.dumps({'target_directory':str(home/'other')})
                with self.assertRaises(hook.Refusal): original()
                self.assertIn('--offline',command.call_args.args[0])


if __name__ == '__main__': unittest.main()
