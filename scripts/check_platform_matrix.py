#!/usr/bin/env python3
"""Check supplied evidence coverage, never authorize a release or verify a display."""
import argparse
import itertools
import json
from pathlib import Path

PLATFORMS = {"android", "macos_silicon", "windows", "linux"}
STATES = {"not_tested", "passed", "partial", "failed", "unsupported"}
FIELDS = {"producer", "viewer", "transport", "capture_source", "encode", "decode",
          "first_frame", "continuous_frames", "audio", "input", "reconnect", "result",
          "evidence", "blockers", "core_regressions"}
CASES = {"enumeration_identity", "single_application_capture", "multi_window_push",
         "multi_window_pull", "concurrent_isolation", "move_resize_close", "rapid_switch",
         "disconnect_duplicate_connect", "resource_release"}


def validate(data):
    rows = data["directions"]
    cross = [row for row in rows if row["kind"] == "cross_platform"]
    pairs = [(row["from"], row["to"]) for row in cross]
    assert len(pairs) == 12 and set(pairs) == set(itertools.permutations(PLATFORMS, 2)), "12 unique directed platform pairs required"
    same = [row for row in rows if row["kind"] == "same_platform"]
    assert len(same) == 4 and {(row["from"], row["to"]) for row in same} == {(p, p) for p in PLATFORMS}
    assert any(row["kind"] == "loopback" for row in rows), "loopback must be separate"
    for row in rows:
        assert row["from"] in PLATFORMS and row["to"] in PLATFORMS
        assert FIELDS <= row.keys(), (row["id"], "missing fields")
        assert row["result"] in STATES
        assert CASES == row["core_regressions"].keys()
        assert all(case["status"] in STATES and "notes" in case for case in row["core_regressions"].values())
        for side in ("producer", "viewer"):
            assert {"device", "version", "artifact_sha256", "renderer"} <= row[side].keys()
        if row["result"] in {"passed", "partial", "failed"}:
            assert row["evidence"], (row["id"], "executed result needs evidence")
        if row["result"] == "passed":
            assert row["first_frame"]["decoded"] is True and row["first_frame"]["visible"] is True
            assert row["continuous_frames"]["decoded_count"] > 1
            assert all(case["status"] == "passed" for case in row["core_regressions"].values()), "unsupported features cannot count as complete functional acceptance"
            assert all(row[feature]["status"] == "passed" for feature in ("audio", "input", "reconnect"))
            assert all(row[side]["version"] and len(row[side]["artifact_sha256"] or "") == 64 for side in ("producer", "viewer"))
            assert not row.get("synthetic_source", False), "synthetic fixture is not full platform acceptance"
    return cross


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("matrix", nargs="?", type=Path, default=Path(__file__).resolve().parents[1] / "docs/testing/FOUR-PLATFORM-MATRIX.json")
    parser.add_argument("--require-cross-platform", action="store_true")
    args = parser.parse_args()
    try:
        data = json.loads(args.matrix.read_text())
        cross = validate(data)
    except (ValueError, KeyError, AssertionError) as error:
        print(json.dumps({"structure_valid": False, "error": str(error)}))
        return 2
    counts = {state: sum(row["result"] == state for row in cross) for state in sorted(STATES)}
    accepted = all(row["result"] == "passed" for row in cross)
    print(json.dumps({"structure_valid": True, "cross_platform_directions": len(cross),
                      "cross_platform_results": counts, "all_cross_platform_accepted": accepted,
                      "scope": "supplied data consistency only; no independent verification/release authorization"}))
    return 1 if args.require_cross_platform and not accepted else 0


if __name__ == "__main__":
    raise SystemExit(main())
