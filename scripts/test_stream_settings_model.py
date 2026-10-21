#!/usr/bin/env python3
"""Fixed local regressions and Linux X11 test compilation; no GUI/network."""
import argparse
import contextlib
import hashlib
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

RESERVE = 4 * 1024**3
LINUX_TARGET = "x86_64-unknown-linux-gnu"
BUILD_TIMEOUT_SECONDS = 1800
# Native GPUI dependencies can exceed the ordinary focused-test build window.
# Keep this fixed and bounded; other modes and test execution retain their limits.
MACOS_CHECK_BUILD_TIMEOUT_SECONDS = 7200
TEST_TIMEOUT_SECONDS = 120
OBSERVATION_TIMEOUT_SECONDS = 60
# Fixed package/filter pairs only. These modes share the same guarded transaction;
# callers cannot supply Cargo arguments, alternate profiles or test executables.
SESSION_CLOCK_TESTS = {
    "native-build-toolchain": ("client", "native_build_toolchain_tests"),
    "lifecycle-capability": ("weavelo_app", "gui_backend::tests"),
    "lifecycle-ownership": ("weavelo_app", "desktop::instance::tests"),
    "lifecycle-service-contract": ("weavelo_app", "background_service::service_contract_tests"),
    "session-security": ("remote_core", "session_"),
    "clock-measurement": ("remote_core", "clock_sync::tests"),
    "clock-timing": ("remote_core", "timing::tests"),
    "clock-protocol": ("protocol", "session_auth"),
    "clock-media": ("remote_core", "client_session::tests"),
    "clock-workspace": ("remote_core", "workspace_session::"),
    "clock-host-cleared": ("host", "service::failure_admission_tests::cleared_auth_"),
    "host-resubscribe-lifecycle": ("host", "service::failure_admission_tests::resubscribe_allows_capture_source_switch"),
    "host-capture-lifecycle": ("host", "linux_capture::lifecycle_tests::"),
    "hevc-access-units": ("host", "ffmpeg_hevc::tests"),
    "clock-host-auth": ("host", "service::failure_admission_tests::finish_rechecks_native_owner_and_retired_finish_cannot_replace_new_session"),
    "clock-host-rotation": ("host", "service::failure_admission_tests::key_rotation_same_address_hello_keeps_pending_native_budget_until_release"),
    "transport-replay": ("remote_core", "net::tests"),
    "routed-auth": ("remote_core", "p2p::routed_p2p_tests"),
    "clock-display": ("weavelo_app", "telemetry_display::tests"),
    "clock-renderer": ("client", "render_timing::tests"),
    "mesh-profile": ("remote_core", "mesh::tests"),
    "relay-bootstrap": ("remote_core", "relay::tests::websocket_tls_bootstraps_without_ambient_provider"),
    "relay-transport": ("remote_core", "relay::tests"),
    "discovery-observation": ("remote_core", "discovery::tests"),
    "desktop-network": ("weavelo_app", "network_diagnostics::tests"),
    "computer-use": ("weavelo_app", "computer_use"),
    "computer-use-input": ("weavelo_app", "desktop::model::static_input_tests"),
    "computer-use-pixels": ("client", "observation_pixels::"),
    "bridge-network": ("remote_client_bridge", "network_diagnostics::tests"),
    "pairing-qr": ("remote_core", "pairing_qr::tests"),
    "pairing-image": ("remote_core", "pairing_image::tests"),
    "bridge-invitation": ("remote_client_bridge", "pairing_export::tests"),
    "bridge-startup": ("remote_client_bridge", "startup_diagnostics::tests"),
    "bridge-auth": ("remote_client_bridge", "publisher_bridge::tests"),
    "bridge-telemetry": ("remote_client_bridge", "tests::telemetry_defaults_are_zero_not_marketing_numbers"),
    "bridge-keepalive": ("remote_client_bridge", "tests::paused_session_uses_sparse_shared_keepalive_and_stops_input_grants"),
}


PLATFORM_CHECKS = {
    "lifecycle-linux-native-check": ("--tests", "--target", LINUX_TARGET, "--no-default-features",
                                     "--features", "gpui-restoration,native-linux-video"),
    "platform-macos-check": ("--bin", "weavelo", "--features", "gpui-native-video"),
    "platform-windows-check": ("--tests", "--target", "x86_64-pc-windows-gnu", "--features", "native-windows-video"),
    "platform-linux-check": ("--tests", "--target", LINUX_TARGET),
}


