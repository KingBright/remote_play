#!/usr/bin/env python3
"""RP observe-only lifecycle check. Delegates to installed helper; never builds or cleans."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

AUTHORITY = "docs/plans/ARTIFACT-LIFECYCLE-ADOPTION.json"
BINDING = ".rust-storage/adoption.json"
UNCOVERED = {"build_lock": False, "closure": False, "exact_apply": False, "ide": False}


class Refusal(Exception):
    pass


def observe_only(binding):
    coverage = binding.get("coverage")
    if (not isinstance(coverage, dict) or set(coverage) != set(UNCOVERED)
            or any(value is not False for value in coverage.values())):
        raise Refusal("binding must keep build_lock/closure/exact_apply/ide false")
    if binding.get("owned_budget_refs") != [] or binding.get("budgets") is not None:
        raise Refusal("RP shared-target binding cannot claim owned budget scopes")


def invoke(helper, arguments):
    command = [sys.executable, "-B", str(helper), *arguments]
    run = subprocess.run(command, capture_output=True, text=True, timeout=30)
    if run.returncode:
        raise Refusal(run.stderr.strip()[:8000] or "installed helper refused observation")
    try:
        report = json.loads(run.stdout)
    except ValueError as error:
        raise Refusal("installed helper did not return JSON") from error
    if (report.get("mode") != "observe-only" or report.get("build_ready") is not False
            or report.get("candidates") != [] or report.get("contract_preserved") is not True):
        raise Refusal("installed helper must return a preserved observe-only contract, no candidates")
    observe_only(report["binding"])
    if report["budget"]["status"] != "no-owned-cache-scopes":
        raise Refusal("shared-root budget must remain unknown, not attributed to RP")
    return report


def check(action, root, authority, binding_path, helper):
    if action not in {"check", "dry-run"}:
        raise Refusal("only check and dry-run are supported")
    root = root.resolve()
    if (binding_path.is_symlink() or binding_path.parent.is_symlink()
            or not binding_path.resolve().is_relative_to(root)):
        raise Refusal("binding must be a regular project metadata file")
    binding = json.loads(binding_path.read_text())
    if (binding.get("schema") != "rust.project_adoption.v1"
            or binding.get("project_root") != str(root)
            or binding.get("authority_inventory", {}).get("path") != str(authority.resolve())):
        raise Refusal("binding must reference this repository and its existing sole authority")
    observe_only(binding)
    refs = binding.get("selected_artifact_refs")
    if (not isinstance(refs, list) or not refs or any(not isinstance(p, str) or not Path(p).is_absolute() for p in refs)
            or len(refs) != len(set(refs))):
        raise Refusal("binding needs unique exact canonical artifact references")
    checked = invoke(helper, ["adopt-check", "--binding", str(binding_path)])
    if action == "check":
        return checked
    arguments = ["adopt", "--project", str(root), "--inventory", str(authority), "--dry-run"]
    for ref in refs:
        arguments.extend(["--artifact", ref])
    evidence = binding["source_storage"].get("evidence")
    if evidence:
        arguments.extend(["--local-source-evidence", evidence])
    report = invoke(helper, arguments)
    if report["binding"] != checked["binding"]:
        raise Refusal("evidence changed between check and dry-run; owner must reassess")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", nargs="?", choices=["check", "dry-run"], default="check")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    helper = Path.home() / ".codex/skills/rust-project-lifecycle/scripts/rust_storage.py"
    try:
        report = check(args.action, root, root / AUTHORITY, root / BINDING, helper)
        print(json.dumps(report, ensure_ascii=False, indent=2))
        return 0
    except (Refusal, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(json.dumps({"mode": "refused", "error": str(error), "candidates": []}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
