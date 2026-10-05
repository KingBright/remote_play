from __future__ import annotations
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import install_macos_unified as installer
from macos_release_guard import ReleaseRejected, load_policy


class InstallerFixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name) / 'home'
        self.applications = self.home / 'Applications'
        self.applications.mkdir(parents=True)
        self.target = self.applications / 'RemotePlay.app'
        self.source = Path(self.temp.name) / 'source' / 'RemotePlay.app'
        policy = load_policy()
        self.old = dict(version='2.0.0-alpha.7', build='20260930.7',
                        bundle_id=policy.bundle_id, certificate_sha1=policy.certificate_sha1,
                        designated_requirement=policy.requirement,
                        executable_sha256='old', info_sha256='old-info', signature_verified=True)
        self.new = dict(self.old, version='2.0.0-alpha.8', build='20261004.80',
                        executable_sha256='new', info_sha256='new-info')
        self.make_app(self.target, self.old)
        self.make_app(self.source, self.new)
        self.plist = self.home / 'Library/LaunchAgents/com.remoteplay.host.plist'
        self.plist.parent.mkdir(parents=True)
        self.executable = self.target / 'Contents/MacOS/remote_play'
        self.plist.write_bytes(plistlib.dumps({'Label': installer.LABEL,
                                             'ProgramArguments': [str(self.executable)]}))
        self.state = {'loaded': True, 'running': True, 'pid': 100, 'program': str(self.executable)}
        self.commands = []
        self.states = []
        self.verify_count = 0
        self.verify_hook = None
        self.command_hook = None
        self.archive = Path(self.temp.name) / 'candidate.zip'
        self.archive.write_bytes(b'synthetic archive, never executed')
        for obj, name, value in [(installer.Path, 'home', lambda: self.home),
                                 (installer, 'verify_app', self.verify),
                                 (installer, 'verified_archive', self.archive_view),
                                 (installer, 'service_state', self.service_state),
                                 (installer, 'run', self.command_run),
                                 (installer, 'wait_running', self.health)]:
            p = patch.object(obj, name, value)
            p.start(); self.addCleanup(p.stop)

    def make_app(self, path, info):
        path.mkdir(parents=True)
        (path / '.fixture-info.json').write_text(json.dumps(info))
        (path / 'Contents/MacOS').mkdir(parents=True)
        (path / 'Contents/MacOS/remote_play').write_bytes(b'fixture, not executable')

    def verify(self, path, policy, **kwargs):
        self.verify_count += 1
        if kwargs.get('check'): kwargs['check']()
        if self.verify_hook: self.verify_hook(path, self.verify_count)
        if not path.is_dir(): raise ReleaseRejected('Fixture app missing')
        return json.loads((path / '.fixture-info.json').read_text())

    @contextmanager
    def archive_view(self, archive, policy, **kwargs):
        yield self.source, dict(self.new, archive_sha256='synthetic', size_bytes=17)

    def service_state(self, domain, **kwargs):
        if self.states: return self.states.pop(0)
        return dict(self.state)

    def command_run(self, args, timeout=45):
        self.commands.append(args)
        if self.command_hook: self.command_hook(args)
        if args[0] == '/bin/ps':
            return f'{self.state["pid"]} {self.executable}\n' if self.state['running'] else ''
        if args[1] == 'bootout': self.state = {'loaded': False, 'running': False, 'pid': None}
        if args[1] == 'bootstrap':
            self.state = {'loaded': True, 'running': True, 'pid': 101, 'program': str(self.executable)}
        return ''

    def health(self, domain, executable, old_pid, **kwargs):
        return dict(self.state)

    def journal(self):
        return json.loads((self.applications / '.remoteplay-install-journal.json').read_text())

    def target_info(self):
        return json.loads((self.target / '.fixture-info.json').read_text())

    def mutations(self):
        return [a for a in self.commands if a[0] == '/bin/launchctl']

    def crash_after_phase(self, phase):
        original = installer.write_journal
        def record(path, document):
            original(path, document)
            if document['phase'] == phase: raise SystemExit('synthetic process interruption')
        with patch.object(installer, 'write_journal', record):
            with self.assertRaises(SystemExit): installer.install(self.archive, True)