def validation_command(cargo, target, mode):
    shared = ["--offline", "--locked", "--target-dir", str(target)]
    if mode == "pairing-fixture":
        return [cargo, "run", *shared, "-p", "remote_core", "--example", "synthetic_pairing_qr"]
    if mode == "pairing-join":
        return [cargo, "test", *shared, "-p", "remote_core", "--features", "pairing-image", "--test", "pairing_join", "--", "--nocapture"]
    if mode == "lifecycle-linux-native-build":
        return [cargo, "build", *shared, "-p", "weavelo_app", "--bin", "weavelo",
                "--target", LINUX_TARGET, "--no-default-features",
                "--features", "gpui-restoration,native-linux-video", "-j", "2"]
    if mode in PLATFORM_CHECKS:
        return [cargo, "check", *shared, "-p", "weavelo_app", *PLATFORM_CHECKS[mode]]
    if mode in SESSION_CLOCK_TESTS:
        package, test_filter = SESSION_CLOCK_TESTS[mode]
        features = {"pairing-image": ["--features", "pairing-image"],
                    "bridge-invitation": ["--features", "android"], "bridge-network": ["--features", "android"]}.get(mode, [])
        return [cargo, "test", *shared, "-p", package, "--lib", *features,
                test_filter, "--", "--nocapture"]
    if mode == "service-contract":
        return [cargo, "test", *shared, "-p", "weavelo_app", "--lib",
                "service_contract", "--", "--nocapture"]
    if mode == "gui-input-status":
        return [cargo, "test", *shared, "-p", "weavelo_app", "--lib", "--no-default-features",
                "--features", "gpui-restoration", "restored_ui::input_tests::", "--", "--nocapture"]
    if mode in ("input-loss-build", "input-loss-tests"):
        return [cargo, "test" if mode.endswith("tests") else "build", *shared,
                "-p", "weavelo_app", "--example", "window_scoped_acceptance_guarded",
                "--no-default-features", "--features", "gpui-restoration"]
    if mode == "host-control":
        return [cargo, "test", *shared, "-p", "host", "--lib", "service::", "--", "--nocapture"]
    if mode == "source-catalog":
        return [cargo, "test", *shared, "-p", "host", "--lib", "source_catalog::tests", "--", "--nocapture"]
    if mode == "session-confirmation":
        return [cargo, "test", *shared, "-p", "remote_core", "--lib",
                "workspace_session::tests", "--", "--nocapture"]
    if mode == "media-acceptance-build":
        return [cargo, "build", *shared, "-p", "weavelo_app",
                "--example", "restored_gui_acceptance",
                "--no-default-features", "--features", "gpui-restoration"]
    if mode == "media-lifecycle-boundaries":
        return [cargo, "test", *shared, "-p", "client",
                "--lib", "payload_boundary", "--", "--nocapture"]
    if mode in ("media-lifecycle", "media-lifecycle-check"):
        return [cargo, "check" if mode.endswith("check") else "test", *shared,
                "-p", "remote_core", "-p", "client", "-p", "host", "-p", "weavelo_app",
                "--tests" if mode.endswith("check") else "--lib"]
    if mode == "linux-x11-check":
        return [cargo, "check", *shared, "-p", "gpui-ce", "--lib", "--tests",
                "--target", LINUX_TARGET]
    return [cargo, "test", *shared, "-p", "remote_core", "--lib",
            "stream_settings::tests", "--", "--nocapture"]


