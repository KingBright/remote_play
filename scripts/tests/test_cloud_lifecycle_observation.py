"""Environment-specific observation fixtures. No Cargo, real cache or cleanup.

Set WEAVELO_LIFECYCLE_TEST_HELPER to the verified official 2.0.2 helper when it is not
installed at the usual location. The production checker has no env override.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

from scripts import check_artifact_lifecycle as entry


class CloudLifecycleSelectionTests(unittest.TestCase):
    def test_default_environment_paths_preserve_existing_mac_entry(self):
        root = Path("/fixture/project")
        self.assertEqual(entry.environment_paths(root), (
            root / entry.AUTHORITY, root / entry.BINDING,
            Path.home() / ".codex/skills/rust-project-lifecycle/scripts/rust_storage.py",
        ))

    def test_explicit_paths_require_all_three_selectors(self):
        for values in [("/a", None, None), (None, "/b", None),
                       (None, None, "/h"), ("/a", "/b", None),
                       ("/a", None, "/h"), (None, "/b", "/h")]:
            with self.subTest(values=values), self.assertRaisesRegex(entry.Refusal, "together"):
                entry.environment_paths(Path("/fixture"), *values)
        self.assertEqual(entry.environment_paths(Path("/fixture"), "/a", "/b", "/h"),
                         (Path("/a"), Path("/b"), Path("/h")))

    def test_explicit_relative_paths_do_not_change_environment(self):
        with self.assertRaisesRegex(entry.Refusal, "absolute"):
            entry.environment_paths(Path("/fixture"), "authority.json", "/b", "/h")


class CloudLifecycleObservationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if os.name != "posix":
            raise unittest.SkipTest("Official lifecycle helper currently requires POSIX")
        supplied = os.environ.get("WEAVELO_LIFECYCLE_TEST_HELPER")
        cls.helper = Path(supplied) if supplied else (
            Path.home() / ".codex/skills/rust-project-lifecycle/scripts/rust_storage.py")
        if not cls.helper.is_file():
            if supplied:
                raise RuntimeError("Explicit lifecycle test helper is unavailable")
            raise unittest.SkipTest("Set WEAVELO_LIFECYCLE_TEST_HELPER to the verified official 2.0.2 helper")
        spec = importlib.util.spec_from_file_location("rp_cloud_lifecycle_test_helper", cls.helper)
        module = importlib.util.module_from_spec(spec)
        previous = sys.dont_write_bytecode
        sys.dont_write_bytecode = True
        try:
            spec.loader.exec_module(module)
        finally:
            sys.dont_write_bytecode = previous
        if module.VERSION != "2.0.2":
            if supplied:
                raise RuntimeError("Cloud observation fixtures require reviewed helper 2.0.2")
            raise unittest.SkipTest("Installed helper is not the reviewed 2.0.2")

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="rp-cloud-observation-")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name).resolve()
        self.root = self.base / "project"
        self.root.mkdir()
        (self.root / "Cargo.toml").write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
        (self.root / ".cargo").mkdir()
        (self.root / ".cargo/config.toml").write_text('[build]\ntarget-dir="../existing-target"\n')
        self.target = self.base / "existing-target"
        self.target.mkdir()
        self.authority = self.root / "cloud-authority.json"
        self.authority.write_text(json.dumps({
            "owner": "fixture", "project_root": str(self.root), "artifacts": [{
                "canonical_path": str(self.target), "kind": "shared_cargo_cache",
                "shared": True, "exclusive_owner": False, "generated": True,
                "owner": "fixture-shared", "delete_now": False,
            }],
        }))
        self.binding = self.root / "cloud-adoption.json"
        result = subprocess.run([
            sys.executable, "-B", str(self.helper), "adopt", "--project", str(self.root),
            "--inventory", str(self.authority), "--artifact", str(self.target),
            "--write-binding", str(self.binding),
        ], capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.original_binding = self.binding.read_bytes()
        self.original_authority = self.authority.read_bytes()

    def check(self, action="check"):
        return entry.check(action, self.root, self.authority, self.binding, self.helper)

    def change_binding(self, change):
        body = json.loads(self.original_binding)
        change(body)
        self.binding.write_text(json.dumps(body))

    def test_official_adopt_check_and_dry_run_preserve_contract_and_claim_no_run(self):
        before = (self.root / ".cargo/config.toml").read_bytes()
        for action in ("check", "dry-run"):
            report = self.check(action)
            self.assertEqual(report["mode"], "observe-only")
            self.assertIs(report["build_ready"], False)
            self.assertEqual(report["binding"]["coverage"], entry.UNCOVERED)
            self.assertEqual(report["binding"]["selected_artifact_refs"], [str(self.target)])
            self.assertEqual(report["candidates"], [])
            self.assertEqual(report["budget"]["status"], "no-owned-cache-scopes")
        self.assertEqual(self.binding.read_bytes(), self.original_binding)
        self.assertEqual(self.authority.read_bytes(), self.original_authority)
        self.assertEqual((self.root / ".cargo/config.toml").read_bytes(), before)
        self.assertEqual(list(self.target.iterdir()), [])
        self.assertFalse((self.root / ".rust-artifacts").exists())

    def test_wrong_project_and_authority_reject_before_helper(self):
        changes = [lambda b: b.update(project_root=str(self.base / "other")),
                   lambda b: b["authority_inventory"].update(path=str(self.root / "other.json"))]
        for change in changes:
            self.change_binding(change)
            with patch.object(entry, "invoke") as invoke, self.assertRaisesRegex(entry.Refusal, "sole authority"):
                self.check()
            invoke.assert_not_called()

    def test_whole_authority_digest_drift_requires_owner_reassessment(self):
        self.authority.write_bytes(self.original_authority + b"\n")
        with self.assertRaisesRegex(entry.Refusal, "authority_inventory"):
            self.check()

    def test_root_cargo_contract_drift_is_checked_by_official_helper(self):
        config = self.root / ".cargo/config.toml"
        config.write_text(config.read_text() + "# changed contract\n")
        with self.assertRaisesRegex(entry.Refusal, "contract_sha256"):
            self.check()

    def test_coverage_cannot_claim_implemented_gates(self):
        for value in (True, 0):
            self.change_binding(lambda b: b["coverage"].update(build_lock=value))
            with self.assertRaisesRegex(entry.Refusal, "must keep"):
                self.check()

    def test_helper_cannot_upgrade_observation_to_build_or_cleanup_authority(self):
        original = self.check()
        changes = [lambda r: r.update(build_ready=True),
                   lambda r: r.update(candidates=[{"path": "invented"}]),
                   lambda r: r["binding"]["coverage"].update(closure=True)]
        for change in changes:
            report = json.loads(json.dumps(original))
            change(report)
            response = SimpleNamespace(returncode=0, stdout=json.dumps(report), stderr="")
            with patch.object(entry.subprocess, "run", return_value=response):
                with self.assertRaises(entry.Refusal):
                    entry.invoke(self.helper, ["adopt-check", "--binding", str(self.binding)])

    def test_helper_must_return_the_same_verified_binding(self):
        report = self.check()
        report["binding"]["selected_artifact_refs"] = [str(self.base / "different")]
        with patch.object(entry, "invoke", return_value=report):
            with self.assertRaisesRegex(entry.Refusal, "changed the selected environment"):
                self.check()

    def test_missing_target_is_not_a_valid_build_observation(self):
        self.target.rmdir()  # This test's empty, disposable directory only.
        with self.assertRaisesRegex(entry.Refusal, "existing target directory"):
            self.check()

    def test_existing_but_unregistered_target_is_rejected(self):
        other = self.base / "unregistered-target"
        other.mkdir()
        self.change_binding(lambda b: b.update(selected_artifact_refs=[str(other)]))
        with self.assertRaisesRegex(entry.Refusal, "canonical inventory reference"):
            self.check()

    def test_authority_for_another_project_is_rejected_even_with_matching_digest(self):
        body = json.loads(self.original_authority)
        body["project_root"] = str(self.base / "other")
        self.authority.write_text(json.dumps(body))
        self.change_binding(lambda b: b["authority_inventory"].update(
            sha256=hashlib.sha256(self.authority.read_bytes()).hexdigest()))
        with self.assertRaisesRegex(entry.Refusal, "describe this project"):
            self.check()

    def test_symlinked_metadata_does_not_select_another_contract(self):
        alias = self.root / "binding-alias.json"
        alias.symlink_to(self.binding)
        with self.assertRaisesRegex(entry.Refusal, "symlinks"):
            entry.check("check", self.root, self.authority, alias, self.helper)


if __name__ == "__main__":
    unittest.main()
