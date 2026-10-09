#!/usr/bin/env python3
"""Fixed small three-slice regressions. No Cargo, GUI, network, or cache cleanup."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

sys.dont_write_bytecode = True
RESERVE = 4 * 1024**3
EXPECTED = {
    "device_list": [
        "projection_keeps_device_and_connection_status_separate_from_the_adapter",
        "state_actions_return_effects_without_starting_runtime_work",
        "switching_device_moves_active_state_without_rebinding_row_identity",
    ],
    "session_tabs": [
        "repeated_open_reuses_healthy_connection_and_replaces_unhealthy_one",
        "pending_duplicate_and_capacity_do_not_create_more_attempts",
        "selecting_a_pending_request_tracks_it_without_restarting_or_clearing_other_attempts",
        "switching_and_closing_keep_selection_of_current_other_and_last_pages",
        "cancelling_connect_preserves_old_page_and_late_completion_is_rejected",
        "reopen_after_cancel_or_clear_uses_new_generation_and_rejects_old_reply",
        "background_response_cannot_select_over_new_intent_and_failure_hides_old_page",
        "reconnect_actions_and_status_projection_use_scalar_identity_not_frames",
        "late_status_callback_cannot_overwrite_a_new_selection_or_reconnect",
        "cancelling_pending_connect_restores_first_background_arrival_without_routing_it_early",
        "resource_routing_and_reconnect_projection_share_transition_and_empty_selection_gate",
        "late_background_attach_preserves_empty_selection_after_foreground_failure",
    ],
    "stream_settings": [
        "drafts_survive_projection_without_changing_committed_values_or_emitting_effects",
        "invalid_custom_values_preserve_committed_settings_and_have_no_side_effects",
        "presets_preserve_other_values_and_sync_only_the_matching_draft",
        "editing_next_draft_keeps_committed_effect_valid_but_rejects_its_late_error",
        "connecting_request_consumes_latest_commit_without_consuming_unapplied_drafts",
        "pending_settings_are_isolated_by_device_and_reconnect_generation",
        "cancelled_failed_and_cleared_requests_cannot_retain_or_create_settings",
    ],
}
SOURCES = [
    "app/src/lib.rs", "app/src/main.rs", "app/src/restored_ui.rs",
    "app/src/desktop/device_list.rs", "app/src/desktop/device_drawer.rs",
    "app/src/desktop/original_owner.rs", "app/src/desktop/original_presenter.rs",
    "app/src/desktop/model.rs", "app/src/product_components.rs",
    "app/src/product_components/text_input.rs", "remote_core/src/lib.rs",
    "remote_core/src/role.rs", "remote_core/src/discovery.rs",
    "remote_core/src/session_tabs.rs", "remote_core/src/stream_settings.rs",
    "protocol/src/lib.rs", "Cargo.toml", "Cargo.lock", "app/Cargo.toml",
    "scripts/tests/test_desktop_gui_single_backend.py",
    "scripts/tests/test_desktop_gui_identity.py", "scripts/verify_desktop_gui.py",
    "scripts/tests/run_gui_slice_regressions.py",
]


def main():
    if not __debug__:
        raise SystemExit("Run this regression script without Python -O")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--preservation-ledger", type=Path,
        help="Optional authorized repository-relative status/SHA-256 ledger to verify; not required for the fixed test suite",
    )
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    ledger = json.loads(args.preservation_ledger.read_text()) if args.preservation_ledger else {}

    def git(*argv):
        return subprocess.check_output(["git", *argv], cwd=root).decode().rstrip("\n")

    def worktree_status():
        rows = git("status", "--porcelain=v1", "--no-renames", "-z", "--untracked-files=all")
        return {row[3:]: row[:2] for row in rows.split("\0") if row}

    head = git("rev-parse", "HEAD")
    branch = git("branch", "--show-current")
    initial_status = worktree_status()
    for relative in ledger:
        path = Path(relative)
        assert not path.is_absolute() and ".." not in path.parts, "ledger paths must be repository-relative"
        assert (root / path).resolve().is_relative_to(root), "ledger paths must stay in this repository"
    lock = Path(git("rev-parse", "--git-path", "index.lock"))
    if not lock.is_absolute():
        lock = root / lock

    def preserve():
        status = worktree_status()
        assert status == initial_status, "working-tree status changed during regression"
        for relative, item in ledger.items():
            assert status.get(relative) == item["status"], relative
            assert hashlib.sha256((root / relative).read_bytes()).hexdigest() == item["sha256"], relative
        assert not git("diff", "--cached", "--name-only"), "use an empty index"
        assert not lock.exists(), "an index operation is in progress"
        assert git("rev-parse", "HEAD") == head
        assert git("branch", "--show-current") == branch
        assert shutil.disk_usage(root).free >= RESERVE

    def hashes():
        return {path: hashlib.sha256((root / path).read_bytes()).hexdigest() for path in SOURCES}

    preserve()
    before = hashes()
    output = Path(tempfile.mkdtemp(prefix="remoteplay-gui-three-slice-"))
    assert root not in output.parents, "temporary output must stay outside the repository"
    assert shutil.disk_usage(output).free >= RESERVE

    def run(command, name):
        with (output / name).open("w+") as log:
            process = subprocess.Popen(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, start_new_session=(os.name == "posix"))
            started = time.monotonic()
            while process.poll() is None:
                if min(shutil.disk_usage(root).free, shutil.disk_usage(output).free) < RESERVE or time.monotonic() - started > 60:
                    if os.name == "posix":
                        os.killpg(process.pid, signal.SIGTERM)  # This runner's owned process only.
                    else:
                        process.terminate()
                    process.wait(timeout=5)
                    raise RuntimeError("reserve/time bound reached; owned process stopped; no cleanup")
                time.sleep(0.1)
            log.seek(0)
            text = log.read()
            assert process.returncode == 0, (command, process.returncode, text[-4000:])
            return text

    def block(text, start, end):
        begin = text.index(start)
        return text[begin:text.index(end, begin)]

    # Exact production definitions, not stand-in structs, limits, or validation.
    app = (root / "app/src/lib.rs").read_text()
    device = block(app, "#[derive(Debug, Clone, PartialEq, Eq)]\npub struct AppDevice", "#[derive(Debug, Clone, PartialEq, Eq)]\npub struct ViewingRequest")
    discovery = (root / "remote_core/src/discovery.rs").read_text()
    scope = block(discovery, "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub enum DiscoveryScope", "#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]\npub struct DiscoveryCapabilities")
    protocol = (root / "protocol/src/lib.rs").read_text()
    validator = block(protocol, "pub fn validate_video_settings(", "#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]\npub enum TouchAction")
    role = (root / "remote_core/src/role.rs").read_text().split("#[cfg(test)]")[0]
    harness = "extern crate self as remote_core;\nextern crate self as protocol;\n"
    harness += "use std::net::SocketAddr;\nuse discovery::DiscoveryScope;\nuse role::RolePeer;\n"
    harness += "pub mod role {\n" + role + "\n}\npub mod discovery {\n" + scope + "\n}\n" + validator + "\n" + device
    for module, path in [("device_list", "app/src/desktop/device_list.rs"), ("session_tabs", "remote_core/src/session_tabs.rs"), ("stream_settings", "remote_core/src/stream_settings.rs")]:
        harness += f"#[path={json.dumps(str(root / path), ensure_ascii=False)}] mod {module};\n"
    source = output / "three_slice_models.rs"
    source.write_text(harness)
    binary = output / ("three_slice_models.exe" if os.name == "nt" else "three_slice_models")
    rustc = shutil.which("rustc")
    assert rustc
    run([rustc, "--edition", "2024", "--test", str(source), "-o", str(binary)], "compile.log")
    listed = run([str(binary), "--list"], "rust-list.log")
    discovered = re.findall(r"^(.+): test$", listed, re.M)
    expected = {f"{module}::tests::{name}" for module, names in EXPECTED.items() for name in names}
    assert len(discovered) == len(set(discovered)) == len(expected) == 22
    assert set(discovered) == expected, set(discovered) ^ expected
    rust_results = []
    for number, name in enumerate(sorted(expected), 1):
        text = run([str(binary), "--exact", name, "--nocapture"], f"rust-{number:02}.log")
        assert "1 passed; 0 failed; 0 ignored" in text, name
        rust_results.append(name)

    sys.path.insert(0, str(root))
    suite = unittest.defaultTestLoader.loadTestsFromNames([
        "scripts.tests.test_desktop_gui_single_backend",
        "scripts.tests.test_desktop_gui_identity",
    ])
    def flatten(tests):
        for test in tests:
            if isinstance(test, unittest.TestSuite):
                yield from flatten(test)
            else:
                yield test.id()
    python_ids = list(flatten(suite))
    assert len(python_ids) == len(set(python_ids)) == 16
    log = io.StringIO()
    result = unittest.TextTestRunner(stream=log, verbosity=2).run(suite)
    (output / "python.log").write_text(log.getvalue())
    assert result.wasSuccessful(), log.getvalue()
    assert result.testsRun == 16 and not result.skipped
    assert before == hashes(), "production source changed during regression"
    preserve()
    receipt = {
        "head": head, "branch": branch, "source_hashes": before, "rust_unique_passed": len(rust_results),
        "rust_by_slice": {module: len(names) for module, names in EXPECTED.items()},
        "rust_test_ids": rust_results, "python_unique_passed": result.testsRun,
        "python_test_ids": python_ids, "unique_total_passed": len(rust_results) + result.testsRun,
        "failed": 0, "ignored": 0, "skipped": [], "preserved_paths": len(ledger),
        "preservation_ledger_supplied": bool(args.preservation_ledger),
        "unchanged_worktree_status_entries": len(initial_status),
        "head_unchanged": True, "index_empty": True, "index_lock_absent": True,
        "source_stable_during_compile_and_tests": True,
        "disk_free_bytes": shutil.disk_usage(root).free,
        "harness": "standard-library test wrapper, exact current production definitions and three path modules",
        "not_run": ["GPUI mock/native windows", "real GUI/IME/a11y", "Owner live resource/network tests", "capture/decode/display/input/audio/files", "physical Linux/Windows/macOS acceptance", "signing integration/release suite", "prior standalone duplicate repro", "full Cargo build/check"],
        "cargo_invoked": False, "gui_started": False, "real_network_started": False,
        "repository_mutated": False, "cleanup": False, "new_commit": False,
    }
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"Output: {output}")
    print(json.dumps({key: receipt[key] for key in ["head", "rust_by_slice", "python_unique_passed", "unique_total_passed", "failed", "ignored", "skipped", "preserved_paths", "disk_free_bytes"]}, indent=2))


if __name__ == "__main__":
    main()