def validation_environment(mode):
    env = os.environ.copy()
    env.pop("RUST_LOG", None)
    env["CARGO_TERM_COLOR"] = "never"
    if (mode in SESSION_CLOCK_TESTS or mode in PLATFORM_CHECKS or mode == "lifecycle-linux-native-build" or mode in ("pairing-fixture", "pairing-join", "service-contract", "media-acceptance-build", "input-loss-build", "input-loss-tests", "gui-input-status")) and any(name in env for name in (
            "WEAVELO_BUILD_IDENTITY_FILE", "WEAVELO_BUILD_IDENTITY_RS")):
        raise ValueError("diagnostic development build must not inherit product identity inputs")
    if mode in ("linux-x11-check", "platform-linux-check", "lifecycle-linux-native-check", "lifecycle-linux-native-build"):
        zig = shutil.which("zig")
        cache = Path.home() / ".cache/zig"
        if not zig or not cache.is_dir():
            raise ValueError("Linux target check requires installed Zig and existing ~/.cache/zig")
        # Existing adapter translates cc-rs Rust triples to Zig triples.
        # Target-specific settings leave native build-script compilers unchanged.
        tools = Path(__file__).resolve().parent / "cmake"
        env["WEAVELO_VALIDATION_ZIG"] = str(Path(zig).resolve())
        # CMake requires a single compiler executable path. Embedded command
        # arguments can be discarded by cmake-rs, invoking cargo-zigbuild itself.
        env["CC_x86_64_unknown_linux_gnu"] = str(tools / "linux-zig-cc")
        env["CXX_x86_64_unknown_linux_gnu"] = str(tools / "linux-zig-cxx")
        env["AR_x86_64_unknown_linux_gnu"] = str(tools / "linux-zig-ar")
        env["CMAKE_TOOLCHAIN_FILE_x86_64_unknown_linux_gnu"] = str(tools / "linux-zig-toolchain.cmake")
        cmake = shutil.which("cmake")
        if not cmake:
            raise ValueError("Linux target build requires installed CMake")
        env["WEAVELO_VALIDATION_CMAKE"] = str(Path(cmake).resolve())
        env["CMAKE_x86_64_unknown_linux_gnu"] = str(tools / "linux-cmake")
        # audiopus_sys's pinned CMake helper prints toolchain variables but does
        # not register them as Cargo rerun inputs. Use its tracked pkg-config
        # selector to select the already-bundled Opus and invalidate stale host
        # archive configuration when our fixed toolchain changes.
        toolchain_bytes = b"".join((tools / name).read_bytes() for name in (
            "linux-zig-toolchain.cmake", "linux_zig_tool.py", "linux-zig-cc", "linux-zig-ar", "linux-zig-ranlib", "linux_cmake.py", "linux-cmake"))
        env["OPUS_NO_PKG_CONFIG"] = "weavelo-target-tools-" + hashlib.sha256(toolchain_bytes).hexdigest()
        # Keep Zig's enabled undefined-behavior checks as traps. Rust's linker
        # does not automatically supply Zig's diagnostic UBSan runtime.
        for variable in ("CFLAGS_x86_64_unknown_linux_gnu", "CXXFLAGS_x86_64_unknown_linux_gnu"):
            env[variable] = (env.get(variable, "") + " -fsanitize-trap=undefined").strip()
        env.pop("CMAKE_PREFIX_PATH", None)  # Never search host Homebrew/Qt as Linux libraries.
        env["ZIG_GLOBAL_CACHE_DIR"] = str(cache)
        env["ZIG_LOCAL_CACHE_DIR"] = str(cache)
        if mode == "lifecycle-linux-native-build":
            sysroot = Path(env.get("WEAVELO_VALIDATION_SYSROOT", ""))
            clang = Path(env.get("WEAVELO_VALIDATION_LINUX_CLANG", ""))
            if (not sysroot.is_absolute() or sysroot.is_symlink() or not sysroot.is_dir()
                    or not clang.is_absolute() or not clang.is_file()):
                raise ValueError("Linux link requires explicit installed Clang and verified target sysroot")
            for library in ("libc.so.6", "libasound.so.2", "libpipewire-0.3.so.0", "libavcodec.so.62", "libavutil.so.60"):
                path = sysroot / "usr/lib64" / library
                header = path.read_bytes()[:20]
                if (not path.resolve().is_relative_to(sysroot.resolve()) or
                        header[:6] != b"\x7fELF\x02\x01" or header[18:20] != b"\x3e\x00"):
                    raise ValueError("Non-x86_64 target sysroot library refused")
            env["CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER"] = str(tools / "linux-clang-link")
    return env


def requires_workspace_source_guard(mode):
    return (mode in SESSION_CLOCK_TESTS or mode in PLATFORM_CHECKS or mode in ("pairing-fixture", "pairing-join", "lifecycle-linux-native-build") or mode.startswith("media-lifecycle") or
            mode in ("service-contract", "media-acceptance-build", "session-confirmation", "host-control", "source-catalog", "input-loss-build", "input-loss-tests", "gui-input-status"))


