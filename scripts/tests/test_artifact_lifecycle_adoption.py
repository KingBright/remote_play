"""Narrow existing-project interface fixtures; no build, shared cache or cleanup call."""
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest.mock import patch


class LifecycleAdoptionInterfaceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if os.name != "posix":
            raise unittest.SkipTest("Installed lifecycle helper currently uses POSIX flock")
        helper = Path.home() / ".codex/skills/rust-project-lifecycle/scripts/rust_storage.py"
        if not helper.is_file():
            raise unittest.SkipTest("rust-project-lifecycle helper is not installed")
        cls.helper_path = helper
        spec = importlib.util.spec_from_file_location("rp_lifecycle_fixture_helper", helper)
        cls.helper = importlib.util.module_from_spec(spec)
        previous = sys.dont_write_bytecode
        sys.dont_write_bytecode = True
        try:
            spec.loader.exec_module(cls.helper)
            if getattr(cls.helper, "VERSION", None) != "1.1.0":
                raise unittest.SkipTest("RP observation fixture requires reviewed helper v1.1.0")
            cls.adopt = cls.helper.companion("rust_adopt")
            entry = Path(__file__).resolve().parents[1] / "check_artifact_lifecycle.py"
            entry_spec = importlib.util.spec_from_file_location("rp_lifecycle_entry", entry)
            cls.entry = importlib.util.module_from_spec(entry_spec)
            entry_spec.loader.exec_module(cls.entry)
        finally:
            sys.dont_write_bytecode = previous

    def setUp(self):
        # Only this test's disposable fixtures are removed by TemporaryDirectory.
        # No existing project, cache, deliverable or cleanup executor is touched.
        self.temporary = tempfile.TemporaryDirectory(prefix="rp-lifecycle-interface-")
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.project = self.base / "existing-project"
        self.project.mkdir()
        (self.project / "Cargo.toml").write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
        (self.project / ".cargo").mkdir()
        (self.project / ".cargo/config.toml").write_text('[build]\ntarget-dir="../shared-target"\n')
        (self.base / "shared-target").mkdir()

    def managed_fixture(self):
        state = self.project / ".rust-storage"
        artifacts = self.project / ".rust-artifacts"
        state.mkdir()
        artifacts.mkdir()
        policy = {
            "schema": 1, "project_id": "disposable-rp-fixture",
            "project_root": str(self.project), "artifact_root": str(artifacts),
            "budget_bytes": 1024, "reserve_bytes": 0, "expected_build_bytes": 0,
            "evidence_limit_bytes": 1, "failure_budget_bytes": 1,
            "keep_history": 2, "keep_failed": 1,
        }
        (state / "policy.json").write_text(json.dumps(policy))
        (state / "registry.json").write_text(json.dumps({
            "schema": 1, "project_id": policy["project_id"], "entries": [],
        }))
        (state / "lock").touch()
        (artifacts / ".owner.json").write_text(json.dumps({
            "project_id": policy["project_id"], "project_root": str(self.project),
        }))
        return state, policy

    def test_init_refuses_existing_project_without_overwriting_it(self):
        paths = [self.project / "Cargo.toml", self.project / ".cargo/config.toml"]
        before = [path.read_bytes() for path in paths]
        with self.assertRaisesRegex(self.helper.Refusal, "init only creates a new project"):
            self.helper.init(SimpleNamespace(project=str(self.project)))
        self.assertEqual(before, [path.read_bytes() for path in paths])
        self.assertFalse((self.project / ".rust-artifacts").exists())

    def test_load_accepts_anchored_managed_fixture_without_changing_existing_target(self):
        self.managed_fixture()
        config = self.project / ".cargo/config.toml"
        before = config.read_bytes()
        root, _, registry = self.helper.load(self.project)
        self.assertEqual(root, self.project)
        self.assertEqual(registry["entries"], [])
        self.assertEqual(config.read_bytes(), before)
        self.assertEqual(list((self.base / "shared-target").iterdir()), [])

    def test_load_rejects_external_shared_root_as_project_owned(self):
        state, policy = self.managed_fixture()
        policy["artifact_root"] = str(self.base / "shared-target")
        (state / "policy.json").write_text(json.dumps(policy))
        with self.assertRaisesRegex(self.helper.Refusal, "shared/external roots are not supported"):
            self.helper.load(self.project)
        self.assertEqual(list((self.base / "shared-target").iterdir()), [])

    def observed_fixture(self):
        authority = self.project / "authority.json"
        authority.write_text(json.dumps({"owner": "fixture-project", "artifacts": [{
            "canonical_path": str(self.base / "shared-target"), "kind": "shared_cargo_cache",
            "owner": "multiple-projects", "shared": True, "exclusive_owner": False,
            "generated": True, "delete_now": False,
        }]}))
        state = self.project / ".bindings"
        state.mkdir()
        binding = state / "adoption.json"
        args = SimpleNamespace(
            project=str(self.project), inventory=str(authority), artifact=[str(self.base / "shared-target")],
            local_source_evidence="", budget_gib=None, reserve_gib=None, expected_build_gib=None,
            measure_owned=False, write_binding=str(binding),
        )
        def no_scan(_):
            raise AssertionError("shared roots must never be scanned by the project observation")
        self.adopt.inspect(args, self.helper.safe_absolute, no_scan, no_scan, lambda _: 4 * 1024**3)
        return authority, binding

    def entry_check(self, action, authority, binding):
        return self.entry.check(action, self.project, authority, binding, self.helper_path)

    def test_shared_observe_check_and_dry_run_preserve_configuration_and_have_no_candidates(self):
        before = (self.project / ".cargo/config.toml").read_bytes()
        authority, binding = self.observed_fixture()
        for action in ["check", "dry-run"]:
            report = self.entry_check(action, authority, binding)
            self.assertFalse(report["build_ready"])
            self.assertEqual(report["candidates"], [])
            self.assertEqual(report["binding"]["owned_budget_refs"], [])
            self.assertEqual(report["binding"]["coverage"], self.entry.UNCOVERED)
        self.assertEqual((self.project / ".cargo/config.toml").read_bytes(), before)
        self.assertFalse((self.project / ".rust-artifacts").exists())
        self.assertEqual(list((self.base / "shared-target").iterdir()), [])

    def test_authority_digest_change_is_refused_before_fresh_dry_run(self):
        authority, binding = self.observed_fixture()
        body = json.loads(authority.read_text())
        body["artifacts"][0]["purpose"] = "changed by the inventory owner"
        authority.write_text(json.dumps(body))
        with self.assertRaisesRegex(self.entry.Refusal, "authority_inventory"):
            self.entry_check("dry-run", authority, binding)

    def test_root_cargo_configuration_digest_change_is_refused(self):
        authority, binding = self.observed_fixture()
        config = self.project / ".cargo/config.toml"
        config.write_text(config.read_text() + "# changed configuration\n")
        with self.assertRaisesRegex(self.entry.Refusal, "contract_sha256"):
            self.entry_check("check", authority, binding)

    def test_authority_reference_change_cannot_switch_to_a_second_ledger(self):
        authority, binding = self.observed_fixture()
        second = self.project / "second-authority.json"
        second.write_bytes(authority.read_bytes())
        body = json.loads(binding.read_text())
        body["authority_inventory"]["path"] = str(second)
        binding.write_text(json.dumps(body))
        with self.assertRaisesRegex(self.entry.Refusal, "sole authority"):
            self.entry_check("check", authority, binding)

    def test_changed_selected_reference_is_detected_by_native_authority_lookup(self):
        authority, binding = self.observed_fixture()
        unregistered = self.base / "unregistered-cache"
        unregistered.mkdir()
        body = json.loads(binding.read_text())
        body["selected_artifact_refs"] = [str(unregistered)]
        binding.write_text(json.dumps(body))
        with self.assertRaisesRegex(self.entry.Refusal, "canonical inventory reference"):
            self.entry_check("check", authority, binding)

    def test_coverage_claims_require_exact_false_and_cannot_escalate(self):
        authority, binding = self.observed_fixture()
        original = json.loads(binding.read_text())
        for value in [True, 0]:
            with self.subTest(value=value):
                body = dict(original, coverage=dict(original["coverage"], build_lock=value))
                binding.write_text(json.dumps(body))
                with self.assertRaisesRegex(self.entry.Refusal, "must keep"):
                    self.entry_check("check", authority, binding)

    def test_entry_never_dispatches_build_or_cleanup_actions(self):
        with patch.object(self.entry, "invoke") as backend:
            for action in ["run", "build", "clean", "init", "register"]:
                with self.subTest(action=action), self.assertRaises(self.entry.Refusal):
                    self.entry_check(action, self.project / "unused.json", self.project / "unused-binding.json")
            backend.assert_not_called()

    def test_agents_toolchain_and_member_content_changes_remain_explicitly_uncovered(self):
        manifest = self.project / "Cargo.toml"
        manifest.write_text(manifest.read_text() + '[workspace]\nmembers=["member"]\n')
        member = self.project / "member"
        member.mkdir()
        (member / "Cargo.toml").write_text('[package]\nname="member"\nversion="0.1.0"\n')
        authority, binding = self.observed_fixture()
        (self.project / "AGENTS.md").write_text("Changed manual project contract\n")
        (self.project / "rust-toolchain.toml").write_text('[toolchain]\nchannel="nightly"\n')
        (member / "Cargo.toml").write_text('[package]\nname="member"\nversion="0.2.0"\n')
        report = self.entry_check("check", authority, binding)
        self.assertEqual(report["binding"]["coverage"], self.entry.UNCOVERED)
        self.assertFalse(report["build_ready"])


if __name__ == "__main__":
    unittest.main()
