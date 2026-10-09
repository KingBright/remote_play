"""Synthetic metadata fixtures only; no sudo, signing, app launch or user credentials."""
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import install_macos_unified as installer
from macos_release_guard import ReleaseRejected


class InstallerProfileAccessTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name) / 'home'
        self.home.mkdir()
        (self.home / 'Applications').mkdir()
        self.profile = self.home / 'Library/Application Support/RemotePlay/NativeMesh'
        self.uid = os.getuid()

    def fixture_profile(self):
        self.profile.mkdir(parents=True, mode=0o700, exist_ok=True)
        for name in ('mesh.conf', 'mesh.secret'):
            path = self.profile / name
            path.write_bytes(b'synthetic-private-fixture')
            path.chmod(0o600)

    def snapshot(self):
        return {p.name: (p.lstat().st_ino, p.stat().st_mode, p.stat().st_uid,
                         p.stat().st_mtime_ns, p.read_bytes())
                for p in self.profile.iterdir() if not p.is_symlink()}

    def test_user_install_does_not_initialize_missing_profile(self):
        with patch.object(installer.Path, 'home', return_value=self.home):
            home, target, _, domain = installer.canonical_paths()
        self.assertEqual(home, self.home)
        self.assertEqual(target, self.home / 'Applications/RemotePlay.app')
        self.assertEqual(domain, f'gui/{self.uid}')
        self.assertFalse(self.profile.exists())

    def test_root_installer_is_rejected_before_profile_then_user_install_can_plan(self):
        before = list(self.home.rglob('*'))
        with patch.object(installer.Path, 'home', return_value=self.home), patch.object(installer.os, 'getuid', return_value=0):
            with self.assertRaisesRegex(ReleaseRejected, 'logged-in user'):
                installer.canonical_paths()
        self.assertEqual(list(self.home.rglob('*')), before)
        self.assertFalse(self.profile.exists())
        with patch.object(installer.Path, 'home', return_value=self.home):
            installer.canonical_paths()
        self.assertFalse(self.profile.exists())

    def test_effective_root_is_rejected_even_with_non_root_real_uid(self):
        with patch.object(installer.Path, 'home', return_value=self.home), patch.object(installer.os, 'geteuid', return_value=0):
            with self.assertRaisesRegex(ReleaseRejected, 'logged-in user'):
                installer.canonical_paths()
        self.assertFalse(self.profile.exists())

    def test_existing_private_profile_is_metadata_only_and_unchanged(self):
        self.fixture_profile()
        before = self.snapshot()
        with patch.object(installer.Path, 'read_bytes', side_effect=AssertionError('configuration body read')), patch.object(installer.Path, 'read_text', side_effect=AssertionError('configuration body read')):
            installer.validate_user_profile(self.home, self.uid)
        self.assertEqual(before, self.snapshot())

    def test_root_owned_config_fixture_has_explicit_error_without_fixing_or_reading(self):
        self.fixture_profile()
        before = self.snapshot()
        original = Path.lstat
        config = self.profile / 'mesh.conf'
        def metadata(path):
            info = original(path)
            if path == config:
                values = list(info)
                values[4] = 0 if self.uid else 1
                return os.stat_result(values)
            return info
        with patch.object(Path, 'lstat', metadata), patch.object(Path, 'read_bytes', side_effect=AssertionError('body read')):
            with self.assertRaisesRegex(ReleaseRejected, 'explicit administrator ownership recovery'):
                installer.validate_user_profile(self.home, self.uid)
        self.assertEqual(before, self.snapshot())

    def test_config_and_secret_links_are_rejected_without_changing_targets(self):
        for name in ('mesh.conf', 'mesh.secret'):
            with self.subTest(name=name):
                self.fixture_profile()
                path = self.profile / name
                unrelated = self.home / ('unrelated-' + name)
                path.rename(unrelated)
                path.symlink_to(unrelated)
                before = unrelated.read_bytes()
                with self.assertRaisesRegex(ReleaseRejected, 'ordinary single-link'):
                    installer.validate_user_profile(self.home, self.uid)
                self.assertTrue(path.is_symlink())
                self.assertEqual(unrelated.read_bytes(), before)
                path.unlink()
                unrelated.rename(path)

    def test_profile_and_parent_directory_links_are_rejected(self):
        self.fixture_profile()
        original = self.profile.parent
        target = self.home / 'unrelated-directory'
        original.rename(target)
        original.symlink_to(target, target_is_directory=True)
        with self.assertRaisesRegex(ReleaseRejected, 'ordinary directory'):
            installer.validate_user_profile(self.home, self.uid)
        self.assertTrue(original.is_symlink())
        self.assertEqual((target / 'NativeMesh/mesh.conf').read_bytes(), b'synthetic-private-fixture')

    def test_hard_links_and_public_mode_are_rejected_without_permission_repair(self):
        self.fixture_profile()
        config = self.profile / 'mesh.conf'
        alias = self.home / 'alias'
        os.link(config, alias)
        with self.assertRaisesRegex(ReleaseRejected, 'ordinary single-link'):
            installer.validate_user_profile(self.home, self.uid)
        self.assertEqual(config.stat().st_nlink, 2)
        alias.unlink()
        config.chmod(0o644)
        before = self.snapshot()
        with self.assertRaisesRegex(ReleaseRejected, 'permissions were not changed'):
            installer.validate_user_profile(self.home, self.uid)
        self.assertEqual(before, self.snapshot())

    def test_traversal_is_rejected_before_any_profile_access(self):
        with self.assertRaisesRegex(ReleaseRejected, 'parent traversal'):
            installer.validate_user_profile(self.home / '..' / 'home', self.uid)
        self.assertFalse(self.profile.exists())


if __name__ == '__main__':
    unittest.main()