@contextlib.contextmanager
def fixed_validation_lock(root):
    """Cloud project-runner lock only; no claim of managed/cleanup coverage."""
    if os.name != "posix":
        raise ValueError("explicit environment validation requires POSIX flock")
    import fcntl
    state = root / ".rust-storage"
    path = state / "fixed-validation.lock"
    if state.is_symlink() or not state.is_dir() or path.is_symlink():
        raise ValueError("validation lock must stay in existing project metadata")
    fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError("another project validation owns the activity lock") from error
        # Retain this exact inode; never unlink a lock another waiter may hold.
        yield
    finally:
        os.close(fd)


def validate_observed_target(report, target):
    target = target.resolve()
    references = report.get("binding", {}).get("selected_artifact_refs", [])
    if str(target) not in references or not target.is_dir():
        raise ValueError("actual Cargo target is not an existing observed artifact reference")
    return target


@contextlib.contextmanager
def owned_interruption():
    previous = signal.getsignal(signal.SIGTERM)
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt("validation cancelled; waiting for owned build before unlock")
    signal.signal(signal.SIGTERM, interrupted)
    try:
        yield
    finally:
        signal.signal(signal.SIGTERM, previous)


@contextlib.contextmanager
def finish_owned_transaction():
    # Once cancellation starts, a second interrupt cannot skip child wait or
    # release the activity lock while this runner's processes still use target.
    previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
    for sig in previous:
        signal.signal(sig, signal.SIG_IGN)
    try:
        yield
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def stop_owned_process(process):
    try:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGTERM)
        else:
            process.terminate()
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        try:
            if os.name == "posix":
                os.killpg(process.pid, signal.SIGKILL)
            else:
                process.kill()
        except ProcessLookupError:
            pass

        # A run deadline never authorizes releasing target while its child lives.
        process.wait()
    if os.name == "posix":
        # The group was created by this runner's start_new_session=True. Cargo
        # may exit before a descendant that ignored TERM, so wait(parent) alone
        # is not proof the owned process group has stopped.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def owned_group_absent(process):
    """Observe only the process group this runner created, never other namespaces."""
    if os.name != "posix":
        return None
    deadline = time.monotonic() + 5
    while True:
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)


