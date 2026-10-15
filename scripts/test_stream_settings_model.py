#!/usr/bin/env python3
"""Fixed local regressions and Linux X11 test compilation; no GUI/network."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

RESERVE = 4 * 1024**3
LINUX_TARGET = "x86_64-unknown-linux-gnu"


def validation_command(cargo, target, mode):
    shared = ["--offline", "--locked", "--target-dir", str(target)]
    if mode in ("input-loss-build", "input-loss-tests"):
        return [cargo, "test" if mode.endswith("tests") else "build", *shared,
                "-p", "remote_play_app", "--example", "window_scoped_acceptance_guarded",
                "--no-default-features", "--features", "gpui-restoration"]
    if mode == "session-confirmation":
        return [cargo, "test", *shared, "-p", "remote_core", "--lib",
                "workspace_session::tests", "--", "--nocapture"]
    if mode == "media-acceptance-build":
        return [cargo, "build", *shared, "-p", "remote_play_app",
                "--example", "restored_gui_acceptance",
                "--no-default-features", "--features", "gpui-restoration"]
    if mode == "media-lifecycle-boundaries":
        return [cargo, "test", *shared, "-p", "client",
                "--lib", "payload_boundary", "--", "--nocapture"]
    if mode in ("media-lifecycle", "media-lifecycle-check"):
        return [cargo, "check" if mode.endswith("check") else "test", *shared,
                "-p", "remote_core", "-p", "client", "-p", "host", "-p", "remote_play_app",
                "--tests" if mode.endswith("check") else "--lib"]
    if mode == "linux-x11-check":
        return [cargo, "check", *shared, "-p", "gpui-ce", "--lib", "--tests",
                "--target", LINUX_TARGET]
    return [cargo, "test", *shared, "-p", "remote_core", "--lib",
            "stream_settings::tests", "--", "--nocapture"]


def validation_environment(mode):
    env = os.environ.copy()
    env.pop("RUST_LOG", None)
    if mode in ("media-acceptance-build", "input-loss-build", "input-loss-tests") and any(name in env for name in (
            "REMOTEPLAY_BUILD_IDENTITY_FILE", "REMOTEPLAY_BUILD_IDENTITY_RS")):
        raise ValueError("diagnostic development build must not inherit product identity inputs")
    if mode == "linux-x11-check":
        zig = shutil.which("zig")
        wrapper = shutil.which("cargo-zigbuild")
        cache = Path.home() / ".cache/zig"
        if not zig or not wrapper or not cache.is_dir():
            raise ValueError("Linux X11 check requires installed Zig, cargo-zigbuild and existing ~/.cache/zig")
        # Existing adapter translates cc-rs Rust triples to Zig triples.
        # Target-specific settings leave native build-script compilers unchanged.
        env["CC_x86_64_unknown_linux_gnu"] = f"{wrapper} zig cc -- -target x86_64-linux-gnu"
        env["CXX_x86_64_unknown_linux_gnu"] = f"{wrapper} zig c++ -- -target x86_64-linux-gnu"
        env["AR_x86_64_unknown_linux_gnu"] = f"{wrapper} zig ar --"
        env["ZIG_GLOBAL_CACHE_DIR"] = str(cache)
        env["ZIG_LOCAL_CACHE_DIR"] = str(cache)
    return env


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--mode", choices=("stream-settings", "linux-x11-check", "media-lifecycle", "media-lifecycle-check", "media-lifecycle-boundaries", "media-acceptance-build", "session-confirmation", "input-loss-build", "input-loss-tests"),
                        default="stream-settings")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    # Refresh stale evidence explicitly with the existing adoption tool; never
    # compile through a failed lifecycle observation.
    preflight = subprocess.run(
        [sys.executable, "-B", str(root / "scripts/check_artifact_lifecycle.py"), "check"],
        cwd=root, capture_output=True, text=True,
    )
    if preflight.returncode:
        print(preflight.stderr or preflight.stdout, file=sys.stderr)
        return preflight.returncode
    print(f"Source workspace: {root}", flush=True)
    print("Git source: " + subprocess.check_output(
        ["git", "log", "-1", "--format=%H %D"], cwd=root, text=True
    ).strip(), flush=True)
    cargo = shutil.which("cargo")
    if not cargo:
        parser.error("cargo is required")
    if args.target_dir:
        target = args.target_dir.resolve()
    else:
        metadata = subprocess.check_output(
            [cargo, "metadata", "--offline", "--locked", "--no-deps", "--format-version=1"], cwd=root
        )
        target = Path(json.loads(metadata)["target_directory"])
    if not target.is_dir():
        parser.error("use an existing target; this script does not create a new target")
    if shutil.disk_usage(target).free < RESERVE:
        parser.error("less than 4 GiB free; no build started")
    command = validation_command(cargo, target, args.mode)
    try:
        env = validation_environment(args.mode)
    except ValueError as error:
        parser.error(str(error))
    sources = [root / "remote_core/src/stream_settings.rs", root / "protocol/src/lib.rs"]
    if args.mode == "linux-x11-check":
        sources = [root / path for path in (
            "Cargo.toml", "Cargo.lock", "third_party/gpui-ce/Cargo.toml",
            "third_party/gpui-ce/build.rs", "third_party/gpui-ce/src/platform/linux/x11/client.rs")]
        print("Linux X11 tests: compile check only; no native execution", flush=True)
    if args.mode.startswith("media-lifecycle") or args.mode in ("media-acceptance-build", "session-confirmation", "input-loss-build", "input-loss-tests"):
        paths = subprocess.check_output(["git", "ls-files", "*.rs", "Cargo.toml", "Cargo.lock"], cwd=root, text=True).splitlines()
        paths += subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "*.rs"], cwd=root, text=True).splitlines()
        sources = [root / path for path in sorted(set(paths))]
    hashes = [hashlib.sha256(path.read_bytes()).hexdigest() for path in sources]
    print(f"Using existing target: {target}", flush=True)
    with tempfile.TemporaryFile(mode="w+") as log:
        process = subprocess.Popen(command, cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        while process.poll() is None:
            if shutil.disk_usage(target).free < RESERVE:
                os.killpg(process.pid, signal.SIGTERM)  # Only this script's owned build group.
                process.wait()
                raise SystemExit("4 GiB reserve reached; owned build stopped, no cache cleaned")
            time.sleep(0.2)
        if hashes != [hashlib.sha256(path.read_bytes()).hexdigest() for path in sources]:
            raise SystemExit("sources changed during tests; result not accepted")
        log.seek(0)
        output = log.read()
        marker = output.rfind("running ")
        print(output[marker:] if process.returncode == 0 and marker >= 0 else output[-12000:])
        print(f"Exit: {process.returncode}; free GiB: {shutil.disk_usage(target).free / 1024**3:.3f}")
        return process.returncode


if __name__ == "__main__":
    raise SystemExit(main())
