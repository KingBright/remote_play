#!/usr/bin/env python3
"""RP observe-only lifecycle check. Delegates to installed helper; never builds or cleans."""
import argparse
import hashlib
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
    if not isinstance(binding, dict):
        raise Refusal("binding must be an observe-only metadata object")
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
    if (not isinstance(report, dict) or report.get("mode") != "observe-only" or report.get("build_ready") is not False
            or report.get("candidates") != [] or report.get("contract_preserved") is not True):
        raise Refusal("installed helper must return a preserved observe-only contract, no candidates")
    observe_only(report["binding"])
    if report["budget"]["status"] != "no-owned-cache-scopes":
        raise Refusal("shared-root budget must remain unknown, not attributed to RP")
    return report


def environment_paths(root, authority=None, binding=None, helper=None):
    """Select one explicit environment contract, or preserve the Mac defaults."""
    supplied = [value is not None for value in (authority, binding, helper)]
    if any(supplied) and not all(supplied):
        raise Refusal("--authority, --binding and --helper must be supplied together")
    if not any(supplied):
        return (root / AUTHORITY, root / BINDING,
                Path.home() / ".codex/skills/rust-project-lifecycle/scripts/rust_storage.py")
    paths = tuple(Path(value) for value in (authority, binding, helper))
    if any(not path.is_absolute() for path in paths):
        raise Refusal("explicit environment paths must be absolute")
    return paths


def canonical_path(path):
    path = Path(path)
    if (not path.is_absolute() or ".." in path.parts
            or any(part.is_symlink() for part in (path, *path.parents))):
        raise Refusal("environment paths must be exact canonical paths without symlinks")
    return path


def metadata_bytes(path, root, label):
    path = canonical_path(path)
    if (not path.is_relative_to(root) or not path.is_file()
            or path.stat().st_size > 1024 ** 2):
        raise Refusal(f"{label} must be a regular project metadata file")
    return path.read_bytes()


def check(action, root, authority, binding_path, helper):
    if action not in {"check", "dry-run"}:
        raise Refusal("only check and dry-run are supported")
    root = root.resolve()
    authority = Path(authority)
    binding_path = Path(binding_path)
    binding = json.loads(metadata_bytes(binding_path, root, "binding"))
    if (not isinstance(binding, dict) or binding.get("schema") != "rust.project_adoption.v1"
            or binding.get("project_root") != str(root)
            or not isinstance(binding.get("authority_inventory"), dict)
            or binding.get("authority_inventory", {}).get("path") != str(authority.resolve())):
        raise Refusal("binding must reference this repository and its existing sole authority")
    authority_bytes = metadata_bytes(authority, root, "authority")
    if binding["authority_inventory"].get("sha256") != hashlib.sha256(authority_bytes).hexdigest():
        raise Refusal("adoption evidence changed; owner must re-assess: authority_inventory")
    inventory = json.loads(authority_bytes)
    if (not isinstance(inventory, dict) or not isinstance(inventory.get("artifacts"), list)
            or inventory.get("project_root", str(root)) != str(root)):
        raise Refusal("authority must describe this project and its registered artifacts")
    observe_only(binding)
    refs = binding.get("selected_artifact_refs")
    if (not isinstance(refs, list) or not refs or any(not isinstance(p, str) or not Path(p).is_absolute() for p in refs)
            or len(refs) != len(set(refs))):
        raise Refusal("binding needs unique exact canonical artifact references")
    for ref in refs:
        target = canonical_path(ref)
        matches = [item for item in inventory["artifacts"] if isinstance(item, dict)
                   and (item.get("canonical_path") or item.get("path")) == ref]
        if len(matches) != 1:
            raise Refusal("missing/ambiguous canonical inventory reference: " + ref)
        if not target.is_dir():
            raise Refusal("selected artifact must be an existing target directory: " + ref)
    checked = invoke(helper, ["adopt-check", "--binding", str(binding_path)])
    if checked["binding"] != binding:
        raise Refusal("helper observation changed the selected environment binding")
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
    parser.add_argument("--authority", type=Path, help="explicit environment authority; requires --binding and --helper")
    parser.add_argument("--binding", type=Path, help="existing adoption binding for this checkout")
    parser.add_argument("--helper", type=Path, help="verified lifecycle helper; no helper or binding is installed or rewritten")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    try:
        authority, binding, helper = environment_paths(root, args.authority, args.binding, args.helper)
        report = check(args.action, root, authority, binding, helper)
        print(json.dumps(report, ensure_ascii=False, indent=2))
        return 0
    except (Refusal, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(json.dumps({"mode": "refused", "error": str(error), "candidates": []}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
