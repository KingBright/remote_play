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
        self.system_states = []
        self.system = {'status': 'not_loaded', 'loaded': False, 'running': False,
                       'pid': None, 'domain': 'system', 'label': 'com.remoteplay.mesh'}
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
                                 (installer, 'system_mesh_state', self.system_mesh_state),
                                 (installer, 'run', self.command_run),
                                 (installer, 'wait_running', self.health)]:
            p = patch.object(obj, name, value, create=name == 'system_mesh_state')
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

    def system_mesh_state(self, *, deadline=None):
        if deadline: deadline.check()
        if self.system_states: return self.system_states.pop(0)
        return dict(self.system)

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


class SystemServiceGaps(InstallerFixture):
    def loaded_root_service(self):
        return {'status': 'observed', 'loaded': True, 'running': True, 'pid': 18849,
                'domain': 'system', 'label': 'com.remoteplay.mesh',
                'program': '/fixture/Old RemotePlay.app/Contents/MacOS/remote_play'}

    def test_dry_plan_classifies_root_service_without_private_hashes(self):
        self.system = self.loaded_root_service()
        with patch.object(installer, 'preserve_hashes', side_effect=AssertionError('private preflight read')):
            plan = installer.install(self.archive, False)
        self.assertTrue(plan['system_mesh']['loaded'])
        self.assertEqual(plan['apply_blockers'], ['system_mesh_loaded'])
        self.assertFalse(plan['will_restart_only_remoteplay'])
        self.assertFalse(self.commands)

    def test_quit_ineffective_root_service_blocks_apply_without_signals(self):
        # A GUI Quit acknowledgement does not unload a launchd system service.
        self.system = self.loaded_root_service()
        with patch.object(installer, 'preserve_hashes', side_effect=AssertionError('private preflight read')):
            with self.assertRaisesRegex(ReleaseRejected, 'system/com.remoteplay.mesh'):
                installer.install(self.archive, True)
        self.assertEqual(self.target_info(), self.old)
        self.assertFalse(self.commands)
        self.assertFalse((self.applications / '.remoteplay-install-journal.json').exists())

    def test_unknown_system_state_blocks_apply(self):
        self.system = {'status': 'unavailable', 'loaded': None, 'reason': 'timeout',
                       'domain': 'system', 'label': 'com.remoteplay.mesh'}
        with self.assertRaisesRegex(ReleaseRejected, 'Cannot determine.*system/com.remoteplay.mesh'):
            installer.install(self.archive, True)
        self.assertFalse(self.commands)

    def test_system_service_appearing_under_lock_blocks_apply(self):
        self.system_states = [dict(self.system), self.loaded_root_service()]
        with self.assertRaisesRegex(ReleaseRejected, 'system/com.remoteplay.mesh'):
            installer.install(self.archive, True)
        self.assertFalse(self.mutations())
        self.assertEqual(self.target_info(), self.old)

    def test_same_version_is_noop_even_with_external_root_service(self):
        self.system = self.loaded_root_service()
        (self.target / '.fixture-info.json').write_text(json.dumps(self.new))
        plan = installer.install(self.archive, True)
        self.assertEqual(plan['action'], 'already_installed')
        self.assertTrue(plan['system_mesh']['loaded'])
        self.assertFalse(self.commands)

    def test_pending_recovery_refuses_active_root_service(self):
        self.crash_after_phase('old_moved')
        self.system = self.loaded_root_service()
        commands = len(self.commands)
        with self.assertRaisesRegex(ReleaseRejected, 'system/com.remoteplay.mesh'):
            installer.recover_interrupted()
        self.assertEqual(len(self.commands), commands)
        self.assertFalse(self.target.exists())
        self.assertEqual(self.journal()['phase'], 'recovery_required')

    def test_quit_ineffective_duplicate_user_process_is_not_retried(self):
        original = self.command_run
        def observe(args, timeout=45):
            if args[0] == '/bin/ps':
                self.commands.append(args)
                return f'100 {self.executable}\n999 {self.executable}\n'
            return original(args, timeout)
        with patch.object(installer, 'run', observe):
            with self.assertRaisesRegex(ReleaseRejected, 'duplicate RemotePlay'):
                installer.install(self.archive, True)
        self.assertFalse(self.mutations())
        self.assertEqual(sum(a[0] == '/bin/ps' for a in self.commands), 1)


