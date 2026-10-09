#!/usr/bin/env python3
"""Check supplied evidence coverage, never authorize a release or verify a display."""
import argparse
import itertools
import json
from pathlib import Path
import re

PLATFORMS = {"android", "macos_silicon", "windows", "linux"}
STATES = {"not_tested", "passed", "partial", "failed", "unsupported"}
FIELDS = {"producer", "viewer", "transport", "capture_source", "encode", "decode",
          "first_frame", "continuous_frames", "audio", "input", "reconnect", "result",
          "evidence", "blockers", "core_regressions"}
CASES = {"enumeration_identity", "single_application_capture", "multi_window_push",
         "multi_window_pull", "concurrent_isolation", "move_resize_close", "rapid_switch",
         "disconnect_duplicate_connect", "resource_release"}
KINDS = {"cross_platform", "same_platform", "loopback"}


def require(condition, message):
    # These are report checks, not debug assertions: -O/PYTHONOPTIMIZE must not
    # turn missing physical evidence into complete supplied-data acceptance.
    if not condition:
        raise ValueError(message)


def validate(data):
    require(isinstance(data, dict), "matrix must be an object")
    rows = data["directions"]
    require(isinstance(rows, list), "directions must be a list")
    ids = set()
    for row in rows:
        require(isinstance(row, dict), "direction must be an object")
        require(FIELDS | {"id", "kind", "from", "to"} <= row.keys(), "direction missing fields")
        require(isinstance(row["id"], str) and row["id"] and row["id"] not in ids, "direction ids must be unique nonempty strings")
        ids.add(row["id"])
        require(row["kind"] in KINDS, (row["id"], "unknown direction kind"))
        require(row["from"] in PLATFORMS and row["to"] in PLATFORMS, (row["id"], "unknown platform"))
    cross = [row for row in rows if row["kind"] == "cross_platform"]
    pairs = [(row["from"], row["to"]) for row in cross]
    require(len(pairs) == 12 and set(pairs) == set(itertools.permutations(PLATFORMS, 2)), "12 unique directed platform pairs required")
    same = [row for row in rows if row["kind"] == "same_platform"]
    require(len(same) == 4 and {(row["from"], row["to"]) for row in same} == {(p, p) for p in PLATFORMS}, "four unique same-platform directions required")
    require(any(row["kind"] == "loopback" for row in rows), "loopback must be separate")
    for row in rows:
        require(row["result"] in STATES, (row["id"], "unknown result"))
        require(isinstance(row["core_regressions"], dict) and CASES == row["core_regressions"].keys(), (row["id"], "core regression coverage missing"))
        require(all(isinstance(case, dict) and case.get("status") in STATES and "notes" in case for case in row["core_regressions"].values()), (row["id"], "invalid core regression case"))
        for side in ("producer", "viewer"):
            require(isinstance(row[side], dict) and {"device", "version", "artifact_sha256", "renderer"} <= row[side].keys(), (row["id"], side, "missing identity fields"))
        if row["result"] in {"passed", "partial", "failed"}:
            require(row["evidence"], (row["id"], "executed result needs evidence"))
        if row["result"] == "passed":
            require(row["first_frame"]["decoded"] is True and row["first_frame"]["visible"] is True, (row["id"], "decoded and visible first frame required"))
            count = row["continuous_frames"]["decoded_count"]
            require(type(count) is int and count > 1, (row["id"], "multiple decoded frames require an integer count"))
            require(all(case["status"] == "passed" for case in row["core_regressions"].values()), "unsupported features cannot count as complete functional acceptance")
            require(all(row[feature]["status"] == "passed" for feature in ("audio", "input", "reconnect")), (row["id"], "audio/input/reconnect coverage required"))
            for side in ("producer", "viewer"):
                digest = row[side]["artifact_sha256"]
                require(isinstance(row[side]["version"], str) and row[side]["version"], (row["id"], side, "version required"))
                require(isinstance(digest, str) and re.fullmatch(r"[0-9a-fA-F]{64}", digest), (row["id"], side, "SHA-256 must be 64 hexadecimal digits"))
            require(not row.get("synthetic_source", False), "synthetic fixture is not full platform acceptance")
    return cross


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("matrix", nargs="?", type=Path, default=Path(__file__).resolve().parents[1] / "docs/testing/FOUR-PLATFORM-MATRIX.json")
    parser.add_argument("--require-cross-platform", action="store_true")
    args = parser.parse_args()
    try:
        data = json.loads(args.matrix.read_text())
        cross = validate(data)
    except (ValueError, KeyError, TypeError, OSError) as error:
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
