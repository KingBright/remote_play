"""Installer transaction tests with synthetic signer/product/native hooks.

Real package inventory and identity-schema checks run; these fixtures do not
claim an observed product build or a real loaded-image acceptance.
"""
from copy import deepcopy
import json
import os
from pathlib import Path
import plistlib
import unittest
from unittest.mock import patch
import zipfile

from scripts.tests.test_installer_hardening import InstallerFixture
import install_macos_unified as installer
import installer_build_identity as adapter
import build_identity_gate as gate
from macos_release_guard import ReleaseRejected, load_policy

REQUIRE_CONTEXT = adapter.require_context


class IdentityInstallerFixture(InstallerFixture):
    def setUp(self):
        super().setUp()
        p=patch.object(adapter,'require_context',REQUIRE_CONTEXT);p.start();self.addCleanup(p.stop)
        for name in ('home', 'applications', 'target', 'source', 'plist', 'executable', 'archive'):
            setattr(self, name, getattr(self, name).resolve())
        self.profile = installer.profile_directory(self.home)
        self.profile.mkdir(parents=True, mode=0o700)
        self.before = {'Label': installer.LABEL, 'ProgramArguments': [str(self.executable)],
                       'KeepAlive': True, 'RunAtLoad': True,
                       'EnvironmentVariables': {'WEAVELO_DEVICE_GROUP_DIR': str(self.profile),
                                                'WEAVELO_DISPLAY_NAME': 'Synthetic fixture'}}
        self.original_plist = plistlib.dumps(self.before)
        self.plist.write_bytes(self.original_plist)
        self.state.update(program=str(self.executable), arguments=[str(self.executable)])
        self.observer = self.home / 'reviewed-observer'
        self.observer.write_bytes(b'synthetic observer, never executed')
        self.infos = {}
        for app, info in ((self.target, self.old), (self.source, self.new)):
            info['executable_sha256'] = gate.hash_file(app / 'Contents/MacOS/weavelo')
            (app / '.fixture-info.json').write_text(json.dumps(info))
        self.value = self.make_contract(self.source, self.new, 'a' * 40)
        self.expected = self.value['expectations']
        self.zip_candidate()
        for obj, name, value in (
                (adapter, 'verified_archive', self.archive_view),
                (gate, 'product_info', self.metadata),
                (gate, 'verify_running_macos', lambda *a: {'actual_loaded_image_verified': True, 'release_authorized': False}),
                (installer, 'product_capabilities', lambda *a: self.infos[self.new['version']]),
                (installer, 'background_health', self.authenticated),
                (installer, 'run', self.command_run)):
            p = patch.object(obj, name, value); p.start(); self.addCleanup(p.stop)

    def make_contract(self, app, signed, commit):
        source = dict(commit=commit, git_tree='b' * 40, state='clean', patch_sha256=None,
                      snapshot_sha256='c' * 64, file_count=1)
        config = gate.configuration('macos', 'aarch64', signed['version'],
                                    'aarch64-apple-darwin', 'dev', 'd' * 64, 'e' * 64)
        identity = gate.identity(source, config)
        info = dict(schema=1, product='Weavelo', version=signed['version'], platform='macos',
                    architecture='aarch64', default_gui=gate.RENDERER, original_gui_compiled=True,
                    native_video_compiled=True, separated_lifecycle=True, frontend_entry='--gui',
                    background_entry='--background-service', build_identity=identity)
        self.infos[signed['version']] = info
        manifest = dict(schema=1, kind='weavelo-release', build_identity=identity,
                        artifact_root='Weavelo.app', executable='Contents/MacOS/weavelo',
                        product_info=deepcopy(info), files=gate.files_in(app),
                        binary_sha256=signed['executable_sha256'],
                        compiler_binary_sha256=signed['executable_sha256'],
                        signing={key:signed[key] for key in ('bundle_id','certificate_sha1',
                                                             'designated_requirement','version','signature_verified')})
        raw = json.dumps(manifest, sort_keys=True).encode()
        expected = adapter.Expectations(commit, identity['identity_sha256'], gate.digest(raw),
                                        str(self.observer), gate.hash_file(self.observer))
        return adapter.contract(raw, expected)

    def zip_candidate(self):
        with zipfile.ZipFile(self.archive, 'w') as output:
            for p in self.source.rglob('*'):
                if p.is_file(): output.write(p, 'Weavelo.app/' + p.relative_to(self.source).as_posix())
            output.writestr(gate.MANIFEST, self.value['raw_manifest'])

    def metadata(self, binary, expected, expected_hash):
        self.assertEqual(gate.hash_file(binary), expected_hash)
        version = json.loads((binary.parents[2] / '.fixture-info.json').read_text())['version']
        info = self.infos[version]
        gate.validate_product_info(info, expected)
        return info

    def command_run(self, args, timeout=45):
        result = super().command_run(args, timeout)
        if args[0] == '/bin/launchctl' and args[1] == 'bootstrap':
            self.state['arguments'] = plistlib.loads(self.plist.read_bytes())['ProgramArguments']
        return result

    def authenticated(self, executable, profile, pid, expected_hash, deadline):
        return dict(background_role_verified=True, pid=pid, executable_sha256=expected_hash)

    def migrate(self):
        return installer.install(self.archive, True, separate_background=True, expected_release=self.expected)

    def old_release(self):
        self.before['ProgramArguments'] = [str(self.executable), '--background-service']
        self.original_plist = plistlib.dumps(self.before); self.plist.write_bytes(self.original_plist)
        self.state['arguments'] = self.before['ProgramArguments']
        value = self.make_contract(self.target, self.old, 'f' * 40)
        raw = adapter.state_bytes(value); adapter.write_new(installer.release_state_path(self.target), raw)
        return raw

    def interrupt(self):
        original = installer.write_journal
        def record(path, document):
            original(path, document)
            if document['phase'] == 'identity_written': raise SystemExit('fixture interruption')
        with patch.object(installer, 'write_journal', record):
            with self.assertRaises(SystemExit): self.migrate()