class SystemMeshObservation(unittest.TestCase):
    def result(self, returncode=0, stdout='', stderr=''):
        from types import SimpleNamespace
        return SimpleNamespace(returncode=returncode, stdout=stdout, stderr=stderr)

    def test_loaded_system_service_is_external_and_raw_arguments_are_not_echoed(self):
        output = 'system/com.remoteplay.mesh = {\n state = running\n pid = 18849\n program = /fixture/old-wrapper\n arguments = { synthetic-private-marker }\n environment = { synthetic-env-marker }\n}'
        with patch.object(installer.subprocess, 'run', return_value=self.result(stdout=output)) as command:
            state = installer.system_mesh_state()
        self.assertEqual(command.call_args.args[0], ['/bin/launchctl', 'print', 'system/com.remoteplay.mesh'])
        self.assertTrue(state['loaded'])
        self.assertTrue(state['running'])
        self.assertEqual(state['management'], 'not_managed_by_user_installer')
        self.assertEqual(state['pid'], 18849)
        self.assertNotIn('synthetic-private-marker', json.dumps(state))
        self.assertNotIn('synthetic-env-marker', json.dumps(state))

    def test_documented_not_found_is_absence(self):
        output = self.result(113, stderr='Could not find service "com.remoteplay.mesh" in domain for system')
        with patch.object(installer.subprocess, 'run', return_value=output):
            state = installer.system_mesh_state()
        self.assertEqual(state['status'], 'not_loaded')
        self.assertIsNone(installer.system_mesh_blocker(state))

    def test_permission_denied_is_unknown_not_absent(self):
        output = self.result(1, stderr='Operation not permitted; synthetic-private-marker')
        with patch.object(installer.subprocess, 'run', return_value=output):
            state = installer.system_mesh_state()
        self.assertIsNone(state['loaded'])
        self.assertEqual(installer.system_mesh_blocker(state), 'system_mesh_unknown')
        self.assertNotIn('synthetic-private-marker', json.dumps(state))

    def test_query_timeout_has_shared_budget_and_is_unknown(self):
        import subprocess
        with patch.object(installer.time, 'monotonic', side_effect=[10.0, 10.25]):
            deadline = installer.Deadline(1)
            with patch.object(installer.subprocess, 'run', side_effect=subprocess.TimeoutExpired('launchctl', .75)) as command:
                state = installer.system_mesh_state(deadline=deadline)
        self.assertAlmostEqual(command.call_args.kwargs['timeout'], .75)
        self.assertEqual(state['reason'], 'timeout')
        self.assertEqual(installer.system_mesh_blocker(state), 'system_mesh_unknown')
        self.assertEqual(command.call_count, 1)

    def test_expired_budget_does_not_issue_system_query(self):
        with patch.object(installer.time, 'monotonic', side_effect=[10.0, 12.0]):
            deadline = installer.Deadline(1)
            with patch.object(installer.subprocess, 'run') as command:
                with self.assertRaisesRegex(ReleaseRejected, 'deadline'):
                    installer.system_mesh_state(deadline=deadline)
                command.assert_not_called()

    def test_empty_success_is_unknown(self):
        with patch.object(installer.subprocess, 'run', return_value=self.result()):
            state = installer.system_mesh_state()
        self.assertEqual(state['reason'], 'empty_response')
        self.assertEqual(installer.system_mesh_blocker(state), 'system_mesh_unknown')


class SystemMeshRecoveryBoundaries(InstallerFixture):
    loaded_root_service = SystemServiceGaps.loaded_root_service
    def test_loaded_nonrunning_service_blocks_because_it_can_restart(self):
        self.system = dict(self.loaded_root_service(), running=False, pid=None)
        with self.assertRaisesRegex(ReleaseRejected, 'system/com.remoteplay.mesh'):
            installer.install(self.archive, True)
        self.assertFalse(self.commands)

    def test_late_root_service_before_stop_keeps_old_app_and_no_journal(self):
        self.system_states = [dict(self.system), dict(self.system), self.loaded_root_service()]
        with self.assertRaisesRegex(ReleaseRejected, 'system/com.remoteplay.mesh'):
            installer.install(self.archive, True)
        self.assertEqual(self.target_info(), self.old)
        self.assertFalse(self.mutations())
        self.assertFalse((self.applications / '.remoteplay-install-journal.json').exists())
        self.assertFalse(list(self.applications.glob('.remoteplay-update-*')))

    def test_unknown_system_state_blocks_recovery_without_renames(self):
        self.crash_after_phase('old_moved')
        self.system = {'status': 'unavailable', 'loaded': None, 'reason': 'query_failed'}
        before = len(self.commands)
        backup = Path(self.journal()['backup'])
        with self.assertRaisesRegex(ReleaseRejected, 'Cannot determine.*system/com.remoteplay.mesh'):
            installer.recover_interrupted()
        self.assertFalse(self.target.exists())
        self.assertTrue(backup.exists())
        self.assertEqual(len(self.commands), before)

    def test_unknown_external_outcome_keeps_no_replay_priority(self):
        def fail(args):
            if args[0] == '/bin/launchctl' and args[1] == 'bootout':
                raise ReleaseRejected('synthetic stop outcome unknown')
        self.command_hook = fail
        with self.assertRaises(ReleaseRejected): installer.install(self.archive, True)
        self.system = self.loaded_root_service()
        before = len(self.commands)
        with self.assertRaisesRegex(ReleaseRejected, 'External service command outcome unknown'):
            installer.recover_interrupted()
        self.assertEqual(len(self.commands), before)
        self.assertEqual(self.journal()['uncertain_external_phase'], 'stop_requested')
