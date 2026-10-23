"""Original fixed-release scope, without global process exe/cwd/map dossiers."""
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "maintenance"))
import linux_release_activity as activity


class ScopedLinuxReleaseTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.release = self.root / "Weavelo.zip"
        self.paths = [{"path": str(self.release), "directory": False}]
        self.process = "10 1 1000 Mon Oct 12 12:00:00 2026 sleep\n"
        self.commands = "10 /usr/bin/sleep DO_NOT_RETURN_ARGUMENTS\n"
        self.launch = {"hits": [], "warnings": []}

    def collect(self, outputs, *, controls=(), protocol=()):
        with patch.object(activity, "_reference_query", side_effect=outputs) as query, \
             patch.object(activity, "launch_reference_scan", return_value=self.launch):
            report = activity.collect_release_references(
                str(self.root), self.paths, controls, protocol)
        return report, query

    def test_read_access_time_is_not_content_or_owner_identity(self):
        from types import SimpleNamespace
        fields = dict(st_dev=1, st_ino=2, st_mode=0o100600, st_uid=1000,
                      st_gid=1000, st_nlink=1, st_size=42,
                      st_mtime_ns=200, st_ctime_ns=300, st_atime_ns=100)
        first = SimpleNamespace(**fields)
        accessed = SimpleNamespace(**dict(fields, st_atime_ns=999))
        self.assertTrue(activity.same_file_identity(first, accessed))
        for key in ("st_ino", "st_mode", "st_uid", "st_gid", "st_size",
                    "st_mtime_ns", "st_ctime_ns", "st_nlink", "st_dev"):
            with self.subTest(key=key):
                changed = SimpleNamespace(**dict(fields, **{key: fields[key] + 1}))
                self.assertFalse(activity.same_file_identity(first, changed))

    def test_description_apostrophe_is_literal_but_ambiguous_exec_is_unknown(self):
        values = list(activity._launch_values(
            "Description=User's service\nExecStart=/bin/true\n"))
        self.assertIn("User's service", values)
        self.assertIn("/bin/true", values)
        with self.assertRaises(activity.ObservationError):
            list(activity._launch_values('ExecStart="/unterminated\n'))

    @unittest.skipUnless(os.name == "posix", "systemd search-root aliases use POSIX paths")
    def test_systemd_reported_search_root_alias_is_canonicalized_and_scanned(self):
        units = self.root / "units"; units.mkdir()
        alias = self.root / "unit-alias"
        alias.symlink_to(units, target_is_directory=True)
        unit = units / "reference.service"
        unit.write_text("[Service]\nExecStart=" + str(self.release) + "\n")
        with patch.object(activity, "_launch_roots", return_value=[str(alias)]):
            report = activity.launch_reference_scan(self.paths)
        self.assertEqual(report["hits"], [str(unit)])

    def test_absent_paths_need_no_global_descriptor_or_map_query(self):
        report, query = self.collect([self.process, self.commands, self.process])
        self.assertEqual(report["state"], "idle")
        self.assertEqual(query.call_count, 3)
        self.assertNotIn("DO_NOT_RETURN_ARGUMENTS", str(report))
        self.assertFalse(any(c.args[0][0] == activity.LSOF for c in query.call_args_list))

    def test_unrelated_exited_process_does_not_make_release_unknown(self):
        before = self.process + "99 1 1000 Mon Oct 12 12:00:01 2026 other\n"
        report, _ = self.collect([before, self.commands + "99 /bin/echo elsewhere\n", self.process])
        self.assertEqual(report["state"], "idle")

    def test_command_reference_is_busy_and_only_verified_control_is_exempt(self):
        commands = "10 tool --file " + str(self.release) + "\n"
        report, _ = self.collect([self.process, commands, self.process])
        self.assertEqual(report["state"], "busy")
        report, _ = self.collect([self.process, commands, self.process], controls=(10,))
        self.assertEqual(report["state"], "idle")

    def test_pid_reuse_or_missing_command_is_unknown(self):
        for after, commands in (
            (self.process.replace("12:00:00", "12:00:01"), self.commands),
            (self.process, "99 another\n"),
            (self.process, "10\n")):
            with self.subTest(after=after, commands=commands):
                report, _ = self.collect([self.process, commands, after])
                self.assertEqual(report["state"], "unknown")

    def test_existing_file_uses_only_exact_scoped_open_file_query(self):
        self.release.write_bytes(b"existing")
        report, query = self.collect([self.process, self.commands, self.process, ""])
        self.assertEqual(report["state"], "idle")
        self.assertEqual(query.call_args.args[0],
            [activity.LSOF, "-nP", "-F0pfDin", "--", str(self.release)])

    def test_denied_existing_file_query_is_never_idle(self):
        self.release.write_bytes(b"existing")
        with self.assertRaisesRegex(activity.ObservationError, "permission"):
            self.collect([self.process, self.commands, self.process,
                          activity.ObservationError("permission denied")])

    def test_nonprotocol_holder_is_busy_and_protocol_fd_must_match_exactly(self):
        self.release.write_bytes(b"existing")
        opened = "p10\0f3\0D0x1\0i42\0n" + str(self.release) + "\0\n"
        report, _ = self.collect([self.process, self.commands, self.process, opened])
        self.assertEqual(report["state"], "busy")
        proof = [{"pid": 10, "fd": "3", "device": 1, "inode": 42,
                  "path": str(self.release)}]
        report, _ = self.collect([self.process, self.commands, self.process, opened], protocol=proof)
        self.assertEqual(report["state"], "idle")
        report, _ = self.collect([self.process, self.commands, self.process, ""], protocol=proof)
        self.assertEqual(report["state"], "unknown")
        report, _ = self.collect([self.process, self.commands, self.process,
                                  opened.replace("i42", "i43")], protocol=proof)
        self.assertEqual(report["state"], "busy")

    def test_compiler_elsewhere_requires_its_actual_lock_and_stable_identity(self):
        process = self.process.replace("sleep", "cargo")
        commands = "10 cargo build\n"
        refs = "p10\0n/other/project/target/debug/.cargo-lock\0\n"
        report, query = self.collect([process, commands, process, refs, process])
        self.assertEqual(report["state"], "idle")
        self.assertEqual(query.call_args_list[3].args[0],
            [activity.LSOF, "-nP", "-a", "-p", "10", "-F0pn"])
        report, _ = self.collect([process, commands, process, "p10\0n/tmp\0\n", process])
        self.assertEqual(report["state"], "unknown")
        report, _ = self.collect([process, commands, process,
                                  "p10\0n" + str(self.root) + "\0\n", process])
        self.assertEqual(report["state"], "busy")

    def test_launch_reference_alone_blocks_and_original_warning_is_preserved(self):
        self.launch = {"hits": ["/unit/ref.service"], "warnings": []}
        report, _ = self.collect([self.process, self.commands, self.process])
        self.assertEqual(report["state"], "busy")
        warning = {"reason": "unreadable-root-owned-global-launch-config",
                   "inspection_complete": False}
        self.launch = {"hits": [], "warnings": [warning]}
        report, _ = self.collect([self.process, self.commands, self.process])
        self.assertEqual(report["state"], "idle")
        self.assertEqual(report["warnings"], [warning])

    def test_query_rejects_stderr_instead_of_treating_denied_scan_as_empty(self):
        query = Mock()
        query.communicate.return_value = ("", "permission denied")
        query.returncode = 1
        query.__enter__ = Mock(return_value=query)
        query.__exit__ = Mock(return_value=False)
        with patch.object(activity.subprocess, "Popen", return_value=query):
            with self.assertRaises(activity.ObservationError):
                activity._reference_query([activity.LSOF, "--", str(self.release)], empty_code=True)

    def test_own_ps_child_is_verified_and_not_a_spurious_snapshot_change(self):
        query = Mock(pid=22, returncode=0)
        query.communicate.return_value = (
            self.process + f"22 {os.getpid()} {os.getuid()} Mon Oct 12 12:00:00 2026 ps\n",
            "")
        query.__enter__ = Mock(return_value=query)
        query.__exit__ = Mock(return_value=False)
        with patch.object(activity.subprocess, "Popen", return_value=query):
            self.assertEqual(activity._reference_query(activity.PS_COMMAND), self.process)

    @unittest.skipUnless(os.name == "posix", "systemd/XDG fixture uses POSIX account metadata")
    def test_actual_unit_and_xdg_files_are_scanned_without_executing_them(self):
        user = self.root / "units"; user.mkdir()
        system = self.root / "system"; system.mkdir()
        config = self.root / "config"; (config / "autostart").mkdir(parents=True)
        (user / "test.service").write_text("[Service]\nExecStart=" + str(self.release) + "\n")
        (system / "masked.service").symlink_to("/dev/null")
        desktop = config / "autostart/test.desktop"
        desktop.write_text("[Desktop Entry]\nExec=/bin/echo unrelated\n")
        with patch.object(activity, "_launch_roots", side_effect=[
                [str(user)], [str(system)]]), \
             patch.dict(os.environ, {"XDG_CONFIG_HOME": str(config),
                                      "XDG_CONFIG_DIRS": str(self.root / "xdg")}):
            result = activity.launch_reference_scan(self.paths)
        self.assertEqual(result["hits"], [str(user / "test.service")])
        self.assertEqual(result["configurations"], 2)

    @unittest.skipUnless(os.name == "posix", "systemd alias fixture uses POSIX paths")
    def test_unit_exec_modifier_and_environment_path_resolve_release_alias(self):
        user = self.root / "units"; user.mkdir()
        alias = self.root / "alias"; alias.symlink_to(self.root, target_is_directory=True)
        unit = user / "alias.service"
        for value in ("ExecStart=-@" + str(alias / self.release.name),
                      "Environment=OUTPUT=" + str(alias / self.release.name)):
            unit.write_text("[Service]\n" + value + "\n")
            with self.subTest(value=value), \
                 patch.object(activity, "_launch_roots", return_value=[str(user)]):
                result = activity.launch_reference_scan(self.paths)
            self.assertEqual(result["hits"], [str(unit)])

    @unittest.skipUnless(os.name == "posix", "systemd fixture uses POSIX paths")
    def test_launch_alias_outside_reviewed_roots_is_unknown_not_followed(self):
        user = self.root / "units"; user.mkdir()
        private = self.root / "private"; private.write_text("DO_NOT_READ")
        (user / "escape.service").symlink_to(private)
        with patch.object(activity, "_launch_roots", return_value=[str(user)]):
            with self.assertRaisesRegex(activity.ObservationError, "outside reviewed"):
                activity.launch_reference_scan(self.paths)


if __name__ == "__main__":
    unittest.main()
