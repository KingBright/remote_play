import importlib.util
import json
from pathlib import Path
import plistlib
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'installation_diagnostics', Path(__file__).resolve().parents[1] / 'diagnose_macos_installation.py')
diagnostics = importlib.util.module_from_spec(spec)
spec.loader.exec_module(diagnostics)


class InstallationDiagnosticsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)
        self.app = self.home / 'Applications/RemotePlay.app'
        self.exe = self.app / 'Contents/MacOS/remote_play'
        self.profile = self.home / 'Library/Application Support/RemotePlay/NativeMesh'
        self.plist = self.home / 'Library/LaunchAgents/com.remoteplay.host.plist'
        self.write_bundle(self.app, '2.0.0-alpha.8', '20261004.80')
        self.plist.parent.mkdir(parents=True)
        self.plist.write_bytes(plistlib.dumps({'Label': diagnostics.LABEL, 'ProgramArguments': [str(self.exe)]}))
        self.profile.mkdir(parents=True)
        self.profile.joinpath('desktop-instance.renderer.json').write_text(json.dumps({'schema': 1, 'renderer': diagnostics.ORIGINAL}))
        self.process_output = f'25060 {self.exe}\n'
        self.service_output = f'    program = {self.exe}\n    state = running\n    pid = 25060\n'
        self.calls = []

    def write_bundle(self, app, version, build):
        app.joinpath('Contents').mkdir(parents=True, exist_ok=True)
        app.joinpath('Contents/Info.plist').write_bytes(plistlib.dumps({
            'CFBundleIdentifier': 'com.remoteplay.unified', 'CFBundleExecutable': 'remote_play',
            'RemotePlayReleaseVersion': version, 'CFBundleVersion': build}))

    def run_command(self, argv, **kwargs):
        self.calls.append(argv)
        self.assertEqual(kwargs, dict(capture_output=True, text=True, timeout=10))
        if argv == ['/bin/ps', '-axo', 'pid=,comm=']:
            return SimpleNamespace(returncode=0, stdout=self.process_output, stderr='')
        if argv == ['/bin/launchctl', 'print', f'gui/{self.uid}/com.remoteplay.host']:
            return SimpleNamespace(returncode=0, stdout=self.service_output, stderr='')
        self.fail(f'Unexpected execution: {argv}')

    @property
    def uid(self):
        return self.home.stat().st_uid

    def collect(self):
        return diagnostics.collect(self.home, self.uid, '2.0.0-alpha.8', self.run_command)

    def test_old_install_and_development_copy_are_reported_separately(self):
        self.write_bundle(self.app, '2.0.0-alpha.7', '20260930.7')
        other = self.home / 'target/package/RemotePlay Unified.app'
        self.write_bundle(other, '2.0.0-alpha.5', '20260929.5')
        self.process_output += f'18849 {other}/Contents/MacOS/remote_play\n'
        report = self.collect()
        self.assertIn('installed_version_differs_from_expected', report['findings'])
        self.assertIn('multiple_remoteplay_processes', report['findings'])
        self.assertIn('noncanonical_remoteplay_process', report['findings'])
        self.assertEqual(report['processes'][1]['on_disk_bundle']['version'], '2.0.0-alpha.5')
        self.assertEqual(len(self.calls), 2)

    def test_even_original_metadata_cannot_prove_live_renderer_or_running_version(self):
        report = self.collect()
        self.assertFalse(report['profile_renderer_metadata']['live_gui_verified'])
        self.assertEqual(report['profile_renderer_metadata']['process_binding'], 'unavailable')
        self.assertEqual(report['processes'][0]['running_version'], 'not_verified_from_process')
        self.assertEqual(set(report['acceptance'].values()), {'not_evaluated'})

    def test_stale_metadata_without_process_is_not_a_running_gui(self):
        self.process_output = ''
        report = self.collect()
        self.assertEqual(report['processes'], [])
        self.assertFalse(report['profile_renderer_metadata']['live_gui_verified'])

    def test_no_configuration_body_is_read_or_emitted(self):
        for name in ('mesh.conf', 'mesh.secret'):
            self.profile.joinpath(name).write_text('PRIVATE_SENTINEL_9f33')
        original_open = diagnostics.os.open
        def guarded_open(path, *args):
            self.assertNotIn(Path(path).name, ('mesh.conf', 'mesh.secret'))
            return original_open(path, *args)
        with patch.object(diagnostics.os, 'open', side_effect=guarded_open):
            report = self.collect()
        self.assertNotIn('PRIVATE_SENTINEL', json.dumps(report))
        self.assertFalse(report['configuration_contents_read'])
        self.assertFalse(report['application_started'])
        self.assertFalse(report['system_mutated'])

    def test_other_owner_is_reported_without_opening_or_fixing_files(self):
        metadata = {'status': 'observed', 'regular_file': True, 'owner_uid': self.uid + 1, 'mode': '0600', 'symlink': False}
        with patch.object(diagnostics, 'file_metadata', return_value=metadata):
            report = self.collect()
        self.assertIn('protected_configuration_owned_by_another_user', report['findings'])
        self.assertFalse(report['system_mutated'])

    def test_secret_symlink_is_only_inspected_as_metadata(self):
        target = self.home / 'private-target'
        target.write_text('PRIVATE_SENTINEL')
        self.profile.joinpath('mesh.secret').symlink_to(target)
        report = self.collect()
        self.assertTrue(report['protected_file_metadata']['identity']['symlink'])
        self.assertNotIn('PRIVATE_SENTINEL', json.dumps(report))

    def test_launcher_mismatch_and_arbitrary_arguments_are_not_echoed(self):
        self.plist.write_bytes(plistlib.dumps({'Label': diagnostics.LABEL, 'ProgramArguments': [str(self.exe), 'PRIVATE_ARGUMENT']}))
        report = self.collect()
        self.assertIn('launcher_does_not_match_canonical_app', report['findings'])
        self.assertNotIn('PRIVATE_ARGUMENT', json.dumps(report))

    def test_loaded_service_is_compared_independently_from_plist(self):
        self.service_output = ' program = /old/remote_play\n state = running\n pid = 77\n'
        report = self.collect()
        self.assertTrue(report['launcher']['canonical'])
        self.assertIn('loaded_service_does_not_match_canonical_app', report['findings'])

    def test_service_environment_is_not_emitted(self):
        self.service_output += ' environment = {\n PRIVATE_KEY = PRIVATE_SENTINEL\n }\n'
        self.assertNotIn('PRIVATE_SENTINEL', json.dumps(self.collect()))

    def test_legacy_missing_renderer_is_unknown(self):
        self.profile.joinpath('desktop-instance.renderer.json').unlink()
        report = self.collect()
        self.assertIn('profile_renderer_metadata_unavailable', report['findings'])
        self.assertIsNone(report['profile_renderer_metadata']['renderer'])

    def test_malformed_and_unknown_renderer_metadata_are_not_trusted(self):
        path = self.profile / 'desktop-instance.renderer.json'
        for data in ('not json', '[]', '{"schema":true,"renderer":"restored-original-gpui"}', '{"schema":1,"renderer":"invented"}'):
            with self.subTest(data=data):
                path.write_text(data)
                self.assertIsNone(self.collect()['profile_renderer_metadata']['renderer'])

    def test_diagnostic_renderer_is_flagged(self):
        self.profile.joinpath('desktop-instance.renderer.json').write_text('{"schema":1,"renderer":"egui-diagnostic"}')
        self.assertIn('profile_renderer_metadata_is_not_original_gui', self.collect()['findings'])

    def test_renderer_symlink_is_not_followed(self):
        path = self.profile / 'desktop-instance.renderer.json'
        path.unlink()
        path.symlink_to(self.home / 'unrelated')
        self.assertIsNone(self.collect()['profile_renderer_metadata']['renderer'])

    def test_oversized_and_non_regular_metadata_are_bounded(self):
        path = self.profile / 'desktop-instance.renderer.json'
        path.write_bytes(b'x' * (diagnostics.LIMIT + 1))
        self.assertEqual(self.collect()['profile_renderer_metadata']['status'], 'oversized')
        path.unlink()
        path.mkdir()
        self.assertIsNone(self.collect()['profile_renderer_metadata']['renderer'])

    def test_failed_process_observation_is_incomplete(self):
        def runner(argv, **kwargs):
            if argv[0] == '/bin/ps':
                return SimpleNamespace(returncode=1, stdout='', stderr='PRIVATE_FAILURE')
            return self.run_command(argv, **kwargs)
        report = diagnostics.collect(self.home, self.uid, runner=runner)
        self.assertEqual(report['diagnostic_status'], 'incomplete')
        self.assertNotIn('PRIVATE_FAILURE', json.dumps(report))

    def test_timeout_does_not_trigger_another_execution(self):
        def runner(argv, **kwargs):
            self.calls.append(argv)
            raise subprocess.TimeoutExpired(argv, 10)
        report = diagnostics.collect(self.home, self.uid, runner=runner)
        self.assertEqual(report['diagnostic_status'], 'incomplete')
        self.assertEqual(len(self.calls), 2)

    def test_service_absence_requires_documented_response(self):
        for error, expected in [('Could not find service', 'not_loaded'), ('Operation not permitted', 'command_failed')]:
            with self.subTest(error=error):
                result = diagnostics.observe_command(['/bin/launchctl', 'print', 'gui/501/com.remoteplay.host'],
                    lambda *a, **kw: SimpleNamespace(returncode=1, stdout='', stderr=error))
                self.assertEqual(result['status'], expected)

    def test_missing_profile_is_not_created(self):
        import shutil
        shutil.rmtree(self.profile)
        self.collect()
        self.assertFalse(self.profile.exists())


if __name__ == '__main__':
    unittest.main()