class ExistingProtections(InstallerFixture):
    def test_concurrent_rejection_preserves_lock_inode_and_target(self):
        path = self.applications / '.remoteplay-install.lock'
        with path.open('w') as owner:
            fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
            before = path.stat().st_ino
            with self.assertRaisesRegex(ReleaseRejected, 'Another RemotePlay installer'):
                installer.install(self.archive, True)
            self.assertEqual(path.stat().st_ino, before)
        self.assertEqual(self.target_info(), self.old)
        self.assertFalse(self.mutations())

    def test_stale_preflight_rejects_before_service_mutation(self):
        def change(path, count):
            if count == 2:
                (self.target / '.fixture-info.json').write_text(json.dumps(self.new))
        self.verify_hook = change
        with self.assertRaisesRegex(ReleaseRejected, 'changed during preflight'):
            installer.install(self.archive, True)
        self.assertFalse(self.mutations())

    def test_same_version_is_noop(self):
        (self.target / '.fixture-info.json').write_text(json.dumps(self.new))
        receipt = installer.install(self.archive, True)
        self.assertEqual(receipt['action'], 'already_installed')
        self.assertFalse(receipt['applied'])
        self.assertFalse(self.mutations())


class HardeningGaps(InstallerFixture):
    def test_service_change_under_lock_rejects_before_stop(self):
        other = dict(self.state, pid=999)
        self.states = [dict(self.state), other]
        with self.assertRaisesRegex(ReleaseRejected, 'Service changed during preflight'):
            installer.install(self.archive, True)
        self.assertFalse(self.mutations())

    def test_unknown_journal_is_not_ignored(self):
        path = self.applications / '.remoteplay-install-journal.json'
        path.write_text(json.dumps({'schema': 1, 'phase': 'unrecognized', 'operation_id': 'unknown'}))
        path.chmod(0o600)
        with self.assertRaisesRegex(ReleaseRejected, 'journal|Journal'):
            installer.install(self.archive, True)
        self.assertEqual(self.target_info(), self.old)
        self.assertFalse(self.mutations())

    def test_deadline_is_monotonic_and_expires(self):
        with patch.object(installer.time, 'monotonic', side_effect=[10.0, 10.25, 11.1]):
            deadline = installer.Deadline(1)
            self.assertAlmostEqual(deadline.timeout(45), .75)
            with self.assertRaisesRegex(ReleaseRejected, 'deadline'):
                deadline.check()

    def test_success_has_durable_journal_without_private_config_hashes(self):
        receipt = installer.install(self.archive, True)
        self.assertTrue(receipt['applied'])
        journal = self.journal()
        self.assertEqual(journal['phase'], 'completed')
        self.assertEqual(journal['candidate']['build'], self.new['build'])
        self.assertNotIn('preserved', journal)
        path = self.applications / '.remoteplay-install-journal.json'
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_first_rename_interruption_recovers_old_signed_app(self):
        self.crash_after_phase('old_moved')
        self.assertFalse(self.target.exists())
        self.assertEqual(self.journal()['phase'], 'old_moved')
        receipt = installer.recover_interrupted()
        self.assertEqual(self.target_info(), self.old)
        self.assertEqual(self.journal()['phase'], 'rolled_back')
        self.assertFalse(receipt['applied'])

    def test_new_rename_interruption_rolls_back_without_forward_replay(self):
        self.crash_after_phase('new_installed')
        self.assertEqual(self.target_info(), self.new)
        receipt = installer.recover_interrupted()
        self.assertEqual(self.target_info(), self.old)
        self.assertFalse(receipt['applied'])
        self.assertIsNone(receipt['configuration_unchanged'])

    def test_unknown_external_timeout_is_journalled_not_replayed(self):
        def timeout(args):
            if args[0] == '/bin/launchctl' and args[1] == 'bootout':
                raise ReleaseRejected('synthetic deadline; external outcome unknown')
        self.command_hook = timeout
        with self.assertRaises(ReleaseRejected): installer.install(self.archive, True)
        self.assertEqual(self.target_info(), self.old)
        self.assertEqual(self.journal()['phase'], 'recovery_required')
        self.assertEqual(self.journal()['uncertain_external_phase'], 'stop_requested')
        with self.assertRaisesRegex(ReleaseRejected, 'outcome unknown'):
            installer.recover_interrupted()
        self.assertLessEqual(sum(a[1] == 'bootout' for a in self.mutations()), 1)


