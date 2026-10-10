import copy
import unittest
from scripts.background_lifecycle_plan import plan, rollback_document, LifecyclePlanRejected


class BackgroundMigrationTests(unittest.TestCase):
    def setUp(self):
        self.exe = '/Users/example/Applications/RemotePlay.app/Contents/MacOS/remote_play'
        self.before = {'Label': 'com.remoteplay.host', 'ProgramArguments': [self.exe],
                       'KeepAlive': True, 'RunAtLoad': True,
                       'EnvironmentVariables': {'REMOTE_PLAY_HEADLESS': '0',
                                                'REMOTE_PLAY_DEVICE_GROUP_DIR': '/private/profile',
                                                'REMOTE_PLAY_DISPLAY_NAME': 'Existing Mac'}}
        self.info = {'separated_lifecycle': True, 'background_entry': '--background-service',
                     'frontend_entry': '--gui', 'default_gui': 'restored-original-gpui',
                     'original_gui_compiled': True}

    def test_keepalive_and_login_start_explicit_background_without_changing_identity(self):
        before = copy.deepcopy(self.before)
        result = plan(self.before, self.exe, self.info)
        self.assertEqual(self.before, before)
        self.assertEqual(result['after']['ProgramArguments'], [self.exe, '--background-service'])
        self.assertTrue(result['after']['KeepAlive'])
        self.assertTrue(result['after']['RunAtLoad'])
        self.assertEqual(result['after']['EnvironmentVariables']['REMOTE_PLAY_DEVICE_GROUP_DIR'], '/private/profile')
        self.assertEqual(result['after']['EnvironmentVariables']['REMOTE_PLAY_DISPLAY_NAME'], 'Existing Mac')
        self.assertFalse(result['applied'])

    def test_env_headless_alone_is_not_a_separation_capability(self):
        for changes in ({'separated_lifecycle': False}, {'background_entry': None},
                        {'frontend_entry': None}, {'default_gui': 'egui-diagnostic'}):
            with self.subTest(changes=changes), self.assertRaises(LifecyclePlanRejected):
                plan(self.before, self.exe, dict(self.info, **changes))

    def test_rollback_restores_exact_prior_policy_but_rejects_concurrent_changes(self):
        result = plan(self.before, self.exe, self.info)
        self.assertEqual(rollback_document(result, result['after']), self.before)
        altered = dict(result['after'], KeepAlive=False)
        with self.assertRaises(LifecyclePlanRejected): rollback_document(result, altered)

    def test_wrong_owner_path_or_missing_profile_is_not_guessed(self):
        for changes in ({'ProgramArguments': ['/wrong/app']}, {'Label': 'remotehosts.agent'},
                        {'EnvironmentVariables': {}}):
            with self.subTest(changes=changes), self.assertRaises(LifecyclePlanRejected):
                plan(dict(self.before, **changes), self.exe, self.info)
