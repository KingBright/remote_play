import copy
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "maintenance"))
import linux_release_activity as activity


def snapshot(comm="python", parent=1):
    return activity.parse_processes(
        f"10 {parent} 1000 Fri Oct 23 04:00:00 2026 {comm}\n")


def facts_for(processes):
    return {pid: {"identity": dict(value),
                  "scopes": {s: "complete" for s in activity.SCOPES},
                  "paths": {s: [] for s in activity.SCOPES if s != "files"},
                  "files": []} for pid, value in processes.items()}


class LinuxReleaseObservationTests(unittest.TestCase):
    def setUp(self):
        self.before = snapshot()
        self.facts = facts_for(self.before)
        self.launch = {s: {"status": "complete", "paths": []}
                       for s in activity.LAUNCH_SCOPES}
        self.paths = [{"path": "/project/package/current.zip", "directory": False},
                      {"path": "/project/package/.pending", "directory": True}]

    def evaluate(self, **overrides):
        args = dict(before=self.before, after=self.before, facts=self.facts,
                    launch=self.launch, paths=self.paths, root="/project")
        args.update(overrides)
        return activity.classify(**args)

    def test_complete_idle_fixture(self):
        self.assertEqual(self.evaluate()["state"], "idle")

    def test_process_parser_keeps_comm_spaces_and_start_identity(self):
        row = snapshot("(sd pam)")[10]
        self.assertEqual(row["comm"], "(sd pam)")
        self.assertEqual(row["start"], "Fri Oct 23 04:00:00 2026")

    def test_invalid_process_snapshots_are_rejected(self):
        valid = "10 1 1000 Fri Oct 23 04:00:00 2026 python\n"
        for raw in ("", "garbage", valid + valid,
                    valid.replace("10 ", "0 ", 1),
                    valid.replace("Oct 23", "Oct 99"), valid.replace("1000", "x")):
            with self.subTest(raw=raw), self.assertRaises(activity.ObservationError):
                activity.parse_processes(raw)

    def test_empty_enumeration_is_not_idle(self):
        with self.assertRaises(activity.ObservationError):
            self.evaluate(before={}, after={}, facts={})

    def test_missing_process_is_unknown(self):
        self.assertEqual(self.evaluate(facts={})["state"], "unknown")

    def test_other_uid_is_not_ignored(self):
        processes = activity.parse_processes("10 1 0 Fri Oct 23 04:00:00 2026 root-owner\n")
        self.assertEqual(self.evaluate(before=processes, after=processes, facts={})["state"], "unknown")

    def test_every_denied_or_unavailable_scope_blocks(self):
        for scope in activity.SCOPES:
            for status in ("denied", "unavailable"):
                with self.subTest(scope=scope, status=status):
                    facts = copy.deepcopy(self.facts)
                    facts[10]["scopes"][scope] = status
                    self.assertEqual(self.evaluate(facts=facts)["state"], "unknown")

    def test_control_exception_is_command_only(self):
        self.facts[10]["scopes"]["command"] = "unavailable"
        self.facts[10]["paths"]["command"] = ["/project/package/current.zip"]
        self.assertEqual(self.evaluate(controls=(10,))["state"], "idle")
        self.facts[10]["scopes"]["maps"] = "denied"
        self.assertEqual(self.evaluate(controls=(10,))["state"], "unknown")

    def test_unknown_control_pid_rejected(self):
        with self.assertRaises(activity.ObservationError):
            self.evaluate(controls=(999,))

    def test_release_references_in_every_scope_block(self):
        for scope in ("command", "executable", "cwd", "maps"):
            with self.subTest(scope=scope):
                facts = copy.deepcopy(self.facts)
                facts[10]["paths"][scope] = ["/project/package/current.zip"]
                self.assertEqual(self.evaluate(facts=facts)["state"], "busy")

    def test_directory_descendant_is_busy_without_prefix_false_positive(self):
        self.facts[10]["paths"]["cwd"] = ["/project/package/.pending/item"]
        self.assertEqual(self.evaluate()["state"], "busy")
        self.facts[10]["paths"]["cwd"] = ["/project/package/.pending-unrelated/item"]
        self.assertEqual(self.evaluate()["state"], "idle")

    def test_deleted_open_file_remains_busy(self):
        self.facts[10]["paths"]["maps"] = ["/project/package/current.zip (deleted)"]
        self.assertEqual(self.evaluate()["state"], "busy")

    def test_known_busy_does_not_become_idle_during_visibility_gap(self):
        self.facts[10]["paths"]["cwd"] = ["/project/package/current.zip"]
        self.facts[10]["scopes"]["maps"] = "denied"
        self.assertEqual(self.evaluate()["state"], "busy")

    def test_process_exit_new_child_and_pid_reuse_are_unknown(self):
        for after in ({}, {**self.before, 11: dict(self.before[10], pid=11)},
                      {10: dict(self.before[10], start="Fri Oct 23 05:00:00 2026")}):
            self.assertEqual(self.evaluate(after=after)["state"], "unknown")

    def test_stale_fact_or_unenumerated_pid_is_rejected(self):
        facts = copy.deepcopy(self.facts)
        facts[10]["identity"]["uid"] = 0
        with self.assertRaises(activity.ObservationError):
            self.evaluate(facts=facts)
        with self.assertRaises(activity.ObservationError):
            self.evaluate(facts={**self.facts, 11: self.facts[10]})

    def test_noncanonical_paths_and_invalid_scope_status_are_rejected(self):
        for path in ("relative", "/a/../b", "/a//b", "/a\nb"):
            facts = copy.deepcopy(self.facts)
            facts[10]["paths"]["cwd"] = [path]
            with self.subTest(path=path), self.assertRaises(activity.ObservationError):
                self.evaluate(facts=facts)
        self.facts[10]["scopes"]["cwd"] = "assumed-idle"
        with self.assertRaises(activity.ObservationError):
            self.evaluate()

    def file(self, **changes):
        row = dict(pid=10, fd="3", device=1, inode=42,
                   path="/project/package/current.zip")
        row.update(changes)
        return row

    def test_exact_held_protocol_proof_only(self):
        row = self.file()
        self.facts[10]["files"] = [row]
        self.assertEqual(self.evaluate(protocol=(row,))["state"], "idle")
        self.assertEqual(self.evaluate()["state"], "busy")

    def test_protocol_inode_device_fd_pid_or_path_mismatch_blocks(self):
        row = self.file()
        self.facts[10]["files"] = [row]
        for key, value in (("inode", 43), ("device", 2), ("fd", "4"), ("pid", 11),
                           ("path", "/project/package/.pending/other")):
            with self.subTest(key=key):
                result = self.evaluate(protocol=(dict(row, **{key: value}),))
                self.assertNotEqual(result["state"], "idle")

    def test_expected_protocol_missing_and_extra_fd_block(self):
        row = self.file()
        self.assertEqual(self.evaluate(protocol=(row,))["state"], "unknown")
        self.facts[10]["files"] = [row, self.file(fd="4", inode=43)]
        self.assertEqual(self.evaluate(protocol=(row,))["state"], "busy")

    def test_protocol_duplicate_or_boolean_identity_rejected(self):
        row = self.file()
        with self.assertRaises(activity.ObservationError):
            self.evaluate(protocol=(row, row))
        with self.assertRaises(activity.ObservationError):
            self.evaluate(protocol=(dict(row, inode=True),))

    def test_symbolic_descriptor_never_exempted_by_numeric_lock(self):
        row = self.file()
        self.facts[10]["files"] = [self.file(fd="cwd")]
        self.assertEqual(self.evaluate(protocol=(row,))["state"], "busy")

    def test_parser_preserves_space_names_and_exact_file_identity(self):
        raw = "p10\0f3\0D0x1\0i42\0n/project/with spaces.zip\0\n"
        self.assertEqual(activity.parse_open_files(raw),
                         [self.file(path="/project/with spaces.zip")])

    def test_partial_malformed_or_duplicate_open_fields_rejected(self):
        for raw in ("", "p10\0f3\0n/a\0", "p10\0f3\0D1\0i42\0n/a",
                    "p10\0f3\0D1\0i42\0n/a\0n/b\0", "f3\0D1\0i42\0n/a\0"):
            with self.subTest(raw=raw), self.assertRaises(activity.ObservationError):
                activity.parse_open_files(raw)

    def test_every_launch_provider_required_and_references_block(self):
        for source in activity.LAUNCH_SCOPES:
            launch = copy.deepcopy(self.launch)
            launch[source]["status"] = "denied"
            self.assertEqual(self.evaluate(launch=launch)["state"], "unknown")
            launch[source] = {"status": "complete", "paths": ["/project/package/current.zip"]}
            self.assertEqual(self.evaluate(launch=launch)["state"], "busy")
        with self.assertRaises(activity.ObservationError):
            self.evaluate(launch={})

    def test_compiler_here_elsewhere_and_unattributed(self):
        processes = snapshot("cargo")
        facts = facts_for(processes)
        self.assertEqual(self.evaluate(before=processes, after=processes, facts=facts)["state"], "unknown")
        facts[10]["paths"]["cwd"] = ["/project/target"]
        self.assertEqual(self.evaluate(before=processes, after=processes, facts=facts)["state"], "busy")
        facts[10]["paths"]["cwd"] = []
        facts[10]["files"] = [self.file(path="/other/target/.cargo-lock")]
        self.assertEqual(self.evaluate(before=processes, after=processes, facts=facts)["state"], "idle")

    def test_child_compiler_inherits_observed_cargo_lock(self):
        processes = snapshot("cargo")
        processes[11] = dict(processes[10], pid=11, parent=10, comm="rustc")
        facts = facts_for(processes)
        facts[10]["paths"]["cwd"] = []
        facts[10]["files"] = [self.file(path="/other/target/.cargo-lock")]
        self.assertEqual(self.evaluate(before=processes, after=processes, facts=facts)["state"], "idle")
        facts[11]["scopes"]["files"] = "denied"
        self.assertEqual(self.evaluate(before=processes, after=processes, facts=facts)["state"], "unknown")

    def query(self, *, code=0, error="", include_query=True):
        from unittest.mock import MagicMock
        text = "10 1 1000 Fri Oct 23 04:00:00 2026 python\n"
        if include_query:
            text += f"20 {activity.os.getpid()} {activity.os.getuid()} Fri Oct 23 04:00:00 2026 ps\n"
        query = MagicMock(pid=20, returncode=code)
        query.communicate.return_value = (text, error)
        query.__enter__.return_value = query
        return query

    def test_metadata_collector_uses_only_fixed_ps_and_never_claims_idle(self):
        with patch.object(activity.subprocess, "Popen", return_value=self.query()) as run:
            report = activity.collect_metadata("/project", self.paths)
        self.assertEqual(report["state"], "unknown")
        self.assertEqual(run.call_count, 2)
        for call in run.call_args_list:
            self.assertEqual(call.args[0], activity.PS_COMMAND)
            self.assertTrue(call.kwargs["close_fds"])
            self.assertEqual(call.kwargs["env"], {"PATH": "/usr/bin:/bin", "LC_ALL": "C"})
        self.assertIn("process-reference-coverage-unavailable", report["reasons"])
        self.assertEqual(report["process_count"], 1)

    def test_metadata_failure_has_no_fallback_command(self):
        with patch.object(activity.subprocess, "Popen", return_value=self.query(code=1)) as run:
            with self.assertRaises(activity.ObservationError):
                activity.collect_metadata("/project", self.paths)
        self.assertEqual(run.call_count, 1)

    def test_metadata_query_requires_its_own_pid_identity(self):
        with patch.object(activity.subprocess, "Popen", return_value=self.query(include_query=False)):
            with self.assertRaises(activity.ObservationError):
                activity.collect_metadata("/project", self.paths)

    def test_metadata_query_timeout_only_stops_its_own_child(self):
        query = self.query()
        query.communicate.side_effect = [subprocess.TimeoutExpired(activity.PS_COMMAND, 10), ("", "")]
        with patch.object(activity.subprocess, "Popen", return_value=query):
            with self.assertRaises(activity.ObservationError):
                activity.collect_metadata("/project", self.paths)
        query.kill.assert_called_once()
        self.assertEqual(query.communicate.call_count, 2)

    def test_argument_only_cargo_lock_is_not_attribution(self):
        processes = snapshot("cargo")
        facts = facts_for(processes)
        facts[10]["paths"]["command"] = ["/other/target/.cargo-lock"]
        self.assertEqual(self.evaluate(before=processes, after=processes, facts=facts)["state"], "unknown")

    def test_numeric_descriptor_reuse_within_snapshot_is_rejected(self):
        self.facts[10]["files"] = [self.file(), self.file(inode=43)]
        with self.assertRaises(activity.ObservationError):
            self.evaluate()

    def test_original_protocol_validator_is_required_and_rechecked(self):
        with self.assertRaises(activity.ObservationError):
            activity.assert_idle(Path("/project"), [Path("/project/package")])
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            with patch.object(activity, "collect_metadata", return_value={"state": "idle"}) as collect:
                from unittest.mock import Mock
                validator = Mock(return_value={})
                self.assertEqual(activity.assert_idle(root, [root / "new"], protocol_validator=validator), [])
                self.assertEqual(validator.call_count, 2)
                self.assertEqual(collect.call_count, 1)