class RecoveryBoundaries(InstallerFixture):
    def test_deadline_after_stop_rolls_back_with_separate_bounded_budget(self):
        clock = [10.0]
        original = installer.write_journal
        expired = [False]
        def record(path, document):
            original(path, document)
            if document['phase'] == 'service_stopped' and not expired[0]:
                expired[0] = True
                clock[0] += 301
        with patch.object(installer.time, 'monotonic', lambda: clock[0]), patch.object(installer, 'write_journal', record):
            with self.assertRaisesRegex(ReleaseRejected, 'deadline'):
                installer.install(self.archive, True)
        self.assertEqual(self.target_info(), self.old)
        self.assertEqual(self.journal()['phase'], 'rolled_back')
        self.assertEqual(sum(a[1] == 'bootout' for a in self.mutations()), 1)
        self.assertEqual(sum(a[1] == 'bootstrap' for a in self.mutations()), 1)

    def test_pending_journal_blocks_new_apply(self):
        self.crash_after_phase('old_moved')
        commands = len(self.commands)
        with self.assertRaisesRegex(ReleaseRejected, 'explicit --recover'):
            installer.install(self.archive, True)
        self.assertEqual(len(self.commands), commands)

    def test_repeated_recovery_after_completion_is_noop(self):
        self.crash_after_phase('new_installed')
        installer.recover_interrupted()
        commands = len(self.commands)
        result = installer.recover_interrupted()
        self.assertEqual(result['action'], 'no_recovery_required')
        self.assertEqual(len(self.commands), commands)

    def test_rollback_rename_interruption_is_recoverable(self):
        self.crash_after_phase('new_installed')
        original = installer.write_journal
        def record(path, document):
            original(path, document)
            if document['phase'] == 'old_moved': raise SystemExit('interruption during rollback')
        with patch.object(installer, 'write_journal', record):
            with self.assertRaises(SystemExit): installer.recover_interrupted()
        self.assertFalse(self.target.exists())
        result = installer.recover_interrupted()
        self.assertFalse(result['applied'])
        self.assertEqual(self.target_info(), self.old)

    def test_changed_backup_rejects_without_service_mutation(self):
        self.crash_after_phase('old_moved')
        backup = Path(self.journal()['backup'])
        tampered = dict(self.old, executable_sha256='unexpected')
        (backup / '.fixture-info.json').write_text(json.dumps(tampered))
        commands = len(self.commands)
        with self.assertRaisesRegex(ReleaseRejected, 'bytes do not match'):
            installer.recover_interrupted()
        self.assertEqual(len(self.commands), commands)
        self.assertFalse(self.target.exists())

    def test_unknown_external_start_outcome_does_not_replay(self):
        def fail(args):
            if args[0] == '/bin/launchctl' and args[1] == 'bootstrap':
                raise ReleaseRejected('synthetic timeout after start submission')
        self.command_hook = fail
        with self.assertRaises(ReleaseRejected): installer.install(self.archive, True)
        self.assertEqual(self.journal()['phase'], 'recovery_required')
        self.assertEqual(self.journal()['uncertain_external_phase'], 'start_requested')
        self.assertEqual(sum(a[1] == 'bootstrap' for a in self.mutations()), 1)
        with self.assertRaisesRegex(ReleaseRejected, 'outcome unknown'):
            installer.recover_interrupted()
        self.assertEqual(sum(a[1] == 'bootstrap' for a in self.mutations()), 1)

    def test_lock_inode_survives_success_and_next_noop(self):
        installer.install(self.archive, True)
        lock = self.applications / '.remoteplay-install.lock'
        inode = lock.stat().st_ino
        commands = len(self.mutations())
        result = installer.install(self.archive, True)
        self.assertEqual(result['action'], 'already_installed')
        self.assertEqual(lock.stat().st_ino, inode)
        self.assertEqual(len(self.mutations()), commands)

    def test_invalid_budget_rejects_before_commands(self):
        for seconds in [0, -1, float('inf'), float('nan')]:
            with self.subTest(seconds=seconds):
                with self.assertRaisesRegex(ReleaseRejected, 'finite positive'):
                    installer.install(self.archive, True, deadline_seconds=seconds)
        self.assertFalse(self.commands)

    def test_journal_symlink_rejects_without_touching_destination(self):
        outside = Path(self.temp.name) / 'outside.json'
        outside.write_text('keep')
        (self.applications / '.remoteplay-install-journal.json').symlink_to(outside)
        with self.assertRaises(ReleaseRejected): installer.install(self.archive, True)
        self.assertEqual(outside.read_text(), 'keep')
        self.assertFalse(self.commands)

    def test_health_failure_uses_integrated_rollback(self):
        calls = [0]
        def health(*args, **kwargs):
            calls[0] += 1
            if calls[0] == 1: raise ReleaseRejected('synthetic health failure')
            return dict(self.state)
        with patch.object(installer, 'wait_running', health):
            with self.assertRaisesRegex(ReleaseRejected, 'previous app was restored'):
                installer.install(self.archive, True)
        self.assertEqual(self.target_info(), self.old)
        self.assertEqual(self.journal()['phase'], 'rolled_back')

    def test_prepared_interruption_aborts_without_stopping_service(self):
        self.crash_after_phase('prepared')
        commands = len(self.mutations())
        installer.recover_interrupted()
        self.assertEqual(len(self.mutations()), commands)
        self.assertEqual(self.target_info(), self.old)
        self.assertEqual(self.journal()['phase'], 'rolled_back')