class IdentityBoundTransactions(IdentityInstallerFixture):
    def test_readonly_preflight_checks_inventory_and_compiled_identity_without_mutation(self):
        plan = installer.install(self.archive, False, separate_background=True, expected_release=self.expected)
        self.assertTrue(plan['build_identity_preflight_verified'])
        self.assertFalse(plan['backend_running_image_verified']); self.assertFalse(plan['applied'])
        self.assertFalse(self.mutations()); self.assertFalse(installer.release_state_path(self.target).exists())

    def test_success_records_manifest_and_actual_backend_separately_from_frontend(self):
        result = self.migrate()
        self.assertTrue(result['backend_running_image_verified'])
        self.assertTrue(result['service']['actual_loaded_image_verified'])
        self.assertFalse(result['service']['frontend_running_image_verified'])
        self.assertTrue(result['rollback_includes_release_identity'])
        self.assertEqual(adapter.read_state(installer.release_state_path(self.target))['raw_manifest'], self.value['raw_manifest'])
        self.assertFalse(result['release_authorized'])

    def test_missing_compiled_identity_fails_before_service_stop(self):
        self.infos[self.new['version']].pop('build_identity')
        with self.assertRaises(gate.IdentityRejected): self.migrate()
        self.assertFalse(self.mutations()); self.assertEqual(self.target_info(), self.old)

    def test_wrong_manifest_anchor_and_payload_tamper_fail_before_stop(self):
        wrong = adapter.Expectations(self.expected.commit,self.expected.identity_sha256,'0'*64,
                                     self.expected.observer,self.expected.observer_sha256)
        with self.assertRaises(gate.IdentityRejected):
            installer.install(self.archive,True,separate_background=True,expected_release=wrong)
        with zipfile.ZipFile(self.archive,'a') as output: output.writestr('Weavelo.app/unlisted',b'changed')
        with self.assertRaises(gate.IdentityRejected): self.migrate()
        self.assertFalse(self.mutations())

    def test_native_identity_failure_rolls_back_previous_app_plist_and_state_absence(self):
        with patch.object(gate,'verify_running_macos',side_effect=gate.IdentityRejected('old mapped image')):
            with self.assertRaisesRegex(ReleaseRejected,'previous app was restored'): self.migrate()
        self.assertEqual(self.target_info(),self.old); self.assertEqual(self.plist.read_bytes(),self.original_plist)
        self.assertFalse(installer.release_state_path(self.target).exists())
        self.assertEqual(self.journal()['phase'],'rolled_back')

    def test_failed_upgrade_restores_exact_previous_release_expectations(self):
        before = self.old_release(); calls=[]
        def native(*args):
            calls.append(args)
            if len(calls)==1: raise gate.IdentityRejected('fixture candidate mapped-image rejection')
            return {'actual_loaded_image_verified':True,'release_authorized':False}
        with patch.object(gate,'verify_running_macos',native):
            with self.assertRaisesRegex(ReleaseRejected,'previous app was restored'): self.migrate()
        self.assertEqual(adapter.private_bytes(installer.release_state_path(self.target)),before)
        self.assertEqual(self.target_info(),self.old); self.assertEqual(len(calls),2)

    def test_interrupted_identity_write_recovers_prior_complete_tuple(self):
        before=self.old_release(); self.interrupt(); installer.recover_interrupted()
        self.assertEqual(self.target_info(),self.old)
        self.assertEqual(self.plist.read_bytes(),self.original_plist)
        self.assertEqual(adapter.private_bytes(installer.release_state_path(self.target)),before)

    def test_concurrent_state_edits_refuse_recovery_before_external_mutations(self):
        self.interrupt(); path=installer.release_state_path(self.target); path.write_bytes(b'changed release state')
        count=len(self.mutations())
        with self.assertRaisesRegex(ReleaseRejected,'concurrently edited'): installer.recover_interrupted()
        self.assertEqual(len(self.mutations()),count); self.assertEqual(path.read_bytes(),b'changed release state')

    def test_tampered_release_backup_refuses_automatic_replay(self):
        self.old_release(); self.interrupt()
        backup=Path(self.journal()['backup']).parent/'release-identity.before.json'; backup.write_bytes(b'changed backup')
        count=len(self.mutations())
        with self.assertRaisesRegex(ReleaseRejected,'backup changed'): installer.recover_interrupted()
        self.assertEqual(len(self.mutations()),count)

    def test_missing_actual_arguments_or_wrong_lifecycle_pid_cannot_pass_backend_gate(self):
        state=dict(self.state,arguments=[str(self.executable),'--background-service'])
        document=dict(self.before,ProgramArguments=state['arguments'])
        auth=self.authenticated(self.executable,self.profile,state['pid'],self.new['executable_sha256'],None)
        with self.assertRaises(gate.IdentityRejected):
            adapter.verify_backend(self.target,self.value,dict(state,arguments=None),document,installer.LABEL,auth)
        with self.assertRaises(gate.IdentityRejected):
            adapter.verify_backend(self.target,self.value,state,document,installer.LABEL,dict(auth,pid=999))

    def test_identity_bound_installation_cannot_take_legacy_upgrade_without_expectations(self):
        self.old_release()
        with self.assertRaisesRegex(ReleaseRejected,'expectations are required'): installer.install(self.archive,True)
        self.assertFalse(self.mutations())

    def test_unloaded_backend_is_reported_and_not_implicitly_started(self):
        self.state=dict(loaded=False,running=False,pid=None)
        plan=installer.install(self.archive,False,separate_background=True,expected_release=self.expected)
        self.assertTrue(plan['apply_blockers']);self.assertFalse(plan['backend_running_image_verified'])
        with self.assertRaisesRegex(ReleaseRejected,'existing loaded backend'):
            self.migrate()
        self.assertFalse(self.mutations());self.assertEqual(self.target_info(),self.old)

    def test_unverified_native_observer_fails_before_any_service_operation(self):
        wrong=adapter.Expectations(self.expected.commit,self.expected.identity_sha256,self.expected.manifest_sha256,
                                   self.expected.observer,'0'*64)
        with self.assertRaises(gate.IdentityRejected):
            installer.install(self.archive,True,separate_background=True,expected_release=wrong)
        self.assertFalse(self.mutations())

    def test_completed_journal_cannot_ignore_a_missing_installed_identity(self):
        self.migrate();installer.release_state_path(self.target).unlink();count=len(self.mutations())
        with self.assertRaisesRegex(ReleaseRejected,'Terminal journal'):
            installer.recover_interrupted()
        self.assertEqual(len(self.mutations()),count)


class IdentityRequirements(InstallerFixture):
    def setUp(self):
        super().setUp()
        p=patch.object(adapter,'require_context',REQUIRE_CONTEXT);p.start();self.addCleanup(p.stop)

    def test_production_background_migration_requires_independent_expectations(self):
        with self.assertRaisesRegex(ReleaseRejected,'expectations are required'):
            installer.install(self.archive,False,separate_background=True)
        self.assertFalse(self.mutations())

    def test_production_apply_cannot_silently_take_the_signature_only_path(self):
        with self.assertRaisesRegex(ReleaseRejected,'expectations are required'):
            installer.install(self.archive,True)
        self.assertFalse(self.mutations());self.assertEqual(self.target_info(),self.old)

    def test_partial_expectations_are_rejected(self):
        with self.assertRaises(ReleaseRejected): adapter.requested(commit='a'*40)


if __name__=='__main__': unittest.main()
