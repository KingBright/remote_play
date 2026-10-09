"""Offline supplied-data checks; fixtures never represent real device acceptance."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_platform_matrix as matrix

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/check_platform_matrix.py"
LIVE_MATRIX = ROOT / "docs/testing/FOUR-PLATFORM-MATRIX.json"


class PlatformMatrixTests(unittest.TestCase):
    def setUp(self):
        self.data = json.loads(LIVE_MATRIX.read_text())

    def passed_direction(self):
        # A complete in-memory fixture tests consistency, not physical truth.
        row = self.data["directions"][0]
        row["result"] = "passed"
        row["evidence"] = ["unit-test supplied-data fixture only"]
        row["first_frame"].update(decoded=True, visible=True)
        row["continuous_frames"]["decoded_count"] = 2
        row["synthetic_source"] = False
        for side in ("producer", "viewer"):
            row[side].update(version="2.0.0-test", artifact_sha256="a" * 64)
        for case in row["core_regressions"].values():
            case["status"] = "passed"
        for feature in ("audio", "input", "reconnect"):
            row[feature]["status"] = "passed"
        return row

    def cli(self, data, optimized, strict=False):
        with tempfile.TemporaryDirectory(prefix="rp-matrix-test-") as directory:
            fixture = Path(directory) / "matrix.json"
            fixture.write_text(json.dumps(data))
            command = [sys.executable, "-B"] + (["-O"] if optimized else []) + [str(SCRIPT), str(fixture)]
            if strict:
                command.append("--require-cross-platform")
            result = subprocess.run(command, capture_output=True, text=True, timeout=15)
        self.assertEqual(result.stderr, "", result.stderr)
        return result.returncode, json.loads(result.stdout)

    def test_existing_matrix_structure_is_valid(self):
        self.assertEqual(len(matrix.validate(self.data)), 12)

    def test_optimized_cli_rejects_untested_rows_marked_passed(self):
        for row in self.data["directions"]:
            if row["kind"] == "cross_platform":
                row["result"] = "passed"
                row["evidence"] = []
        for optimized in (False, True):
            with self.subTest(optimized=optimized):
                code, report = self.cli(self.data, optimized, strict=True)
                self.assertEqual(code, 2)
                self.assertFalse(report["structure_valid"])

    def test_optimized_cli_preserves_pending_acceptance_gate(self):
        for row in self.data["directions"]:
            if row["kind"] == "cross_platform":
                row["result"] = "not_tested"
        for optimized in (False, True):
            with self.subTest(optimized=optimized):
                code, report = self.cli(self.data, optimized, strict=True)
                self.assertEqual(code, 1)
                self.assertTrue(report["structure_valid"])
                self.assertFalse(report["all_cross_platform_accepted"])

    def test_complete_supplied_direction_is_valid(self):
        self.passed_direction()
        self.assertEqual(len(matrix.validate(self.data)), 12)

    def test_passed_direction_requires_hexadecimal_artifact_identity(self):
        row = self.passed_direction()
        for digest in ("z" * 64, "a" * 63, None, 64, ["a" * 64]):
            with self.subTest(digest=digest):
                row["producer"]["artifact_sha256"] = digest
                with self.assertRaises(ValueError):
                    matrix.validate(self.data)

    def test_passed_direction_requires_integral_multiple_frames(self):
        row = self.passed_direction()
        for count in (True, 1, 1.5, float("inf"), "12", None):
            with self.subTest(count=count):
                row["continuous_frames"]["decoded_count"] = count
                with self.assertRaises(ValueError):
                    matrix.validate(self.data)

    def test_incomplete_visible_functional_or_synthetic_evidence_is_rejected(self):
        row = self.passed_direction()
        complete = copy.deepcopy(self.data)
        mutations = [
            lambda r: r["first_frame"].update(visible=False),
            lambda r: r["first_frame"].update(decoded=False),
            lambda r: r["core_regressions"]["single_application_capture"].update(status="unsupported"),
            lambda r: r["audio"].update(status="not_tested"),
            lambda r: r.update(synthetic_source=True),
        ]
        for index, mutate in enumerate(mutations):
            data = copy.deepcopy(complete)
            mutate(data["directions"][0])
            with self.subTest(case=index), self.assertRaises(ValueError):
                matrix.validate(data)

    def test_duplicate_ids_unknown_kinds_or_missing_directions_are_rejected(self):
        cases = []
        duplicate = copy.deepcopy(self.data)
        duplicate["directions"][1]["id"] = duplicate["directions"][0]["id"]
        cases.append(duplicate)
        unknown = copy.deepcopy(self.data)
        unknown["directions"][-1]["kind"] = "unknown"
        cases.append(unknown)
        missing = copy.deepcopy(self.data)
        missing["directions"].pop(0)
        cases.append(missing)
        for data in cases:
            with self.assertRaises(ValueError):
                matrix.validate(data)

    def test_malformed_structure_returns_json_without_traceback(self):
        malformed = copy.deepcopy(self.data)
        malformed["directions"][0]["producer"] = None
        for data in ([], {"directions": None}, {"directions": [None]}, malformed):
            code, report = self.cli(data, optimized=True)
            self.assertEqual(code, 2)
            self.assertFalse(report["structure_valid"])


if __name__ == "__main__":
    unittest.main()