class CommandBudgets(unittest.TestCase):
    def test_external_command_gets_remaining_shared_budget(self):
        with patch.object(installer.time, 'monotonic', side_effect=[10.0, 10.25]):
            deadline = installer.Deadline(1)
            with patch.object(installer, 'run', return_value='observed') as command:
                self.assertEqual(deadline.run(['/fixture/read-only'], 45), 'observed')
            self.assertAlmostEqual(command.call_args.kwargs['timeout'], .75)

    def test_launchctl_observation_uses_remaining_budget(self):
        from types import SimpleNamespace
        output = SimpleNamespace(returncode=0, stdout='state = running\npid = 123\nprogram = /fixture/remote_play\n', stderr='')
        with patch.object(installer.time, 'monotonic', side_effect=[10.0, 10.5]):
            deadline = installer.Deadline(1)
            with patch.object(installer.subprocess, 'run', return_value=output) as command:
                state = installer.service_state('gui/fixture', deadline=deadline)
            self.assertAlmostEqual(command.call_args.kwargs['timeout'], .5)
            self.assertTrue(state['running'])

    def test_exhausted_budget_does_not_issue_command(self):
        with patch.object(installer.time, 'monotonic', side_effect=[10.0, 12.0]):
            deadline = installer.Deadline(1)
            with patch.object(installer, 'run') as command:
                with self.assertRaisesRegex(ReleaseRejected, 'deadline'):
                    deadline.run(['/fixture/mutation'])
                command.assert_not_called()