@contextlib.contextmanager
def private_validation_log(root, mode):
    state = root / ".rust-storage"
    directory = state / "validation-logs"
    if state.is_symlink() or not state.is_dir() or directory.is_symlink():
        raise ValueError("validation logs must stay in existing project metadata")
    directory.mkdir(mode=0o700, exist_ok=True)
    if directory.stat().st_mode & 0o077:
        raise ValueError("validation log directory must be private (0700)")
    fd, name = tempfile.mkstemp(prefix=mode + "-", suffix=".log", dir=directory)
    path = Path(name)
    receipt_path = path.with_suffix(".jsonl")
    try:
        receipt_fd = os.open(receipt_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    except BaseException:
        os.close(fd)
        raise
    with os.fdopen(fd, "wb", buffering=0) as log, os.fdopen(receipt_fd, "w") as receipt:
        yield path, log, receipt_path, receipt


def execute_guarded_command(command, root, target, env, observation, sources, hashes, mode):
    build_timeout = (MACOS_CHECK_BUILD_TIMEOUT_SECONDS
                     if mode == "platform-macos-check" else BUILD_TIMEOUT_SECONDS)
    with private_validation_log(root, mode) as (log_path, log, receipt_path, receipt):
        print(f"Live private log: {log_path}", flush=True)
        print(f"Validation receipt: {receipt_path}", flush=True)
        def record(value):
            receipt.write(json.dumps(value, sort_keys=True) + "\n")
            receipt.flush()
        process = None
        reason, exit_code, error = "completed", 0, None
        started, tests_started, pending = time.monotonic(), None, b""
        tests_observed = 0
        test_results = {"passed": 0, "failed": 0, "ignored": 0}
        cleanup_error, group_absent, post_code, sources_unchanged = None, None, None, False
        try:
            process = subprocess.Popen(command, cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            record({"event": "started", "mode": mode, "pid": process.pid, "owned_process_group": process.pid,
                    "command": command, "build_timeout_seconds": build_timeout,
                    "test_timeout_seconds": TEST_TIMEOUT_SECONDS})
            # A separate open description avoids moving the child's output cursor.
            with log_path.open("rb") as reader:
                while process.poll() is None:
                    now = time.monotonic()
                    pending = (pending + reader.read(65536))[-131072:]
                    if tests_started is None and (b"Running unittests" in pending or re.search(rb"running \d+ tests?", pending)):
                        tests_started = now
                        record({"event": "tests_started", "elapsed_seconds": now - started})
                    if now - started >= build_timeout:
                        reason, exit_code = "build_deadline", 124
                        break
                    if tests_started is not None and now - tests_started >= TEST_TIMEOUT_SECONDS:
                        reason, exit_code = "test_deadline", 124
                        break
                    if shutil.disk_usage(target).free < RESERVE:
                        reason, exit_code = "disk_reserve", 125
                        break
                    time.sleep(0.2)
        except KeyboardInterrupt:
            reason, exit_code = "cancelled", 130
        except Exception as failure:
            reason, exit_code, error = "runner_error", 1, str(failure)
        finally:
            # Cancellation must not skip output, source checks or the final receipt.
            with finish_owned_transaction():
                if process is not None:
                    while True:
                        try:
                            stop_owned_process(process)
                            group_absent = owned_group_absent(process)
                            if group_absent is not False:
                                break
                        except Exception as failure:
                            cleanup_error = str(failure)
                        exit_code = exit_code or 1
                        # Disk-full/closed-console diagnostics cannot release the
                        # activity lock before our process group has stopped.
                        try:
                            record({"event": "cleanup_wait", "owned_process_group": process.pid,
                                    "error": cleanup_error, "activity_lock_retained": True})
                        except (OSError, ValueError):
                            pass
                        try:
                            print("Owned group not confirmed stopped; retaining activity lock", flush=True)
                        except (OSError, ValueError):
                            pass
                        time.sleep(1)
                try:
                    postflight = subprocess.run(observation, cwd=root, capture_output=True, text=True,
                                                timeout=OBSERVATION_TIMEOUT_SECONDS)
                    post_code = postflight.returncode
                    if post_code:
                        print(postflight.stderr or postflight.stdout, file=sys.stderr)
                except Exception as failure:
                    error = f"post-observation failed: {failure}"
                try:
                    sources_unchanged = hashes == [hashlib.sha256(path.read_bytes()).hexdigest() for path in sources]
                except OSError as failure:
                    error = f"source verification failed: {failure}"
                child_code = process.returncode if process is not None else None
                with log_path.open("rb") as reader:
                    for line in reader:
                        count = re.search(rb"^running (\d+) tests?", line.strip())
                        if count:
                            tests_observed += int(count[1])
                        result = re.search(rb"^test result: .*? (\d+) passed; (\d+) failed; (\d+) ignored;", line.strip())
                        if result:
                            for key, value in zip(test_results, result.groups()):
                                test_results[key] += int(value)
                if (mode in SESSION_CLOCK_TESTS or mode == "pairing-join") and not tests_observed:
                    error = "no matching library tests executed; result not accepted"
                    exit_code = exit_code or 1
                if not exit_code:
                    exit_code = child_code or post_code or 0
                if cleanup_error or group_absent is False or post_code is None or not sources_unchanged:
                    exit_code = exit_code or 1
                record({"event": "finished", "reason": reason, "exit_code": exit_code,
                        "child_exit_code": child_code, "post_observation_exit": post_code,
                        "sources_unchanged": sources_unchanged, "owned_group_absent": group_absent,
                        "tests_observed": tests_observed,
                        "reported_test_results": test_results,
                        "cleanup_error": cleanup_error, "error": error,
                        "elapsed_seconds": time.monotonic() - started})
                with log_path.open("rb") as reader:
                    reader.seek(max(0, log_path.stat().st_size - 12000))
                    print(reader.read().decode("utf-8", errors="replace"), flush=True)
                print(f"Exit: {exit_code}; reason: {reason}; post-observation: {post_code}; "
                      f"sources unchanged: {sources_unchanged}; owned group absent: {group_absent}; "
                      f"free GiB: {shutil.disk_usage(target).free / 1024**3:.3f}", flush=True)
        return exit_code


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--lifecycle-authority", type=Path)
    parser.add_argument("--lifecycle-binding", type=Path)
    parser.add_argument("--lifecycle-helper", type=Path)
    parser.add_argument("--mode", choices=("stream-settings", "service-contract", "pairing-fixture", "pairing-join", "lifecycle-linux-native-build", "linux-x11-check", "media-lifecycle", "media-lifecycle-check", "media-lifecycle-boundaries", "media-acceptance-build", "session-confirmation", "host-control", "source-catalog", "input-loss-build", "input-loss-tests", "gui-input-status", *SESSION_CLOCK_TESTS, *PLATFORM_CHECKS),
                        default="stream-settings")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    selectors = [args.lifecycle_authority, args.lifecycle_binding, args.lifecycle_helper]
    if any(selectors) and not all(selectors):
        parser.error("explicit lifecycle authority, binding and helper must be supplied together")
    # The legacy defaults remain intact. An explicitly reconciled environment
    # additionally serializes the whole project-specific test transaction.
    lock = fixed_validation_lock(root) if all(selectors) else contextlib.nullcontext()
    try:
        with lock, owned_interruption():
            return guarded_run(args, root, parser)
    except (ValueError, OSError) as error:
        parser.error(str(error))


def guarded_run(args, root, parser):
    observation = [sys.executable, "-B", str(root / "scripts/check_artifact_lifecycle.py"), "check"]
    for key in ("authority", "binding", "helper"):
        value = getattr(args, "lifecycle_" + key)
        if value is not None:
            observation.extend(["--" + key, str(value)])
    # Official adopt-check must pass; never compile through failed observation.
    preflight = subprocess.run(observation, cwd=root, capture_output=True, text=True)
    if preflight.returncode:
        print(preflight.stderr or preflight.stdout, file=sys.stderr)
        return preflight.returncode
    report = json.loads(preflight.stdout)
    print(f"Source workspace: {root}", flush=True)
    print("Git source: " + subprocess.check_output(
        ["git", "log", "-1", "--format=%H %D"], cwd=root, text=True
    ).strip(), flush=True)
    cargo = shutil.which("cargo")
    if not cargo:
        parser.error("cargo is required")
    metadata = json.loads(subprocess.check_output(
        [cargo, "metadata", "--offline", "--locked", "--no-deps", "--format-version=1"], cwd=root
    ))
    resolved_target = Path(metadata["target_directory"]).resolve()
    target = args.target_dir.resolve() if args.target_dir else resolved_target
    target = validate_observed_target(report, target)
    if args.lifecycle_authority and target != resolved_target:
        parser.error("explicit environment tests must reuse the actual configured Cargo target")
    build = metadata.get("build_directory")
    if build and args.lifecycle_authority:
        validate_observed_target(report, Path(build))
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
    if requires_workspace_source_guard(args.mode):
        inputs = ["*.rs", "*.m", "*.c", "*.h", "Cargo.toml", "Cargo.lock"]
        paths = subprocess.check_output(["git", "ls-files", *inputs], cwd=root, text=True).splitlines()
        paths += subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", *inputs], cwd=root, text=True).splitlines()
        sources = [root / path for path in sorted(set(paths))]
    sources.extend([root / "scripts/test_stream_settings_model.py", root / "scripts/check_artifact_lifecycle.py"])
    if args.mode in ("linux-x11-check", "platform-linux-check", "lifecycle-linux-native-check", "lifecycle-linux-native-build"):
        sources.extend(root / "scripts/cmake" / name for name in
                       ("linux_zig_tool.py", "linux-zig-cc", "linux-zig-cxx", "linux-zig-ar", "linux_clang_link.py", "linux-clang-link", "linux-zig-ranlib", "linux-zig-toolchain.cmake", "linux_cmake.py", "linux-cmake"))
    for path in (args.lifecycle_authority, args.lifecycle_binding, args.lifecycle_helper):
        if path is not None:
            sources.append(path)
    if args.lifecycle_helper:
        sources.extend(args.lifecycle_helper.parent / name for name in ("rust_scope.py", "rust_adopt.py", "rust_release.py"))
    hashes = [hashlib.sha256(path.read_bytes()).hexdigest() for path in sources]
    print(f"Using existing target: {target}", flush=True)
    print("Project-specific run; observation coverage remains false; no cleanup", flush=True)
    return execute_guarded_command(command, root, target, env, observation, sources, hashes, args.mode)


if __name__ == "__main__":
    raise SystemExit(main())
