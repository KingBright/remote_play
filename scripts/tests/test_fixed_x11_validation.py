"""Guard the fixed X11 entry without launching a compiler or creating caches."""
import importlib.util
from pathlib import Path
import unittest
from types import SimpleNamespace
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("fixed_validation", Path(__file__).parents[1] / "test_stream_settings_model.py")
ENTRY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ENTRY)


class FixedX11ValidationTests(unittest.TestCase):
    def test_x11_checks_tests_in_existing_target_offline_and_locked(self):
        command = ENTRY.validation_command("cargo", Path("/existing/target"), "linux-x11-check")
        self.assertEqual(command, ["cargo", "check", "--offline", "--locked", "--target-dir",
                                  "/existing/target", "-p", "gpui-ce", "--lib", "--tests",
                                  "--target", "x86_64-unknown-linux-gnu"])

    def test_existing_default_still_runs_stream_settings(self):
        command = ENTRY.validation_command("cargo", Path("/existing/target"), "stream-settings")
        self.assertEqual(command[1], "test")
        self.assertIn("stream_settings::tests", command)
        self.assertNotIn("--target", command)

    def test_zig_configuration_is_target_scoped_and_uses_existing_cache(self):
        with patch.object(ENTRY.os, "environ", {"CC": "native-cc", "RUST_LOG": "trace"}), \
             patch.object(ENTRY.shutil, "which", return_value="/existing/zig"), \
             patch.object(ENTRY.Path, "is_dir", return_value=True):
            env = ENTRY.validation_environment("linux-x11-check")
        self.assertEqual(env["CC"], "native-cc")
        self.assertNotIn("RUST_LOG", env)
        self.assertEqual(env["CC_x86_64_unknown_linux_gnu"], "/existing/zig zig cc -- -target x86_64-linux-gnu")
        self.assertEqual(env["ZIG_GLOBAL_CACHE_DIR"], env["ZIG_LOCAL_CACHE_DIR"])

    def test_missing_existing_cache_is_refused(self):
        with patch.object(ENTRY.shutil, "which", return_value="/existing/zig"), \
             patch.object(ENTRY.Path, "is_dir", return_value=False):
            with self.assertRaisesRegex(ValueError, "existing"):
                ENTRY.validation_environment("linux-x11-check")

    def test_failed_lifecycle_check_stops_before_any_compiler(self):
        with patch.object(ENTRY.sys, "argv", ["fixed", "--mode", "linux-x11-check"]), \
             patch.object(ENTRY.subprocess, "run", return_value=SimpleNamespace(returncode=2, stderr="refused", stdout="")), \
             patch.object(ENTRY.subprocess, "Popen") as launch, \
             patch.object(ENTRY, "validation_environment") as configure:
            self.assertEqual(ENTRY.main(), 2)
            configure.assert_not_called()
            launch.assert_not_called()
