import copy
import unittest
from scripts.gui_launch_lifecycle import LifecycleRejected, validate


class GuiLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.path = '/Users/example/Applications/RemotePlay.app/Contents/MacOS/remote_play'
        self.gui = {'Label': 'com.remoteplay.host', 'ProgramArguments': [self.path],
                    'RunAtLoad': True, 'KeepAlive': True,
                    'EnvironmentVariables': {'REMOTE_PLAY_HEADLESS': '0'}}

    def test_reported_old_gui_reopen_configuration_is_rejected(self):
        before = copy.deepcopy(self.gui)
        with self.assertRaisesRegex(LifecycleRejected, 'Unconditional KeepAlive'):
            validate(self.gui, self.path)
        self.assertEqual(self.gui, before)

    def test_login_start_does_not_require_restarting_normal_gui_exit(self):
        self.gui['KeepAlive'] = False
        result = validate(self.gui, self.path)
        self.assertFalse(result['normal_gui_exit_restarts'])
        self.assertFalse(result['service_modified'])
        self.assertEqual(result['independent_background_host'], 'not_evaluated')

    def test_only_crash_or_unsuccessful_exit_predicates_allow_normal_close(self):
        for policy in ({'SuccessfulExit': False}, {'Crashed': True},
                       {'SuccessfulExit': False, 'Crashed': True}):
            with self.subTest(policy=policy):
                self.gui['KeepAlive'] = policy
                self.assertFalse(validate(self.gui, self.path)['normal_gui_exit_restarts'])

    def test_or_predicate_cannot_hide_normal_exit_restart(self):
        for policy in ({'SuccessfulExit': True},
                       {'SuccessfulExit': False, 'PathState': {'/tmp/exists': True}},
                       {'SuccessfulExit': False, 'NetworkState': True}):
            with self.subTest(policy=policy):
                self.gui['KeepAlive'] = policy
                with self.assertRaises(LifecycleRejected):
                    validate(self.gui, self.path)

    def test_a_background_host_can_restart_but_cannot_be_named_as_gui(self):
        self.gui['EnvironmentVariables']['REMOTE_PLAY_HEADLESS'] = '1'
        self.assertEqual(validate(self.gui, self.path, role='host')['role'], 'host')
        with self.assertRaises(LifecycleRejected):
            validate(self.gui, self.path, role='gui')

    def test_wrong_executable_missing_role_and_integer_flags_fail_closed(self):
        for changes in ({'Program': '/wrong/remote_play'}, {'ProgramArguments': []},
                        {'EnvironmentVariables': {'REMOTE_PLAY_HEADLESS': 1}},
                        {'KeepAlive': 1}, {'KeepAlive': {}},
                        {'KeepAlive': {'SuccessfulExit': 0}}):
            with self.subTest(changes=changes):
                document = dict(self.gui, **changes)
                with self.assertRaises(LifecycleRejected):
                    validate(document, self.path)


if __name__ == '__main__':
    unittest.main()
