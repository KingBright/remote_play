#!/usr/bin/env python3
"""Small form-state regressions against the real protocol validator; no GUI/network."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time

RESERVE = 4 * 1024**3


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
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
    command = [cargo, "test", "--offline", "--locked", "--target-dir", str(target),
               "-p", "remote_core", "--lib", "stream_settings::tests", "--", "--nocapture"]
    sources = [root / "remote_core/src/stream_settings.rs", root / "protocol/src/lib.rs"]
    hashes = [hashlib.sha256(path.read_bytes()).hexdigest() for path in sources]
    print(f"Using existing target: {target}", flush=True)
    with tempfile.TemporaryFile(mode="w+") as log:
        process = subprocess.Popen(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
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
