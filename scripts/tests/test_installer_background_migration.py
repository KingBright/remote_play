"""Synthetic app/launchd hooks exercise the actual installer transaction.

Native signed fixture/launchd tests are separately opt-in acceptance work.
"""
import fcntl
import os
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch
from scripts.tests.test_installer_hardening import InstallerFixture
import install_macos_unified as installer
import installer_scope
from macos_release_guard import ReleaseRejected


class MigrationFixture(InstallerFixture):
    def setUp(self):
        super().setUp()
        self.profile = installer.profile_directory(self.home)
        self.profile.mkdir(parents=True, mode=0o700)
        for name in ('mesh.conf', 'mesh.secret'):
            path = self.profile / name
            path.write_bytes(b'synthetic fixture identity'); path.chmod(0o600)
        self.before = {'Label': installer.LABEL, 'ProgramArguments': [str(self.executable)],
                       'KeepAlive': True, 'RunAtLoad': True,
                       'EnvironmentVariables': {'REMOTE_PLAY_HEADLESS': '0',
                                                'REMOTE_PLAY_DEVICE_GROUP_DIR': str(self.profile),
                                                'REMOTE_PLAY_DISPLAY_NAME': 'Existing fixture'}}
        self.original = plistlib.dumps(self.before)
        self.plist.write_bytes(self.original)
        self.info = {'separated_lifecycle': True, 'background_entry': '--background-service',
                     'frontend_entry': '--gui', 'default_gui': 'restored-original-gpui',
                     'original_gui_compiled': True}
        for name, value in [('product_capabilities', lambda *a: self.info),
                            ('background_health', lambda *a: {'background_role_verified': True})]:
            p = patch.object(installer, name, value); p.start(); self.addCleanup(p.stop)
        # These existing transaction fixtures have synthetic signatures/metadata.
        # Real expectation rejection and identity-bound transactions have their
        # own tests; this does not introduce a production bypass.
        p = patch.object(installer.release_identity, 'require_context', lambda *a, **k: None)
        p.start(); self.addCleanup(p.stop)

    def migrate(self): return installer.install(self.archive, True, separate_background=True)

    def interrupt(self, phase):
        original = installer.write_journal
        def record(path, document):
            original(path, document)
            if document['phase'] == phase: raise SystemExit('fixture interruption')
        with patch.object(installer, 'write_journal', record):
            with self.assertRaises(SystemExit): self.migrate()


