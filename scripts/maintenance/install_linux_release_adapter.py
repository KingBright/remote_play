#!/usr/bin/env python3
"""Install Linux activity dispatch into the original trusted release backend.

The pinned original backend is supplied as a verified source dependency. This
keeps its policies intact; only assert_idle gets Linux platform dispatch.
No registry, release, cache, permissions, or installed application is changed.
"""
import argparse
import contextlib
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys

BASELINE_SHA256 = "175566a4e71592c8b6a37fbbac7ed0f75d8030207f608ec6681e730f6b876aa4"
HELPER_SHA256 = "367450354cc0ce468e48949aa26da85daa0ffbe6fb81f991a8969d90c32c7c35"
RELEASE_SHA256 = "b5177fa40721ae1baa28a2ce52546ac5bb99702c5b3b8e2d118f4c692e1e0134"
HEADER = "def assert_idle(root, paths, verified_control_pids=(), *, verified_protocol_locks=()):\n"
DISPATCH = '''    if sys.platform.startswith("linux"):
        observer = linux_release_activity()
        try:
            return observer.assert_idle(root, paths, verified_control_pids,
                verified_protocol_locks=verified_protocol_locks,
                protocol_validator=protocol_lock_snapshots)
        except (observer.ObservationError, OSError, ValueError, subprocess.SubprocessError) as error:
            raise Refusal(str(error)) from error
'''
LOADER = '''def linux_release_activity():
    path = trusted_file(Path(__file__).absolute().with_name("linux_release_activity.py"))
    spec = importlib.util.spec_from_file_location("managed_linux_release_activity", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


'''


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def ordinary(path, *, required=True):
    path = Path(path).absolute()
    for part in (path, *path.parents):
        if part.is_symlink():
            raise RuntimeError("linked source/helper path rejected")
        if part.exists():
            info = part.stat()
            if info.st_uid != os.getuid() or info.st_mode & 0o022:
                # System-owned / and /Users or /var may contain the user's tree.
                if (part != path and info.st_uid == 0 and stat.S_ISDIR(info.st_mode)
                        and not info.st_mode & 0o022):
                    continue
                raise RuntimeError("unsafe source/helper owner or mode")
    if required:
        info = path.stat()
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise RuntimeError("regular owned source/helper required")
    return path


def pinned(path, expected):
    path = ordinary(path)
    raw = path.read_bytes()
    if sha(raw) != expected:
        raise RuntimeError("source/helper pin changed")
    return raw


def patch_backend(raw):
    if sha(raw) != BASELINE_SHA256:
        raise RuntimeError("original backend differs from reviewed source")
    text = raw.decode()
    anchor = "def compiler_activity(root):\n"
    if text.count(HEADER) != 1 or text.count(anchor) != 1:
        raise RuntimeError("original backend entry is ambiguous")
    changed = text.replace(anchor, LOADER + anchor, 1).replace(HEADER, HEADER + DISPATCH, 1)
    compile(changed, "managed_release_retention.py", "exec")
    return changed.encode()


def plan(home, baseline, activity, activity_sha):
    home = Path(home).resolve(strict=True)
    scripts = home / ".codex/skills/rust-project-lifecycle/scripts"
    helper = scripts / "rust_storage.py"
    release = scripts / "rust_release.py"
    pinned(helper, HELPER_SHA256); pinned(release, RELEASE_SHA256)
    baseline_raw = pinned(baseline, BASELINE_SHA256)
    activity_raw = pinned(activity, activity_sha)
    compile(activity_raw, "linux_release_activity.py", "exec")
    backend = home / ".codex/skills/clean-mac-storage/scripts/managed_release_retention.py"
    companion = backend.with_name("linux_release_activity.py")
    payload = {companion: activity_raw, backend: patch_backend(baseline_raw)}
    state = {}
    for target, raw in payload.items():
        ordinary(target, required=False)
        if target.exists():
            if pinned(target, sha(raw)) != raw:
                raise RuntimeError("existing trusted adapter differs")
            state[str(target)] = "already-matching"
        else:
            state[str(target)] = "absent"
    body = {"schema": 1, "kind": "original-linux-release-adapter-install",
            "helper": str(helper), "helper_sha256": HELPER_SHA256,
            "release_module_sha256": RELEASE_SHA256,
            "original_backend_sha256": BASELINE_SHA256,
            "files": {str(p): sha(raw) for p, raw in payload.items()},
            "before": state, "authority_changed": False, "storage_cleaned": False,
            "application_changed": False, "observation_granted": False}
    body["plan_sha256"] = sha(json.dumps(body, sort_keys=True, separators=(",", ":")).encode())
    return body, payload


def _write_new(path, raw):
    # Source installation only. Exclusive temporary ownership and a no-replace
    # link make the complete file visible atomically. No old file is removed.
    pending = path.with_name("." + path.name + ".linux-install-pending")
    fd = os.open(pending, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(raw); stream.flush(); os.fsync(stream.fileno())
    if sha(pending.read_bytes()) != sha(raw):
        raise RuntimeError("installation staging changed; preserving it")
    os.link(pending, path, follow_symlinks=False)
    if pending.stat().st_ino != path.stat().st_ino:
        raise RuntimeError("installation identity changed; preserving staging")
    pending.unlink()  # Only this exact source file's verified installation link.


@contextlib.contextmanager
def original_locks(repo, helper_path):
    spec = importlib.util.spec_from_file_location("linux_install_original_storage", helper_path)
    helper = importlib.util.module_from_spec(spec); spec.loader.exec_module(helper)
    lock = ordinary(Path(repo) / ".rust-storage/release-producer.lock")
    fd = os.open(lock, os.O_RDWR | os.O_NOFOLLOW)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with helper.cleanup_lock(True):
            yield
    finally:
        os.close(fd)


def apply(home, baseline, activity, activity_sha, expected_plan, repo):
    before, _ = plan(home, baseline, activity, activity_sha)
    if before["plan_sha256"] != expected_plan:
        raise RuntimeError("reviewed installation plan changed")
    with original_locks(repo, before["helper"]):
        current, payload = plan(home, baseline, activity, activity_sha)
        if current != before:
            raise RuntimeError("installation state changed under original locks")
        for path, raw in payload.items():
            if path.exists():
                continue
            path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
            ordinary(path.parent, required=False)
            _write_new(path, raw)
        for path, raw in payload.items():
            pinned(path, sha(raw))
    return dict(before, installed=True, applied_paths=[p for p, value in before["before"].items()
                                                     if value == "absent"],
                release_or_application_started=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("--activity-sha256", required=True)
    parser.add_argument("--expected-plan-sha256")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    if not sys.platform.startswith("linux"):
        raise RuntimeError("installation is native Linux only")
    repo = Path(__file__).resolve().parents[2]
    activity = Path(__file__).with_name("linux_release_activity.py")
    result, _ = plan(Path.home(), args.baseline, activity, args.activity_sha256)
    if args.apply:
        if not args.expected_plan_sha256:
            raise RuntimeError("independent reviewed plan pin required")
        result = apply(Path.home(), args.baseline, activity, args.activity_sha256,
                       args.expected_plan_sha256, repo)
    print(json.dumps(result, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
