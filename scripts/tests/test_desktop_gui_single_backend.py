from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "app"


class SingleDesktopGuiStaticTests(unittest.TestCase):
    def test_app_manifest_has_no_egui_runtime_dependency(self):
        manifest = (APP / "Cargo.toml").read_text()
        self.assertNotRegex(manifest, r"(?mi)^\s*eframe\s*=")
        self.assertNotRegex(manifest, r"(?mi)^\s*egui(?:[-_][a-z0-9_-]+)?\s*=")

    def test_root_lock_has_no_egui_packages(self):
        lock = (ROOT / "Cargo.lock").read_text()
        self.assertNotRegex(
            lock,
            r'(?m)^name = "(?:eframe|egui|egui-winit|egui_glow)"$',
        )

    def test_app_sources_have_no_egui_widget_implementation(self):
        sources = list((APP / "src").rglob("*.rs"))
        matches = [
            str(path.relative_to(ROOT))
            for path in sources
            if re.search(r"\b(?:eframe|egui)::", path.read_text())
        ]
        self.assertEqual(matches, [])

    def test_entrypoint_has_no_egui_backend_switch(self):
        main = (APP / "src" / "main.rs").read_text()
        self.assertNotIn("REMOTE_PLAY_GUI_BACKEND", main)
        self.assertNotIn("egui-diagnostic", main)

    def test_vendored_capture_crate_has_no_egui_build_entry(self):
        vendor = ROOT / "third_party" / "screencapturekit"
        if not vendor.is_dir():
            self.skipTest("This checkout does not vendor screencapturekit")
        manifest = (vendor / "Cargo.toml").read_text()
        lock = (vendor / "Cargo.lock").read_text()
        self.assertNotRegex(manifest, r"(?mi)^\[dev-dependencies\.eframe\]")
        self.assertNotRegex(
            lock,
            r'(?m)^name = "(?:eframe|egui|egui-wgpu|egui-winit|egui_glow)"$',
        )
        self.assertFalse((vendor / "examples" / "20_egui_viewer.rs").exists())
        sources = [*vendor.rglob("*.rs")]
        matches = [
            str(path.relative_to(ROOT))
            for path in sources
            if re.search(r"\b(?:eframe|egui)::", path.read_text())
        ]
        self.assertEqual(matches, [])

    def test_retired_egui_drafts_are_not_in_active_source_tree(self):
        for relative in (
            "app/src/desktop/fonts.rs",
            "app/src/desktop/input_path_tests.rs",
            "scripts/acceptance/pending/desktop_device_switch.rs",
        ):
            self.assertFalse((ROOT / relative).exists(), relative)


if __name__ == "__main__":
    unittest.main()
