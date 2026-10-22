#!/usr/bin/env python3
"""Build the isolated NAS relay in primary main; never deploy or touch profiles."""
from pathlib import Path
import hashlib
import json
import os
import re
import shutil
import signal
import struct
import subprocess
import sys
import time

import release_artifact_lifecycle as release
import test_stream_settings_model as guarded

ROOT = Path(__file__).resolve().parents[1]
TARGET = Path.home().resolve() / "rust-target"
TRIPLE = "x86_64-unknown-linux-musl"
COMPONENT = "nas-relay-linux-x86_64"
FILES = [ROOT / "dist/weavelo-relay-linux-x86_64",
         ROOT / "dist/weavelo-relay-linux-x86_64.build.json"]


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def sha(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for data in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(data)
    return value.hexdigest()


def git(*args):
    return subprocess.check_output(["git", "--no-optional-locks", "-C", str(ROOT), *args],
                                   text=True, timeout=30).strip()


def inputs():
    paths = git("ls-files", "--", "remote_core", "protocol", "Cargo.toml", "Cargo.lock",
                ".cargo/config.toml", "scripts/build_nas_relay.py",
                "scripts/test_stream_settings_model.py", "scripts/release_artifact_lifecycle.py",
                "deploy/nas/weavelo-relay.service").splitlines()
    require(paths and "scripts/build_nas_relay.py" in paths, "Builder must be tracked in primary main")
    records = {name: sha(ROOT / name) for name in sorted(paths)}
    return records, hashlib.sha256(json.dumps(records, sort_keys=True).encode()).hexdigest()


def run_mode(mode, fd):
    command = [sys.executable, "-B", str(ROOT / "scripts/test_stream_settings_model.py"),
               "--mode", mode, "--lifecycle-authority", str(ROOT / release.AUTHORITY),
               "--lifecycle-binding", str(ROOT / ".rust-storage/adoption.json"),
               "--lifecycle-helper", str(release.HELPER)]
    print(json.dumps({"step": mode, "state": "starting", "command": command}), flush=True)
    child = subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE,
                             stderr=subprocess.STDOUT, text=True, start_new_session=True,
                             pass_fds=(fd,))
    started = time.monotonic()
    try:
        while True:
            try:
                output, _ = child.communicate(timeout=30)
                break
            except subprocess.TimeoutExpired:
                print(json.dumps({"step": mode, "state": "running",
                                  "elapsed_seconds": round(time.monotonic()-started),
                                  "free_gib": round(shutil.disk_usage(TARGET).free/1024**3, 3)}),
                      flush=True)
    except BaseException:
        if child.poll() is None:
            child.send_signal(signal.SIGINT)
            while child.poll() is None:
                try:
                    child.communicate(timeout=30)
                except subprocess.TimeoutExpired:
                    print("Waiting for owned guarded compiler cleanup; producer lock retained",
                          flush=True)
        raise
    match = re.search(r"^Validation receipt: (.+)$", output, re.M)
    require(match is not None, "Guarded build did not produce its original receipt")
    path = Path(match[1])
    require(path.is_relative_to(ROOT / ".rust-storage/validation-logs") and path.is_file(),
            "Unexpected guarded receipt path")
    events = [json.loads(line) for line in path.read_text().splitlines()]
    last = events[-1]
    if child.returncode:
        print(output[-12000:], flush=True)
    require(child.returncode == 0 and last.get("event") == "finished" and
            last.get("exit_code") == 0 and last.get("child_exit_code") == 0 and
            last.get("post_observation_exit") == 0 and
            last.get("sources_unchanged") is True and last.get("owned_group_absent") is True,
            "Guarded compiler/test/cleanup failed; inspect original receipt, do not duplicate")
    if mode == "relay-transport":
        require(last["tests_observed"] > 0 and last["reported_test_results"]["failed"] == 0,
                "No passing relay tests observed")
    print(json.dumps({"step": mode, "state": "passed", "receipt": str(path),
                      "tests": last.get("reported_test_results")}), flush=True)
    return {"path": str(path), "sha256": sha(path), "finished": last}