class BackgroundMigrationTransactions(MigrationFixture):
    def test_readonly_plan_never_writes_or_stops_the_old_service(self):
        plan = installer.install(self.archive, False, separate_background=True)
        self.assertFalse(plan['applied']); self.assertEqual(plan['service_role_after'], 'background-service')
        self.assertEqual(self.plist.read_bytes(), self.original)
        self.assertFalse(self.mutations())
        self.assertFalse((self.profile / 'background-service.enabled').exists())

    def test_success_changes_only_launch_role_and_keeps_raw_plist_backup(self):
        metadata=(self.plist.stat().st_uid,self.plist.stat().st_gid,self.plist.stat().st_mode&0o777)
        receipt = self.migrate()
        after = plistlib.loads(self.plist.read_bytes())
        self.assertEqual(after['ProgramArguments'], [str(self.executable), '--background-service'])
        self.assertTrue(after['KeepAlive']); self.assertTrue(after['RunAtLoad'])
        self.assertEqual(after['EnvironmentVariables']['REMOTE_PLAY_DISPLAY_NAME'], 'Existing fixture')
        backup = Path(self.journal()['lifecycle']['backup_plist'])
        self.assertEqual(backup.read_bytes(), self.original)
        self.assertEqual(backup.stat().st_mode & 0o777, 0o600)
        self.assertTrue(receipt['launch_policy_changed'])
        self.assertTrue(receipt['configuration_unchanged'])
        self.assertEqual((self.profile/'mesh.secret').read_bytes(), b'synthetic fixture identity')
        self.assertEqual(metadata,(self.plist.stat().st_uid,self.plist.stat().st_gid,self.plist.stat().st_mode&0o777))

    def test_health_failure_restores_app_plist_and_prior_role_before_restart(self):
        with patch.object(installer, 'background_health', side_effect=ReleaseRejected('fixture background health failed')):
            with self.assertRaisesRegex(ReleaseRejected, 'previous app was restored'): self.migrate()
        self.assertEqual(self.target_info(), self.old)
        self.assertEqual(self.plist.read_bytes(), self.original)
        self.assertFalse((self.profile / 'background-service.enabled').exists())
        self.assertEqual(self.journal()['phase'], 'rolled_back')
        self.assertEqual(self.state['running'], True)

    def test_existing_private_marker_does_not_override_plist_permissions(self):
        self.plist.chmod(0o644)
        marker = self.profile / 'background-service.enabled'
        marker.touch(mode=0o600)
        receipt = self.migrate()
        self.assertTrue(receipt['applied'])
        self.assertEqual(self.journal()['lifecycle']['mode'], 0o644)
        self.assertEqual(self.journal()['lifecycle']['gid'], self.plist.stat().st_gid)
        self.assertEqual(self.plist.stat().st_mode & 0o777, 0o644)
        self.assertEqual(marker.stat().st_mode & 0o777, 0o600)

    def test_app_swaps_and_rollback_hold_the_profile_lock(self):
        original_rename = installer.durable_rename
        guarded_moves = []
        def rename(source, destination):
            fd = os.open(self.profile / 'desktop-instance.lock', os.O_RDONLY)
            try:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            finally: os.close(fd)
            guarded_moves.append((source, destination))
            original_rename(source, destination)
        with patch.object(installer, 'durable_rename', rename), patch.object(
                installer, 'background_health', side_effect=ReleaseRejected('fixture health failure')):
            with self.assertRaisesRegex(ReleaseRejected, 'previous app was restored'): self.migrate()
        self.assertEqual(len(guarded_moves), 4)
        self.assertEqual(self.target_info(), self.old)
        self.assertEqual(self.plist.read_bytes(), self.original)

    def test_interruption_after_plist_write_recovers_complete_prior_tuple(self):
        self.interrupt('lifecycle_written')
        self.assertEqual(self.target_info(), self.new)
        self.assertNotEqual(self.plist.read_bytes(), self.original)
        installer.recover_interrupted()
        self.assertEqual(self.target_info(), self.old)
        self.assertEqual(self.plist.read_bytes(), self.original)
        self.assertFalse((self.profile / 'background-service.enabled').exists())

    def test_concurrent_plist_change_refuses_recovery(self):
        self.interrupt('lifecycle_written')
        changed = dict(self.before, KeepAlive=False)
        self.plist.write_bytes(plistlib.dumps(changed))
        with self.assertRaisesRegex(ReleaseRejected, 'concurrently edited'): installer.recover_interrupted()
        self.assertEqual(plistlib.loads(self.plist.read_bytes()), changed)
        self.assertEqual(self.target_info(), self.new)

    def test_tampered_original_plist_backup_refuses_recovery(self):
        self.interrupt('lifecycle_written')
        backup=Path(self.journal()['lifecycle']['backup_plist'])
        backup.write_bytes(b'changed fixture backup')
        with self.assertRaisesRegex(ReleaseRejected, 'backup changed'): installer.recover_interrupted()
        self.assertEqual(self.target_info(), self.new)

    def test_unsafe_role_marker_is_rejected_before_any_service_operation(self):
        unrelated = self.home / 'unrelated'; unrelated.write_bytes(b'keep')
        (self.profile / 'background-service.enabled').symlink_to(unrelated)
        with self.assertRaisesRegex(ReleaseRejected, 'unsafe'): self.migrate()
        self.assertFalse(self.mutations()); self.assertEqual(unrelated.read_bytes(), b'keep')

    def test_busy_old_profile_never_starts_a_second_runtime(self):
        lock = self.profile / 'desktop-instance.lock'
        fd = os.open(lock, os.O_CREAT | os.O_RDWR, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with patch.object(installer.Deadline, 'sleep', lambda *a: None):
                with self.assertRaisesRegex(ReleaseRejected, 'recovery required'): self.migrate()
            self.assertEqual(self.target_info(), self.old)
            self.assertFalse(any(a[1] == 'bootstrap' for a in self.mutations()))
        finally: os.close(fd)
        installer.recover_interrupted()
        self.assertEqual(self.plist.read_bytes(), self.original)

    def test_ordinary_upgrade_of_background_role_requires_compiled_capability(self):
        after = dict(self.before, ProgramArguments=[str(self.executable), '--background-service'])
        self.plist.write_bytes(plistlib.dumps(after))
        with patch.object(installer, 'product_capabilities', side_effect=ReleaseRejected('candidate lacks separation')):
            with self.assertRaisesRegex(ReleaseRejected, 'lacks separation'): installer.install(self.archive, True)
        self.assertFalse(self.mutations())


class FixtureScopeBoundaries(unittest.TestCase):
    def test_fixture_cannot_use_production_label_or_unmarked_home(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            with self.assertRaises(ReleaseRejected): installer_scope.initialize(root, installer.LABEL)
            self.assertEqual(list(root.iterdir()), [])
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            with self.assertRaises((OSError, ReleaseRejected)):
                with installer_scope.isolated(root, 'com.remoteplay.lifecycle-test.' + 'a'*32): pass

    def test_fixture_is_explicit_independent_and_cannot_fall_back_to_production(self):
        with tempfile.TemporaryDirectory() as temp:
            scope = installer_scope.initialize(Path(temp), 'com.remoteplay.lifecycle-test.'+'b'*32)
            with installer_scope.isolated(scope.root, scope.label):
                home, target, plist, domain = installer.canonical_paths()
                self.assertEqual(home, scope.home)
                self.assertEqual(target.parent, scope.home/'Applications')
                self.assertEqual(plist.name, scope.label+'.plist')
                self.assertNotEqual(installer.service_label(), installer.LABEL)
            self.assertEqual(installer.service_label(), installer.LABEL)


if __name__ == '__main__': unittest.main()
