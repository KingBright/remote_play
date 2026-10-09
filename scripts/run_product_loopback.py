#!/usr/bin/env python3
"""Bound one explicitly synthetic product fixture; never build or deploy it.

The process receipt records exit, not visual/E2E acceptance. Window controls must
be exercised on the actual production dashboard and backed by separate evidence.
"""
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("preflight", "window"))
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    if binary.name != "product_loopback_e2e":
        parser.error("only the explicit product_loopback_e2e fixture is allowed")
    root = Path(__file__).resolve().parents[1]
    maintenance = Path.home() / "Library/Caches/clean-mac-storage/cleanup.pid"
    if maintenance.exists() or shutil.disk_usage(root).free < 4 * 1024**3:
        parser.error("host maintenance/4 GiB reserve prevents test startup")
    sources = ["app/examples/product_loopback_e2e.rs",
               "app/examples/support/product_loopback_peer.rs",
               "app/src/restored_ui.rs", "app/src/desktop/original_owner.rs",
               "app/src/lib.rs", "Cargo.lock", "scripts/run_product_loopback.py"]
    digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
    hashes = {name: digest(root / name) for name in sources}
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    evidence = Path(tempfile.mkdtemp(prefix=f"remoteplay-product-{args.mode}-"))
    log_path = evidence / "process.log"
    # Keep a caller-supplied .app/Contents/MacOS path for NSBundle identity;
    # resolve only for byte verification, never collapse the launch path.
    launch_path = args.binary.absolute()
    command = [str(launch_path)] + (["--preflight"] if args.mode == "preflight" else [])
    limit = 45 if args.mode == "preflight" else 210
    started = time.monotonic()
    stop_reason = None
    with log_path.open("w") as log:
        process = subprocess.Popen(command, cwd=root, stdout=log,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        fixture_root = Path(f"/tmp/remoteplay-product-loopback-{process.pid}")
        receipt = {"mode": args.mode, "pid": process.pid, "head": head,
                   "fixture_root": str(fixture_root), "log": str(log_path),
                   "binary": str(binary), "launch_path": str(launch_path), "binary_sha256": digest(binary),
                   "source_hashes": hashes, "deadline_seconds": limit,
                   "generated_content": True, "full_product_acceptance": False}
        (evidence / "running.json").write_text(json.dumps(receipt, indent=2) + "\n")
        print(json.dumps({"evidence": str(evidence), **receipt}), flush=True)
        progress = started
        while process.poll() is None:
            elapsed = time.monotonic() - started
            if maintenance.exists():
                stop_reason = "host maintenance started"
            elif shutil.disk_usage(root).free < 4 * 1024**3:
                stop_reason = "4 GiB reserve reached"
            elif elapsed > limit:
                stop_reason = "owned test deadline exceeded"
            if stop_reason:
                # A new session belongs solely to this invocation and its children.
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=3)
                break
            if time.monotonic() - progress >= 20:
                print(json.dumps({"pid": process.pid, "elapsed_seconds": round(elapsed, 2),
                                  "fixture_root": str(fixture_root)}), flush=True)
                progress = time.monotonic()
            time.sleep(0.2)
    receipt.update(exit_code=process.returncode, stop_reason=stop_reason,
                   elapsed_seconds=round(time.monotonic() - started, 3),
                   sources_stable=all(digest(root / name) == value for name, value in hashes.items()),
                   process_exited=True, log_sha256=digest(log_path),
                   free_bytes=shutil.disk_usage(root).free)
    if args.mode == "preflight" and (fixture_root / "preflight.json").exists():
        receipt["preflight"] = json.loads((fixture_root / "preflight.json").read_text())
    (evidence / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps({"evidence": str(evidence), **receipt}), flush=True)
    return 0 if process.returncode == 0 and stop_reason is None and receipt["sources_stable"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
