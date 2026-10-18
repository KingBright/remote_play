import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import hashlib
import os
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch
import install_native_first_identity as m


class Gate:
    @staticmethod
    def hash_file(path):
        return hashlib.sha256(path.read_bytes()).hexdigest()


class SafetyTests(unittest.TestCase):
    def test_initial_journal_never_overwrites(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root).resolve()/'journal.json'
            m.atomic_state(path, {'phase':'first'}, initial=True)
            before = path.read_bytes()
            with self.assertRaises(FileExistsError):
                m.atomic_state(path, {'phase':'second'}, initial=True)
            self.assertEqual(path.read_bytes(), before)

    def test_unknown_pending_preserved(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root).resolve()/'journal.json'
            m.atomic_state(path, {'phase':'first'}, initial=True)
            pending = path.with_suffix('.pending')
            pending.write_bytes(b'unknown')
            with self.assertRaises(FileExistsError):
                m.atomic_state(path, {'phase':'second'})
            self.assertEqual(pending.read_bytes(), b'unknown')

    def test_linked_payload_refused(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            (root/'source').write_text('x')
            (root/'link').symlink_to(root/'source')
            with self.assertRaises(RuntimeError):
                m.inventory(root, Gate)

    def test_hardlinked_payload_refused(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            (root/'source').write_text('x')
            os.link(root/'source', root/'other')
            with self.assertRaises(RuntimeError):
                m.inventory(root, Gate)

    def test_plan_drift_cannot_create_transaction(self):
        with tempfile.TemporaryDirectory() as root:
            install = m.NativeInstall.__new__(m.NativeInstall)
            install.work = Path(root).resolve()/'not-created'
            install.plan = lambda: {'reviewed':'actual'}
            with self.assertRaises(RuntimeError):
                install.apply(m.digest({'reviewed':'other'}))
            self.assertFalse(install.work.exists())

    def test_runtime_does_not_import_old_credentials(self):
        install = m.NativeInstall.__new__(m.NativeInstall)
        install.windows = False
        install.profile = Path('/new/profile')
        install.old_profiles = [Path('/old/profile')]
        with patch.dict(os.environ, {'REMOTE_PLAY_DEVICE_GROUP_DIR':'/wrong','WEAVELO_SECRET':'not-a-real-secret','RP_MEDIA_ACCEPTANCE_TOKEN':'fixture'}, clear=True):
            env = install.runtime_env()
        self.assertEqual(env['WEAVELO_DEVICE_GROUP_DIR'], '/new/profile')
        self.assertNotIn('REMOTE_PLAY_DEVICE_GROUP_DIR', env)
        self.assertNotIn('WEAVELO_SECRET', env)
        self.assertNotIn('RP_MEDIA_ACCEPTANCE_TOKEN', env)

    def test_rollback_environment_targets_only_old_profile(self):
        install = m.NativeInstall.__new__(m.NativeInstall)
        install.windows = False
        install.profile = Path('/new/profile')
        install.old_profiles = [Path('/old/profile')]
        with patch.dict(os.environ, {}, clear=True):
            env = install.runtime_env(old=True)
        self.assertEqual(env['REMOTE_PLAY_DEVICE_GROUP_DIR'], '/old/profile')
        self.assertNotIn('WEAVELO_DEVICE_GROUP_DIR', env)

    def test_windows_production_and_rollback_launches_restore_duplex_host(self):
        # Exercise the real launcher, not just the environment helper; no child
        # or window may start while checking restoration from receive-only env.
        for old in (False, True):
            with self.subTest(rollback=old):
                install = m.NativeInstall.__new__(m.NativeInstall)
                install.windows = True
                install.exe = Path('/new/app/weavelo.exe')
                install.old_exe = Path('/old/app/remote_play.exe')
                install.profile = Path('/new/profile')
                install.old_profiles = [Path('/old/profile')]
                inherited = {'WEAVELO_PASSIVE_HOST': '0', 'REMOTE_PLAY_PASSIVE_HOST': '0',
                             'WEAVELO_RESTORED_TEST_OUTPUT': '/fixture-only',
                             'PSExecutionPolicyPreference': 'Restricted', 'PATH': 'existing'}
                with patch.dict(os.environ, inherited, clear=True), \
                     patch.object(m.subprocess, 'CREATE_NEW_PROCESS_GROUP', 512, create=True), \
                     patch.object(m.subprocess, 'Popen') as start:
                    install.launch_windows(old=old)
                    self.assertEqual(dict(os.environ), inherited)
                args, kwargs = start.call_args
                prefix = 'REMOTE_PLAY_' if old else 'WEAVELO_'
                self.assertEqual(args[0], [str(install.old_exe if old else install.exe)])
                self.assertEqual(kwargs['env'][prefix + 'PASSIVE_HOST'], '1')
                self.assertEqual(kwargs['env'][prefix + 'HEADLESS'], '0')
                self.assertEqual(kwargs['env'][prefix + 'DEVICE_GROUP_DIR'],
                                 '/old/profile' if old else '/new/profile')
                self.assertEqual(kwargs['env']['PSExecutionPolicyPreference'], 'Restricted')
                self.assertNotIn('WEAVELO_RESTORED_TEST_OUTPUT', kwargs['env'])
                self.assertNotIn(('WEAVELO_' if old else 'REMOTE_PLAY_') + 'DEVICE_GROUP_DIR', kwargs['env'])

    def test_unit_environment_injection_refused(self):
        install = m.NativeInstall.__new__(m.NativeInstall)
        install.exe = Path('/owned/weavelo')
        install.profile = Path('/owned/profile')
        with patch.dict(os.environ, {'DISPLAY':':0\nExecStart=/unexpected'}, clear=True):
            with self.assertRaises(RuntimeError):
                install.unit_bytes()

    def test_windows_native_check_remains_available_after_shortcut_creation(self):
        install = m.NativeInstall.__new__(m.NativeInstall)
        install.windows = True
        class NativeGate:
            @staticmethod
            def verify_running_windows(*args):
                return {'verified':True}
        install.gate = NativeGate
        self.assertEqual(install.native(1,Path('/exe'),'a'*64,{}),{'verified':True})

    def test_old_windows_failed_ipc_is_never_claimed_idle(self):
        install = m.NativeInstall.__new__(m.NativeInstall)
        install.windows = True
        install.old_exe = Path('/old/exe')
        install.runtime_env = lambda old: {}
        with patch.object(m.subprocess,'run',return_value=SimpleNamespace(returncode=1)):
            report = install.require_idle()
        self.assertFalse(report['idle_verified'])
        self.assertFalse(report['available'])

    def test_available_active_sessions_still_refused(self):
        install = m.NativeInstall.__new__(m.NativeInstall)
        install.windows = True
        install.old_exe = Path('/old/exe')
        install.runtime_env = lambda old: {}
        result = SimpleNamespace(returncode=0,stdout='{"ok":true,"sessions":{"pending":1}}')
        with patch.object(m.subprocess,'run',return_value=result):
            with self.assertRaises(RuntimeError):
                install.require_idle()


    def test_private_directory_failure_preserves_old_owner_and_never_launches(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root).resolve()
            install = m.NativeInstall.__new__(m.NativeInstall)
            install.windows = True
            install.work = root/'work'
            install.journal = install.work/'journal.json'
            install.candidate = root/'candidate'
            install.candidate.mkdir()
            (install.candidate/'weavelo.exe').write_bytes(b'fixture executable')
            install.stage = root/'stage'
            install.profile = root/'profile-parent'/'Mesh'
            install.gate = Gate
            plan = {'payload':m.inventory(install.candidate, Gate)}
            install.plan = lambda: plan
            install.native = Mock()
            install.launch_windows = Mock()
            with patch('windows_private_profile.NativeSecurity') as security, patch('windows_process_lifecycle.stop_verified') as stop:
                security.return_value.create_private_directory.side_effect = RuntimeError('fixture ACL refusal')
                with self.assertRaisesRegex(RuntimeError, 'observe original private journal'):
                    install.apply(m.digest(plan))
                security.return_value.create_private_directory.assert_called_once_with(install.profile)
                stop.assert_not_called()
            install.native.assert_not_called()
            install.launch_windows.assert_not_called()
            self.assertFalse(install.profile.exists())
            self.assertEqual(m.json.loads(install.journal.read_text())['phase'], 'failed_recovery_required')


if __name__ == '__main__':
    unittest.main()