def elf(binary):
    data = Path(binary).read_bytes()
    require(data[:7] == b"\x7fELF\x02\x01\x01", "Expected Linux ELF64 little-endian binary")
    header = struct.unpack("<16sHHIQQQIHHHHHH", data[:64])
    require(header[1] in (2, 3) and header[2] == 62, "Expected x86_64 ELF executable")
    offset, size, count = header[5], header[9], header[10]
    require(size == 56 and count < 256 and offset + size * count <= len(data),
            "Invalid ELF program headers")
    require(all(struct.unpack_from("<I", data, offset+i*size)[0] != 3 for i in range(count)),
            "NAS relay must not require a dynamic interpreter")
    return {"architecture": "x86_64", "format": "ELF64", "dynamic_interpreter": False}


def main():
    require(ROOT == ROOT.resolve() and git("branch", "--show-current") == "main",
            "Build only in the canonical primary main checkout")
    require(not git("status", "--porcelain=v1", "--untracked-files=no"),
            "Commit the reviewed source changes in main before release build")
    require(sys.platform == "darwin" and TARGET.is_dir(), "Use the established Studio Cargo cache")
    require(shutil.disk_usage(TARGET).free >= guarded.RESERVE, "Existing 4 GiB reserve not met")
    commit = git("rev-parse", "HEAD")
    before, source_digest = inputs()
    with guarded.owned_interruption(), release.producer("nas-relay") as (fd, state):
        FILES[0].parent.mkdir(mode=0o755, exist_ok=True)
        admission = release.public("release-admit", "--component", COMPONENT,
                                   "--producer-pid", state["producer_pid"],
                                   "--file", FILES[0], "--file", FILES[1])
        require(admission.get("admission") == "reserved", "Current output was not reserved")
        staged = [Path(p) for p in admission["staged_files"]]
        tests = run_mode("relay-transport", fd)
        build = run_mode("nas-relay-linux-build", fd)
        require(git("rev-parse", "HEAD") == commit and inputs()[0] == before,
                "Source changed during relay producer; do not publish")
        binary = TARGET / TRIPLE / "release/examples/tcp_relay_server"
        info = elf(binary)
        shutil.copyfile(binary, staged[0])
        staged[0].chmod(0o755)
        require(sha(binary) == sha(staged[0]), "Compiler binary changed during staging")
        unit = ROOT / "deploy/nas/weavelo-relay.service"
        require("WEAVELO_RELAY_BIND_ADDR=127.0.0.1:39493" in unit.read_text() and
                "WEAVELO_RELAY_WEBSOCKET=1" in unit.read_text(), "Wrong isolated NAS relay unit")
        receipt = {"schema": "weavelo.nas_relay.build.v1", "component": COMPONENT,
                   "source_commit": commit, "source_inputs_sha256": source_digest,
                   "source_files": before, "target": TRIPLE, "profile": "release",
                   "incremental_cache": str(TARGET), "binary_sha256": sha(staged[0]),
                   "binary_bytes": staged[0].stat().st_size, "ELF": info,
                   "guarded_tests": tests, "guarded_build": build,
                   "unit_sha256": sha(unit), "validation_kind": "unsigned-dev",
                   "desktop_packaging_or_installation": False, "device_E2E": "not_performed"}
        staged[1].write_text(json.dumps(receipt, indent=2, sort_keys=True)+"\n")
        staged[1].chmod(0o600)
        result = release.public("release-publish", "--component", COMPONENT,
                                "--producer-pid", state["producer_pid"],
                                "--file", FILES[0], "--file", FILES[1],
                                "--expected-sha256", sha(staged[0]),
                                "--expected-sha256", sha(staged[1]),
                                "--validation-kind", "unsigned-dev",
                                "--validation-file", staged[1],
                                "--validation-digest", sha(staged[1]))
        require(result.get("current_committed") is True, "Current relay publish not confirmed")
        print(json.dumps({"state": "published", "binary": str(FILES[0]),
                          "receipt": str(FILES[1]), "source_commit": commit,
                          "binary_sha256": receipt["binary_sha256"],
                          "lifecycle": result}), flush=True)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError) as error:
        print("RELAY_BUILD_REFUSED: "+str(error), file=sys.stderr)
        raise SystemExit(1)
