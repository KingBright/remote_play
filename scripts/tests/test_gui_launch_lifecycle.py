import copy
import unittest
from scripts.gui_launch_lifecycle import LifecycleRejected, validate


class GuiLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.path = '/Users/example/Applications/Weavelo.app/Contents/MacOS/weavelo'
        self.gui = {'Label': 'fun.hackerlife.weavelo.host', 'ProgramArguments': [self.path],
                    'RunAtLoad': True, 'KeepAlive': True,
                    'EnvironmentVariables': {'WEAVELO_HEADLESS': '0'}}

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

    def test_independent_activation_cannot_claim_normal_gui_close_survival(self):
        # launchd can start a job independently of KeepAlive after it exits.
        triggers = {'StartInterval': 60, 'StartCalendarInterval': {'Minute': 0},
                    'WatchPaths': ['/tmp/owned-fixture'],
                    'QueueDirectories': ['/tmp/owned-fixture'],
                    'StartOnMount': True, 'LaunchEvents': {'fixture.event': {}}}
        for keep_alive in (False, {'SuccessfulExit': False}):
            for key, value in triggers.items():
                with self.subTest(keep_alive=keep_alive, trigger=key):
                    document = dict(self.gui, ProgramArguments=[self.path, '--gui'],
                                    KeepAlive=keep_alive, **{key: value})
                    before = copy.deepcopy(document)
                    with self.assertRaisesRegex(LifecycleRejected, 'activation'):
                        validate(document, self.path)
                    self.assertEqual(document, before)

    def test_host_schedule_is_not_treated_as_gui_reopen(self):
        document = dict(self.gui, ProgramArguments=[self.path, '--background-service'],
                        KeepAlive=False, StartInterval=60)
        result = validate(document, self.path, role='host')
        self.assertEqual(result['role'], 'host')
        self.assertEqual(result['independent_background_host'], 'not_evaluated')

    def test_a_background_host_can_restart_but_cannot_be_named_as_gui(self):
        self.gui['EnvironmentVariables']['WEAVELO_HEADLESS'] = '1'
        self.assertEqual(validate(self.gui, self.path, role='host')['role'], 'host')
        with self.assertRaises(LifecycleRejected):
            validate(self.gui, self.path, role='gui')

    def test_wrong_executable_missing_role_and_integer_flags_fail_closed(self):
        for changes in ({'Program': '/wrong/weavelo'}, {'ProgramArguments': []},
                        {'EnvironmentVariables': {'WEAVELO_HEADLESS': 1}},
                        {'KeepAlive': 1}, {'KeepAlive': {}},
                        {'KeepAlive': {'SuccessfulExit': 0}}):
            with self.subTest(changes=changes):
                document = dict(self.gui, **changes)
                with self.assertRaises(LifecycleRejected):
                    validate(document, self.path)


    def test_installed_explicit_background_entry_needs_no_headless_environment(self):
        self.gui['ProgramArguments'] = [self.path, '--background-service']
        self.gui['EnvironmentVariables'] = {}
        self.gui['KeepAlive'] = {'SuccessfulExit': False}
        result = validate(self.gui, self.path, role='host')
        self.assertEqual(result['role_source'], 'explicit-entry')
        with self.assertRaises(LifecycleRejected):
            validate(self.gui, self.path, role='gui')

    def test_explicit_gui_cannot_be_claimed_as_host_by_inherited_environment(self):
        self.gui['ProgramArguments'] = [self.path, '--gui']
        self.gui['EnvironmentVariables']['WEAVELO_HEADLESS'] = '1'
        self.gui['KeepAlive'] = False
        self.assertEqual(validate(self.gui, self.path)['role'], 'gui')
        with self.assertRaises(LifecycleRejected):
            validate(self.gui, self.path, role='host')

    def test_unknown_or_mixed_entries_fail_before_policy_claim(self):
        for tail in (['--gui', '--background-service'], ['--background-service', '--gui'],
                     ['--product-info-json'], ['--unknown']):
            with self.subTest(tail=tail), self.assertRaises(LifecycleRejected):
                validate(dict(self.gui, ProgramArguments=[self.path, *tail], KeepAlive=False), self.path)

    def test_program_only_legacy_entry_keeps_environment_role_semantics(self):
        self.gui.pop('ProgramArguments')
        self.gui['Program'] = self.path
        self.gui['EnvironmentVariables']['WEAVELO_HEADLESS'] = '1'
        self.assertEqual(validate(self.gui, self.path, role='host')['role_source'], 'environment-default')


if __name__ == '__main__':
    unittest.main()
