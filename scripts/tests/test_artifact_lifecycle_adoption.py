"""Narrow existing-project interface fixtures; no build, shared cache or cleanup call."""
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace
import sys
import tempfile
import unittest


class LifecycleAdoptionInterfaceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if os.name != "posix":
            raise unittest.SkipTest("Installed lifecycle helper currently uses POSIX flock")
        helper = Path.home() / ".codex/skills/rust-project-lifecycle/scripts/rust_storage.py"
        if not helper.is_file():
            raise unittest.SkipTest("rust-project-lifecycle helper is not installed")
        spec = importlib.util.spec_from_file_location("rp_lifecycle_fixture_helper", helper)
        cls.helper = importlib.util.module_from_spec(spec)
        previous = sys.dont_write_bytecode
        sys.dont_write_bytecode = True
        try:
            spec.loader.exec_module(cls.helper)
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


if __name__ == "__main__":
    unittest.main()
