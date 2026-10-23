import ast
from contextlib import contextmanager
import copy
import hashlib
import importlib.util
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "maintenance"))
import install_linux_release_adapter as install

BASE = """import sys
def compiler_activity(root):
    return None
def assert_idle(root, paths, verified_control_pids=(), *, verified_protocol_locks=()):
    return ["original-mac"]
"""


class LinuxReleaseAdapterInstallTests(unittest.TestCase):
    def patch_baseline(self):
        return patch.object(install, "BASELINE_SHA256", install.sha(BASE.encode()))

    def test_wrong_baseline_never_patched(self):
        with self.assertRaises(RuntimeError):
            install.patch_backend(b"unreviewed")

    def test_only_platform_branch_and_loader_are_added(self):
        with self.patch_baseline():
            changed = install.patch_backend(BASE.encode()).decode()
        original = ast.parse(BASE)
        current = ast.parse(changed)
        old = next(n for n in original.body if isinstance(n, ast.FunctionDef) and n.name == "assert_idle")
        new = next(n for n in current.body if isinstance(n, ast.FunctionDef) and n.name == "assert_idle")
        self.assertEqual(ast.dump(ast.Module(body=old.body, type_ignores=[])),
                         ast.dump(ast.Module(body=new.body[1:], type_ignores=[])))
        self.assertEqual(changed.replace(install.LOADER, "").replace(install.DISPATCH, ""), BASE)

    def test_mac_keeps_original_path_and_linux_selects_new_observer(self):
        with self.patch_baseline():
            namespace = {}
            exec(install.patch_backend(BASE.encode()), namespace)
        namespace["sys"] = SimpleNamespace(platform="darwin")
        namespace["linux_release_activity"] = Mock(side_effect=AssertionError("Linux called on Mac"))
        self.assertEqual(namespace["assert_idle"]("root", ["path"]), ["original-mac"])
        observer = SimpleNamespace(assert_idle=Mock(return_value=["linux"]),
                                   ObservationError=RuntimeError)
        namespace.update(sys=SimpleNamespace(platform="linux"),
                         linux_release_activity=lambda: observer,
                         protocol_lock_snapshots=Mock(), subprocess=SimpleNamespace(SubprocessError=ValueError),
                         Refusal=RuntimeError)
        self.assertEqual(namespace["assert_idle"]("root", ["path"], (1,)), ["linux"])
        self.assertEqual(observer.assert_idle.call_args.kwargs["verified_protocol_locks"], ())

    def test_linux_unknown_and_permission_failure_still_refuse(self):
        with self.patch_baseline():
            namespace = {}
            exec(install.patch_backend(BASE.encode()), namespace)
        observer = SimpleNamespace(assert_idle=Mock(), ObservationError=RuntimeError)
        namespace.update(sys=SimpleNamespace(platform="linux"), linux_release_activity=lambda: observer,
                         protocol_lock_snapshots=Mock(), subprocess=SimpleNamespace(SubprocessError=ValueError),
                         Refusal=RuntimeError)
        for error in (RuntimeError("unknown"), PermissionError("denied")):
            observer.assert_idle.side_effect = error
            with self.subTest(error=error), self.assertRaises(RuntimeError):
                namespace["assert_idle"]("root", ["path"])

    def fixture(self, folder):
        home = Path(folder).resolve() / "home"
        scripts = home / ".codex/skills/rust-project-lifecycle/scripts"
        scripts.mkdir(parents=True)
        helper = scripts / "rust_storage.py"; helper.write_text("# installed original helper\n")
        release = scripts / "rust_release.py"; release.write_text("# fixed release-admit\n")
        baseline = home / "baseline.py"; baseline.write_text(BASE)
        activity = home / "activity.py"; activity.write_text("class ObservationError(RuntimeError):pass\n")
        return home, scripts, baseline, activity

    def pins(self, scripts):
        return patch.multiple(install, BASELINE_SHA256=install.sha(BASE.encode()),
                              HELPER_SHA256=install.sha((scripts/"rust_storage.py").read_bytes()),
                              RELEASE_SHA256=install.sha((scripts/"rust_release.py").read_bytes()))

    def test_plan_is_read_only_and_pins_original_authority(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as tmp:
            home, scripts, baseline, activity = self.fixture(tmp)
            with self.pins(scripts):
                plan, payload = install.plan(home, baseline, activity, install.sha(activity.read_bytes()))
            self.assertFalse((home/".codex/skills/clean-mac-storage").exists())
            self.assertFalse(plan["authority_changed"])
            self.assertFalse(plan["observation_granted"])
            self.assertEqual(len(payload), 2)

    def test_install_changes_only_two_missing_trusted_source_files(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as tmp:
            home, scripts, baseline, activity = self.fixture(tmp)
            old = {p: p.read_bytes() for p in scripts.iterdir()}
            @contextmanager
            def locks(*args):
                yield
            with self.pins(scripts), patch.object(install, "original_locks", locks):
                plan, _ = install.plan(home, baseline, activity, install.sha(activity.read_bytes()))
                result = install.apply(home, baseline, activity, install.sha(activity.read_bytes()),
                                       plan["plan_sha256"], home)
            self.assertTrue(result["installed"])
            self.assertEqual(len(result["applied_paths"]), 2)
            for p, raw in old.items():
                self.assertEqual(p.read_bytes(), raw)
            for p in result["applied_paths"]:
                self.assertEqual(Path(p).stat().st_nlink, 1)
                self.assertEqual(Path(p).stat().st_mode & 0o777, 0o600)

    def test_pinned_activity_update_keeps_backend_helper_release_and_mode(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as tmp:
            home, scripts, baseline, activity = self.fixture(tmp)
            @contextmanager
            def locks(*args):
                yield
            with self.pins(scripts), patch.object(install, "original_locks", locks):
                first, _ = install.plan(home, baseline, activity, install.sha(activity.read_bytes()))
                install.apply(home, baseline, activity, install.sha(activity.read_bytes()),
                              first["plan_sha256"], home)
                target = home/".codex/skills/clean-mac-storage/scripts/linux_release_activity.py"
                backend = target.with_name("managed_release_retention.py")
                preserved = {p: p.read_bytes() for p in (*scripts.iterdir(), backend)}
                old_pin = install.sha(target.read_bytes())
                target.chmod(0o640)
                activity.write_text("class ObservationError(RuntimeError):pass\n# reviewed update\n")
                new_pin = install.sha(activity.read_bytes())
                reviewed, _ = install.plan(home, baseline, activity, new_pin, old_pin)
                self.assertEqual(reviewed["before"][str(target)], "replace-pinned-activity")
                result = install.apply(home, baseline, activity, new_pin,
                                       reviewed["plan_sha256"], home, old_pin)
                self.assertEqual(result["applied_paths"], [str(target)])
                self.assertEqual(target.read_bytes(), activity.read_bytes())
                self.assertEqual(target.stat().st_mode & 0o777, 0o640)
                self.assertFalse(result["observation_granted"])
                again, _ = install.plan(home, baseline, activity, new_pin, old_pin)
                self.assertEqual(again["before"][str(target)], "already-matching")
                for path, raw in preserved.items():
                    self.assertEqual(path.read_bytes(), raw)

    def test_wrong_installed_pin_cannot_replace_unknown_activity(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as tmp:
            home, scripts, baseline, activity = self.fixture(tmp)
            target = home/".codex/skills/clean-mac-storage/scripts/linux_release_activity.py"
            target.parent.mkdir(parents=True)
            target.write_bytes(b"user-source\n")
            with self.pins(scripts), self.assertRaisesRegex(RuntimeError, "pin changed"):
                install.plan(home, baseline, activity, install.sha(activity.read_bytes()), "0"*64)
            self.assertEqual(target.read_bytes(), b"user-source\n")

    def test_activity_update_never_overwrites_unknown_pending_or_drift(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as tmp:
            target = Path(tmp).resolve()/"activity.py"; target.write_bytes(b"old")
            pending = target.with_name("." + target.name + ".linux-install-pending")
            pending.write_bytes(b"other-transaction")
            with self.assertRaises(FileExistsError):
                install._replace_activity(target, b"new", install.sha(b"old"))
            self.assertEqual(target.read_bytes(), b"old")
            self.assertEqual(pending.read_bytes(), b"other-transaction")
            with self.assertRaisesRegex(RuntimeError, "pin changed"):
                install._replace_activity(target, b"new", install.sha(b"stale"))
            self.assertEqual(target.read_bytes(), b"old")

    def test_unknown_existing_adapter_or_changed_source_is_preserved(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as tmp:
            home, scripts, baseline, activity = self.fixture(tmp)
            target = home/".codex/skills/clean-mac-storage/scripts/managed_release_retention.py"
            target.parent.mkdir(parents=True); target.write_text("user-content\n")
            with self.pins(scripts), self.assertRaises(RuntimeError):
                install.plan(home, baseline, activity, install.sha(activity.read_bytes()))
            self.assertEqual(target.read_text(), "user-content\n")

    def test_no_replace_install_race_preserves_existing_file(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as tmp:
            target = Path(tmp).resolve()/"target.py";target.write_bytes(b"other-writer")
            with self.assertRaises(FileExistsError):
                install._write_new(target, b"ours")
            self.assertEqual(target.read_bytes(), b"other-writer")

    def test_symlink_and_writable_source_rejected(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as tmp:
            root = Path(tmp).resolve();source = root/"source";source.write_text("x")
            alias = root/"alias";alias.symlink_to(source)
            with self.assertRaises(RuntimeError):
                install.ordinary(alias)
            source.chmod(0o666)
            with self.assertRaises(RuntimeError):
                install.ordinary(source)

    def test_changed_plan_is_refused_before_install(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as tmp:
            home, scripts, baseline, activity = self.fixture(tmp)
            with self.pins(scripts), self.assertRaisesRegex(RuntimeError, "reviewed installation plan changed"):
                install.apply(home, baseline, activity, install.sha(activity.read_bytes()), "0"*64, home)
            self.assertFalse((home/".codex/skills/clean-mac-storage").exists())

    def test_writable_ancestor_remains_rejected(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as tmp:
            root = Path(tmp).resolve()
            parent = root/"shared"
            parent.mkdir(mode=0o700)
            source = parent/"source.py"
            source.write_bytes(b"reviewed")
            parent.chmod(0o777)
            with self.assertRaisesRegex(RuntimeError, "unsafe source/helper owner or mode"):
                install.ordinary(source)

    def test_actual_pinned_backend_changes_only_linux_dispatch(self):
        baseline = Path(__file__).resolve().parents[1] / "maintenance/release_backend/managed_release_retention.py"
        raw = baseline.read_bytes()
        self.assertEqual(install.sha(raw), install.BASELINE_SHA256)
        changed = install.patch_backend(raw).decode()
        self.assertEqual(changed.replace(install.LOADER, "").replace(install.DISPATCH, ""), raw.decode())
        original = ast.parse(raw)
        modified = ast.parse(changed)
        original_functions = {n.name: n for n in original.body if isinstance(n, ast.FunctionDef)}
        changed_functions = {n.name: n for n in modified.body if isinstance(n, ast.FunctionDef)}
        self.assertEqual(set(changed_functions) - set(original_functions), {"linux_release_activity"})
        for name, node in original_functions.items():
            candidate = copy.deepcopy(changed_functions[name])
            if name == "assert_idle":
                candidate.body = candidate.body[1:]
            self.assertEqual(ast.dump(node), ast.dump(candidate), name)
