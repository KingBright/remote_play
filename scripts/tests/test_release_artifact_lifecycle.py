import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'release_artifact_lifecycle.py'
spec = importlib.util.spec_from_file_location('release_artifact_lifecycle_fixture', SCRIPT)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)


class ReleaseHookTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='rp-retention-hook-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        (self.root / 'VERSION').write_text('2.0.0-alpha.8\n')
        (self.root / 'scripts').mkdir()
        self.addCleanup(patch.stopall)
        patch.object(hook, 'ROOT', self.root).start()
        patch.dict(os.environ, {}, clear=False).start()
        for key in (hook.LOCK_ENV, hook.GENERATION_ENV, 'REMOTE_PLAY_PREBUILT_BINARY'):
            os.environ.pop(key, None)
        self.calls = []
        def public(action, *arguments):
            self.calls.append((action, list(map(str, arguments))))
            return {'status': 'completed', 'admission': 'reserved', 'registration': 'created',
                    'registered_deliveries': [], 'registered_deliveries_complete': True}
        patch.object(hook, 'public', side_effect=public).start()
        patch.object(hook, 'macos_output_root', return_value=self.root / 'cargo-shared/package/macos').start()
        patch.object(hook, 'git', side_effect=lambda *args: '' if args[0] == 'status' else 'a' * 40).start()
        self.state = {'generation': 'release-one', 'generation_basis': 'explicit-delivery-group',
                      'android_deliveries': [], 'deliveries_complete': True}

    def apk(self, **changes):
        apk, receipt = hook.android_files(); apk.parent.mkdir(parents=True, exist_ok=True)
        apk.write_bytes(b'synthetic signed APK bytes')
        report = {'sha256': hashlib.sha256(apk.read_bytes()).hexdigest(), 'size_bytes': apk.stat().st_size,
                  'signer_sha256': hook.ANDROID_SIGNER, 'debuggable': False, 'abi': ['arm64-v8a'],
                  'native_16kb_aligned': True, 'version': '2.0.0-alpha.8', 'version_code': 9}
        report.update(changes); receipt.write_text(json.dumps(report))
        return apk, receipt, report

    def test_two_platforms_use_same_producer_generation(self):
        mac = hook.generation('macos'); android = hook.generation('android-release')
        self.assertEqual(mac, android)
        self.assertEqual(mac[0], 'producer-2.0.0-alpha.8-' + 'a' * 40)
        self.assertEqual(mac[1], 'producer-checkout-observation-only')

    def test_prebuilt_requires_explicit_generation(self):
        os.environ['REMOTE_PLAY_PREBUILT_BINARY'] = '/synthetic/prebuilt'
        with self.assertRaises(hook.Refusal): hook.generation('macos')
        os.environ[hook.GENERATION_ENV] = 'shared-release-generation'
        self.assertEqual(hook.generation('macos')[0], 'shared-release-generation')

    def test_direct_guard_requires_explicit_generation(self):
        with self.assertRaises(hook.Refusal): hook.generation('macos', explicit_required=True)

    def test_dirty_checkout_refuses_automatic_group(self):
        with patch.object(hook, 'git', return_value=' M app/src/main.rs'):
            with self.assertRaises(hook.Refusal): hook.generation('macos')

    def test_source_change_blocks_post_publish(self):
        _, receipt, _ = self.apk()
        state = {'generation': 'producer-2.0.0-alpha.8-' + 'b' * 40,
                 'generation_basis': 'producer-checkout-observation-only'}
        with self.assertRaises(hook.Refusal): hook.publish_android(state)
        self.assertEqual(self.calls, [])

    def test_producer_calls_real_preflight_action_before_child(self):
        with hook.producer('android-native') as (_, state):
            self.assertIsNone(state['generation'])
            self.assertEqual([call[0] for call in self.calls], ['release-setup', 'release-preflight'])
            self.assertIn('--output-root', self.calls[0][1])

    def test_second_producer_refused_without_starting_work(self):
        with hook.producer('android-native'):
            with self.assertRaisesRegex(hook.Refusal, 'Another RemotePlay'):
                with hook.producer('android-native'): self.fail('overlapping producer')
        self.assertEqual(len(self.calls), 2)

    def test_existing_generation_component_refuses_rebuild_before_child(self):
        for explicit in (False, True):
            for archived in (False, True):
                with self.subTest(explicit=explicit, archived=archived):
                    selected = 'explicit-one' if explicit else hook.observed_generation()
                    if explicit:
                        os.environ[hook.GENERATION_ENV] = selected
                    summary = {'registered_deliveries_complete': True, 'registered_deliveries': [
                        {'generation': selected, 'component': 'android-arm64-release', 'archived': archived}]}
                    with patch.object(hook, 'public', return_value=summary), patch.object(hook.subprocess, 'Popen') as child:
                        with self.assertRaisesRegex(hook.Refusal, 'already registered'):
                            hook.run('android-release', [])
                        child.assert_not_called()
                    os.environ.pop(hook.GENERATION_ENV, None)

    def test_same_generation_allows_other_platform_component(self):
        summary = {'registered_deliveries_complete': True, 'registered_deliveries': [
            {'generation': hook.observed_generation(), 'component': 'macos-arm64-release', 'archived': False}]}
        with patch.object(hook, 'public', return_value=summary):
            with hook.producer('android-release') as (_, state):
                self.assertEqual(state['generation'], hook.observed_generation())

    def test_unknown_summary_refuses_complete_release_before_child(self):
        summary = {'mode': 'deferred', 'registered_deliveries_complete': False, 'registered_deliveries': []}
        with patch.object(hook, 'public', return_value=summary), patch.object(hook.subprocess, 'Popen') as child:
            with self.assertRaisesRegex(hook.Refusal, 'identity coverage is unknown'):
                hook.run('android-release', [])
            child.assert_not_called()

    def test_unknown_summary_does_not_block_native_component_build(self):
        summary = {'mode': 'deferred', 'registered_deliveries_complete': False, 'registered_deliveries': []}
        with patch.object(hook, 'public', return_value=summary):
            with hook.producer('android-native') as (_, state):
                self.assertIsNone(state['generation'])

    def test_gc_deferred_with_complete_identity_summary_allows_new_release(self):
        summary = {'mode': 'deferred', 'registered_deliveries_complete': True, 'registered_deliveries': []}
        with patch.object(hook, 'public', return_value=summary):
            with hook.producer('android-release') as (_, state):
                self.assertTrue(state['generation'])

    def test_native_inherits_release_lock_through_bash(self):
        shutil.copy2(SCRIPT, self.root / 'scripts/release_artifact_lifecycle.py')
        with hook.producer('android-release') as (fd, state):
            env = dict(os.environ, **{hook.LOCK_ENV: str(fd)})
            result = subprocess.run(['bash', '-c', 'exec "$1" -B "$2" check-lock android-native',
                                     'fixture', sys.executable, str(self.root / 'scripts/release_artifact_lifecycle.py')],
                                    env=env, pass_fds=(fd,), capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(state['kind'], 'android-release')

    def test_wrong_lock_scope_rejected(self):
        with hook.producer('macos') as (fd, _), patch.dict(os.environ, {hook.LOCK_ENV: str(fd)}):
            with self.assertRaises(hook.Refusal): hook.check_lock('android-native')

    def test_failed_preflight_never_starts_child(self):
        with patch.object(hook, 'public', side_effect=hook.Refusal('unfinished transaction')), patch.object(hook.subprocess, 'Popen') as child:
            with self.assertRaises(hook.Refusal): hook.run('android-release', [])
            child.assert_not_called()

    def test_failed_build_never_publishes_complete_generation(self):
        with patch.object(hook.subprocess, 'Popen') as child:
            child.return_value.wait.return_value = 7
            self.assertEqual(hook.run('android-native', []), 7)
        self.assertEqual([call[0] for call in self.calls], ['release-setup', 'release-preflight'])

    def test_child_receives_existing_cargo_environment_and_success_exit(self):
        overrides = {'CARGO_TARGET_DIR': '/synthetic/shared-target',
                     'CARGO_BUILD_BUILD_DIR': '/synthetic/shared-build', 'RUSTFLAGS': '-Cdebuginfo=2'}
        with patch.dict(os.environ, overrides), patch.object(hook.subprocess, 'Popen') as child:
            child.return_value.wait.return_value = 0
            self.assertEqual(hook.run('android-native', []), 0)
            environment = child.call_args.kwargs['env']
            self.assertEqual({key: environment[key] for key in overrides}, overrides)

    def test_signing_preflight_failure_precedes_any_retention(self):
        with patch.object(hook.subprocess, 'run') as command:
            command.return_value.returncode = 1
            with self.assertRaises(hook.Refusal): hook.run('macos', [])
        self.assertEqual(self.calls, [])

    def test_existing_apk_preserved_before_build(self):
        apk, receipt, report = self.apk()
        with patch.object(hook, 'verify_existing_android_signer') as signer:
            hook.preserve_existing_android(self.state)
            signer.assert_called_once_with(apk)
        action, args = self.calls[-1]
        self.assertEqual(action, 'release-publish')
        self.assertEqual(args[args.index('--generation') + 1], 'legacy-' + report['sha256'])
        self.assertEqual(args[args.index('--validation-kind') + 1], 'existing-signing-evidence')
        self.assertIn('--snapshot', args)
        self.assertEqual(args.count('--file'), 2)
        self.assertEqual(apk.read_bytes(), b'synthetic signed APK bytes')
        self.assertEqual([call[0] for call in self.calls], ['release-admit', 'release-publish'])

    def test_unknown_coverage_never_bootstraps_existing_apk(self):
        apk, _, _ = self.apk(); self.state['deliveries_complete'] = False
        with self.assertRaisesRegex(hook.Refusal, 'coverage is unknown'):
            hook.preserve_existing_android(self.state)
        self.assertTrue(apk.exists()); self.assertEqual(self.calls, [])

    def test_post_gc_error_is_reported_without_reclassifying_valid_build(self):
        self.apk()
        with patch.object(hook, 'public', side_effect=hook.Refusal('cleanup lock held')):
            result = hook.publish_android(self.state)
        self.assertEqual(result['status'], 'deferred')
        self.assertFalse(result['retention_satisfied'])

    def test_preoverwrite_snapshot_failure_still_blocks_build(self):
        self.apk()
        with patch.object(hook, 'verify_existing_android_signer'), patch.object(hook, 'public', side_effect=hook.Refusal('snapshot unavailable')):
            with self.assertRaises(hook.Refusal): hook.preserve_existing_android(self.state)

    def test_known_apk_reuses_registered_generation_without_legacy_duplicate(self):
        _, receipt, _ = self.apk(); value, _ = hook.hashed(receipt)
        self.state['android_deliveries'] = [{'generation': 'prior-source-generation',
                                           'validation_kind': 'signing', 'validation_digest': value, 'archived': False}]
        with patch.object(hook, 'verify_existing_android_signer'):
            hook.preserve_existing_android(self.state)
        args = self.calls[-1][1]
        self.assertEqual(args[args.index('--generation') + 1], 'prior-source-generation')
        self.assertEqual(args[args.index('--validation-kind') + 1], 'signing')
        self.assertEqual([call[0] for call in self.calls], ['release-publish'])

    def test_release_admission_refusal_never_starts_android_producer(self):
        original = hook.public.side_effect
        def full(action, *args):
            result = original(action, *args)
            if action == 'release-admit':
                raise hook.Refusal('two complete generations protected; no slot')
            return result
        with patch.object(hook, 'public', side_effect=full), patch.object(hook.subprocess, 'Popen') as child:
            with self.assertRaisesRegex(hook.Refusal, 'no slot'):
                hook.run('android-release', [])
            child.assert_not_called()
        self.assertEqual([call[0] for call in self.calls], ['release-setup', 'release-preflight', 'release-admit'])
        self.assertFalse(hook.android_files()[0].exists())

    def test_old_apk_snapshot_precedes_new_admission_and_refusal_keeps_fixed_apk(self):
        apk, _, report = self.apk(); original = hook.public.side_effect
        def full(action, *args):
            result = original(action, *args)
            if action == 'release-admit' and args[args.index('--generation') + 1] != 'legacy-' + report['sha256']:
                raise hook.Refusal('no new delivery slot')
            return result
        with patch.object(hook, 'public', side_effect=full), patch.object(hook, 'verify_existing_android_signer'), \
                patch.object(hook.subprocess, 'Popen') as child:
            with self.assertRaisesRegex(hook.Refusal, 'no new delivery slot'):
                hook.run('android-release', [])
            child.assert_not_called()
        self.assertEqual([call[0] for call in self.calls],
                         ['release-setup', 'release-preflight', 'release-admit', 'release-publish', 'release-admit'])
        self.assertEqual(apk.read_bytes(), b'synthetic signed APK bytes')

    def test_unconfirmed_legacy_registration_never_permits_overwrite(self):
        apk, _, _ = self.apk(); original = hook.public.side_effect
        def unconfirmed(action, *args):
            result = original(action, *args)
            return {'status': 'deferred'} if action == 'release-publish' else result
        with patch.object(hook, 'public', side_effect=unconfirmed), patch.object(hook, 'verify_existing_android_signer'), \
                patch.object(hook.subprocess, 'Popen') as child:
            with self.assertRaisesRegex(hook.Refusal, 'registration was not confirmed'):
                hook.run('android-release', [])
            child.assert_not_called()
        self.assertEqual([call[0] for call in self.calls].count('release-admit'), 1)
        self.assertTrue(apk.exists())

    def test_failed_release_keeps_exact_pending_group_and_cannot_change_generation(self):
        original = hook.public.side_effect; pending = []
        def reserve_once(action, *args):
            result = original(action, *args)
            if action == 'release-admit':
                if pending and pending[0] != list(args):
                    raise hook.Refusal('different pending output group')
                if not pending:
                    pending.append(list(args))
            return result
        with patch.object(hook, 'public', side_effect=reserve_once), patch.object(hook.subprocess, 'Popen') as child:
            child.return_value.wait.return_value = 7
            self.assertEqual(hook.run('android-release', []), 7)
            child.reset_mock()
            os.environ[hook.GENERATION_ENV] = 'try-bypass-pending'
            with self.assertRaisesRegex(hook.Refusal, 'different pending'):
                hook.run('android-release', [])
            child.assert_not_called()
        self.assertEqual(len(pending), 1)
        self.assertNotIn('release-publish', [call[0] for call in self.calls])

    def test_native_does_not_reserve_or_publish_when_release_quota_is_full(self):
        summary = {'mode': 'deferred', 'registered_deliveries_complete': True,
                   'registered_deliveries': [{'generation': name, 'component': 'macos-arm64-release',
                                             'archived': False} for name in ('first', 'second')]}
        with patch.object(hook, 'public', return_value=summary) as public, patch.object(hook.subprocess, 'Popen') as child:
            child.return_value.wait.return_value = 0
            self.assertEqual(hook.run('android-native', []), 0)
            self.assertEqual([call.args[0] for call in public.call_args_list], ['release-setup', 'release-preflight'])

    def test_unconfirmed_admission_never_starts_product_build(self):
        original = hook.public.side_effect
        def ambiguous(action, *args):
            result = original(action, *args)
            return {'status': 'deferred'} if action == 'release-admit' else result
        with patch.object(hook, 'public', side_effect=ambiguous), patch.object(hook.subprocess, 'Popen') as child:
            with self.assertRaisesRegex(hook.Refusal, 'admission was not confirmed'):
                hook.run('android-release', [])
            child.assert_not_called()

    def test_macos_reserves_exact_immutable_pair_without_snapshot(self):
        output = self.root / 'cargo-shared/package/macos'
        hook.admit_macos(self.state, output, '2.0.0-alpha.8', '20261013.8')
        action, args = self.calls[-1]
        self.assertEqual(action, 'release-admit')
        self.assertEqual(args[args.index('--generation') + 1], self.state['generation'])
        archive = output / 'RemotePlay-macos-arm64-2.0.0-alpha.8-20261013.8.zip'
        self.assertIn(str(archive), args); self.assertIn(str(archive.with_suffix('.verification.json')), args)
        self.assertNotIn('--snapshot', args)
        self.assertFalse(archive.exists())

    def test_macos_guard_admission_precedes_any_package_production(self):
        # Exercise the existing CLI dispatch using synthetic outputs and mocked signature policy.
        spec = importlib.util.spec_from_file_location('rp_admission_guard_fixture', SCRIPT.with_name('macos_release_guard.py'))
        guard = importlib.util.module_from_spec(spec)
        with patch.dict(sys.modules, {spec.name: guard, 'release_artifact_lifecycle': hook}):
            spec.loader.exec_module(guard)
            binary = self.root / 'synthetic-binary'; binary.write_bytes(b'fixture')
            argv = ['macos_release_guard.py', 'package', '--binary', str(binary), '--output', str(self.root / 'package'),
                    '--version', '2.0.0-alpha.8', '--build', '20261013.8']
            os.environ[hook.GENERATION_ENV] = 'explicit-fixture'
            with patch.object(sys, 'argv', argv), patch.object(guard, 'load_policy'), patch.object(guard, 'preflight'), \
                    patch.object(guard, 'package') as package, patch.object(hook, 'admit_macos', side_effect=hook.Refusal('no slot')):
                with self.assertRaisesRegex(hook.Refusal, 'no slot'):
                    guard.main()
                package.assert_not_called()

    def test_archived_apk_identity_is_not_reminted_as_legacy(self):
        apk, receipt, _ = self.apk(); value, _ = hook.hashed(receipt)
        self.state['android_deliveries'] = [{'generation': 'known-archived-generation',
                                           'validation_kind': 'signing', 'validation_digest': value, 'archived': True}]
        with patch.object(hook, 'verify_existing_android_signer'):
            with self.assertRaisesRegex(hook.Refusal, 'archived delivery'):
                hook.preserve_existing_android(self.state)
        self.assertTrue(apk.exists()); self.assertEqual(self.calls, [])

    def test_unknown_existing_apk_refuses_overwrite(self):
        apk, receipt, _ = self.apk(); receipt.unlink()
        with self.assertRaises(FileNotFoundError): hook.preserve_existing_android(self.state)
        self.assertTrue(apk.is_file()); self.assertEqual(self.calls, [])

    def test_tampered_apk_or_signer_refuses_publish(self):
        for changes in ({'sha256': '0' * 64}, {'signer_sha256': '0' * 64}, {'abi': ['x86_64']}):
            self.apk(**changes)
            with self.assertRaises(hook.Refusal): hook.publish_android(self.state)
        self.assertEqual(self.calls, [])

    def test_actual_legacy_signer_check_is_independent_of_current_jni(self):
        apk, _, _ = self.apk(); sdk = self.root / 'sdk'; tool = sdk / 'build-tools/36.0.0/apksigner'
        tool.parent.mkdir(parents=True); tool.write_text('synthetic verifier')
        with patch.dict(os.environ, {'ANDROID_SDK_ROOT': str(sdk)}), patch.object(hook.subprocess, 'run') as run:
            run.return_value.returncode = 0
            run.return_value.stdout = 'Signer #1 certificate SHA-256 digest: ' + hook.ANDROID_SIGNER
            hook.verify_existing_android_signer(apk)
            self.assertEqual(run.call_args.args[0], [str(tool), 'verify', '--verbose', '--print-certs', str(apk)])

    def test_new_apk_snapshot_is_exact_and_uses_signing_evidence(self):
        apk, receipt, _ = self.apk(); hook.publish_android(self.state)
        _, args = self.calls[-1]
        self.assertIn(str(apk), args); self.assertIn(str(receipt), args)
        self.assertIn('--snapshot', args)
        self.assertNotIn('identity', args)
        self.assertEqual(args.count('--expected-sha256'), 2)

    def test_oversized_post_snapshot_deferred_without_copy(self):
        apk, _, _ = self.apk()
        with patch.object(hook, 'MAX_SNAPSHOT', 1):
            result = hook.publish_android(self.state)
            self.assertEqual(result['status'], 'deferred')
            self.assertFalse(result['retention_satisfied'])
        self.assertTrue(apk.is_file()); self.assertEqual(self.calls, [])

    def test_oversized_preoverwrite_snapshot_refuses_build(self):
        apk, _, _ = self.apk()
        with patch.object(hook, 'MAX_SNAPSHOT', 1), patch.object(hook, 'verify_existing_android_signer'):
            with self.assertRaises(hook.Refusal): hook.preserve_existing_android(self.state)
        self.assertTrue(apk.is_file()); self.assertEqual(self.calls, [])

    def test_mac_immutable_archive_is_not_copied_or_changed(self):
        archive = self.root / 'package/macos/RemotePlay-macos-arm64-v-build.zip'
        archive.parent.mkdir(parents=True); archive.write_bytes(b'synthetic signed zip')
        value, size = hook.hashed(archive)
        report = {'archive': str(archive), 'archive_sha256': value, 'size_bytes': size, 'signature_verified': True}
        receipt = archive.with_suffix('.verification.json'); receipt.write_text(json.dumps(report))
        hook.publish_macos(self.state, report)
        _, args = self.calls[-1]
        self.assertNotIn('--snapshot', args); self.assertEqual(args.count('--file'), 2)
        self.assertEqual(archive.read_bytes(), b'synthetic signed zip')

    def test_shell_entries_require_verified_lock_and_android_publishes_after_gate(self):
        scripts = SCRIPT.parent
        for name in hook.ENTRIES.values():
            source = (scripts / name).read_text()
            self.assertIn('check-lock', source)
        release = (scripts / 'build_android_release.sh').read_text()
        self.assertLess(release.index('verify_release_apk.py'), release.index('publish-android'))
        self.assertNotIn('publish-android', (scripts / 'build_android_native.sh').read_text())


if __name__ == '__main__':
    unittest.main()
