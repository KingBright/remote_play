"""The focused service contract tests retain the existing guarded build entry."""
from pathlib import Path
import contextlib
import io
import hashlib
import json
import os
import signal
import subprocess
import sys
import tempfile
from unittest.mock import Mock, patch
import unittest

import scripts.test_stream_settings_model as runner
from scripts.test_stream_settings_model import (validation_command, validation_environment,
    fixed_validation_lock, validate_observed_target, stop_owned_process, finish_owned_transaction,
    requires_workspace_source_guard)


class ServiceContractEntryTests(unittest.TestCase):
    SESSION_CLOCK_CASES = {
        'native-build-toolchain': ('client', 'native_build_toolchain_tests'),
        'lifecycle-capability': ('weavelo_app', 'gui_backend::tests'),
        'lifecycle-ownership': ('weavelo_app', 'desktop::instance::tests'),
        'lifecycle-service-contract': ('weavelo_app', 'background_service::service_contract_tests'),
        'session-security': ('remote_core', 'session_'),
        'clock-measurement': ('remote_core', 'clock_sync::tests'),
        'clock-timing': ('remote_core', 'timing::tests'),
        'clock-protocol': ('protocol', 'session_auth'),
        'clock-media': ('remote_core', 'client_session::tests'),
        'clock-workspace': ('remote_core', 'workspace_session::'),
        'clock-host-auth': ('host', 'service::failure_admission_tests::finish_rechecks_native_owner_and_retired_finish_cannot_replace_new_session'),
        'clock-host-rotation': ('host', 'service::failure_admission_tests::key_rotation_same_address_hello_keeps_pending_native_budget_until_release'),
        'clock-host-cleared': ('host', 'service::failure_admission_tests::cleared_auth_'),
        'host-resubscribe-lifecycle': ('host', 'service::failure_admission_tests::resubscribe_allows_capture_source_switch'),
        'host-capture-lifecycle': ('host', 'linux_capture::lifecycle_tests::'),
        'hevc-access-units': ('host', 'ffmpeg_hevc::tests'),
        'transport-replay': ('remote_core', 'net::tests'),
        'routed-auth': ('remote_core', 'p2p::routed_p2p_tests'),
        'clock-display': ('weavelo_app', 'telemetry_display::tests'),
        'clock-renderer': ('client', 'render_timing::tests'),
        'mesh-profile': ('remote_core', 'mesh::tests'),
        'relay-bootstrap': ('remote_core', 'relay::tests::websocket_tls_bootstraps_without_ambient_provider'),
        'relay-transport': ('remote_core', 'relay::tests'),
        'bridge-network': ('remote_client_bridge', 'network_diagnostics::tests'),
        'pairing-qr': ('remote_core', 'pairing_qr::tests'),
        'pairing-image': ('remote_core', 'pairing_image::tests'),
        'bridge-invitation': ('remote_client_bridge', 'pairing_export::tests'),
        'bridge-startup': ('remote_client_bridge', 'startup_diagnostics::tests'),
        'bridge-auth': ('remote_client_bridge', 'publisher_bridge::tests'),
        'bridge-telemetry': ('remote_client_bridge', 'tests::telemetry_defaults_are_zero_not_marketing_numbers'),
        'bridge-keepalive': ('remote_client_bridge', 'tests::paused_session_uses_sparse_shared_keepalive_and_stops_input_grants'),
    }

    def test_linux_cmake_uses_target_archive_tools_and_invalidates_old_opus_cache(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / ".cache/zig").mkdir(parents=True)
            with patch.object(Path, "home", return_value=root), patch.object(runner.shutil, "which", return_value="/installed/zig"), \
                    patch.dict(os.environ, {"CC": "native-cc", "AR": "native-ar"}, clear=True):
                first = validation_environment("lifecycle-linux-native-check")
                self.assertEqual(first["CC"], "native-cc")
                self.assertEqual(first["AR"], "native-ar")
                self.assertTrue(first["CMAKE_TOOLCHAIN_FILE_x86_64_unknown_linux_gnu"].endswith("/linux-zig-toolchain.cmake"))
                self.assertTrue(first["OPUS_NO_PKG_CONFIG"].startswith("weavelo-target-tools-"))
                self.assertIn("-fsanitize-trap=undefined", first["CFLAGS_x86_64_unknown_linux_gnu"])
                with patch.object(Path, "read_bytes", return_value=b"changed toolchain"):
                    second = validation_environment("lifecycle-linux-native-check")
                self.assertNotEqual(first["OPUS_NO_PKG_CONFIG"], second["OPUS_NO_PKG_CONFIG"])

    def test_linux_link_build_is_fixed_and_preserves_source_and_identity_guards(self):
        mode = "lifecycle-linux-native-build"
        self.assertEqual(validation_command("/cargo", Path("/existing/target"), mode), [
            "/cargo", "build", "--offline", "--locked", "--target-dir", "/existing/target",
            "-p", "weavelo_app", "--bin", "weavelo", "--target", "x86_64-unknown-linux-gnu",
            "--no-default-features", "--features", "gpui-restoration,native-linux-video", "-j", "2"])
        self.assertTrue(requires_workspace_source_guard(mode))
        with patch.dict(os.environ, {"WEAVELO_BUILD_IDENTITY_FILE": "/fixture/identity"}):
            with self.assertRaisesRegex(ValueError, "product identity inputs"):
                validation_environment(mode)

    def test_linux_link_build_refuses_missing_or_host_architecture_sysroot(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / ".cache/zig").mkdir(parents=True)
            libraries = root / "sysroot/usr/lib64"
            libraries.mkdir(parents=True)
            clang = root / "clang"
            clang.touch()
            with patch.object(Path, "home", return_value=root), patch.object(runner.shutil, "which", return_value=str(clang)):
                with patch.dict(os.environ, {}, clear=True):
                    with self.assertRaisesRegex(ValueError, "explicit installed Clang"):
                        validation_environment("lifecycle-linux-native-build")
                for name in ("libc.so.6", "libasound.so.2", "libpipewire-0.3.so.0", "libavcodec.so.62", "libavutil.so.60"):
                    (libraries / name).write_bytes(b"Mach-O arm64 host object")
                with patch.dict(os.environ, {"WEAVELO_VALIDATION_LINUX_CLANG": str(clang),
                        "WEAVELO_VALIDATION_SYSROOT": str(root / "sysroot")}, clear=True):
                    with self.assertRaisesRegex(ValueError, "Non-x86_64"):
                        validation_environment("lifecycle-linux-native-build")

    def test_platform_checks_keep_fixed_targets_and_source_identity_guards(self):
        expected = {
            "lifecycle-linux-native-check": ("--tests", "--target", "x86_64-unknown-linux-gnu", "--no-default-features",
                                              "--features", "gpui-restoration,native-linux-video"),
            "platform-macos-check": ("--bin", "weavelo", "--features", "gpui-native-video"),
            "platform-windows-check": ("--tests", "--target", "x86_64-pc-windows-gnu", "--features", "native-windows-video"),
            "platform-linux-check": ("--tests", "--target", "x86_64-unknown-linux-gnu"),
        }
        self.assertEqual(runner.PLATFORM_CHECKS, expected)
        for mode, arguments in expected.items():
            with self.subTest(mode=mode):
                self.assertEqual(validation_command("/cargo", Path("/existing/target"), mode),
                    ["/cargo", "check", "--offline", "--locked", "--target-dir", "/existing/target",
                     "-p", "weavelo_app", *arguments])
                self.assertTrue(requires_workspace_source_guard(mode))
                with patch.dict(os.environ, {"WEAVELO_BUILD_IDENTITY_FILE": "/fixture/identity"}):
                    with self.assertRaisesRegex(ValueError, "product identity inputs"):
                        validation_environment(mode)

    def test_session_clock_modes_have_only_fixed_locked_offline_package_filters(self):
        self.assertEqual(runner.SESSION_CLOCK_TESTS, self.SESSION_CLOCK_CASES)
        for mode, (package, test_filter) in self.SESSION_CLOCK_CASES.items():
            with self.subTest(mode=mode):
                features = {'pairing-image': ['--features', 'pairing-image'],
                            'bridge-invitation': ['--features', 'android'], 'bridge-network': ['--features', 'android']}.get(mode, [])
                self.assertEqual(validation_command('/existing/cargo', Path('/existing/target'), mode), [
                    '/existing/cargo', 'test', '--offline', '--locked', '--target-dir',
                    '/existing/target', '-p', package, '--lib', *features, test_filter, '--', '--nocapture',
                ])

    def test_pairing_modes_keep_actual_module_fixture_and_source_identity_guards(self):
        for mode, arguments in (
            ('pairing-join', ['test', '-p', 'remote_core', '--features', 'pairing-image', '--test', 'pairing_join', '--', '--nocapture']),
            ('pairing-fixture', ['run', '-p', 'remote_core', '--example', 'synthetic_pairing_qr']),
        ):
            with self.subTest(mode=mode):
                self.assertEqual(validation_command('/cargo', Path('/target'), mode),
                    ['/cargo', arguments[0], '--offline', '--locked', '--target-dir', '/target', *arguments[1:]])
                self.assertTrue(requires_workspace_source_guard(mode))
                with patch.dict(os.environ, {'WEAVELO_BUILD_IDENTITY_FILE': '/fixture/identity'}):
                    with self.assertRaisesRegex(ValueError, 'product identity inputs'):
                        validation_environment(mode)

    def test_session_clock_modes_use_the_existing_guarded_run_entry(self):
        for mode in self.SESSION_CLOCK_CASES:
            with self.subTest(mode=mode), patch('sys.argv', ['runner', '--mode', mode]), \
                    patch.object(runner, 'guarded_run', return_value=0) as guarded:
                self.assertEqual(runner.main(), 0)
                guarded.assert_called_once()
                self.assertEqual(guarded.call_args.args[0].mode, mode)

    def test_cli_rejects_arbitrary_test_selection_before_guarded_run(self):
        for arguments in (['--mode', 'arbitrary-filter'], ['--mode', 'clock-timing', '--', '--release']):
            with self.subTest(arguments=arguments), patch('sys.argv', ['runner', *arguments]), \
                    patch.object(runner, 'guarded_run') as guarded, contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as stopped:
                    runner.main()
                self.assertEqual(stopped.exception.code, 2)
                guarded.assert_not_called()

    def test_session_clock_modes_hash_all_workspace_rust_sources(self):
        for mode in self.SESSION_CLOCK_CASES:
            with self.subTest(mode=mode):
                self.assertTrue(requires_workspace_source_guard(mode))
        self.assertTrue(requires_workspace_source_guard('service-contract'))
        self.assertTrue(requires_workspace_source_guard('host-control'))
        self.assertTrue(requires_workspace_source_guard('media-lifecycle-check'))
        self.assertFalse(requires_workspace_source_guard('stream-settings'))

    def test_session_clock_modes_refuse_product_identity_and_remove_runtime_log_overrides(self):
        for mode in self.SESSION_CLOCK_CASES:
            with self.subTest(mode=mode), patch.dict(os.environ, {'RUST_LOG': 'trace'}):
                self.assertNotIn('RUST_LOG', validation_environment(mode))
            for key in ('WEAVELO_BUILD_IDENTITY_FILE', 'WEAVELO_BUILD_IDENTITY_RS'):
                with self.subTest(mode=mode, key=key), patch.dict(os.environ, {key: '/fixture/identity'}):
                    with self.assertRaisesRegex(ValueError, 'product identity inputs'):
                        validation_environment(mode)

    def test_service_contract_uses_fixed_offline_locked_existing_target_command(self):
        target = Path('/existing/target')
        command = validation_command('/existing/cargo', target, 'service-contract')
        self.assertEqual(command, [
            '/existing/cargo', 'test', '--offline', '--locked', '--target-dir',
            str(target), '-p', 'weavelo_app', '--lib', 'service_contract',
            '--', '--nocapture',
        ])
        self.assertNotIn('--release', command)

    def test_service_contract_does_not_select_a_cross_platform_or_new_target_environment(self):
        environment = validation_environment('service-contract')
        self.assertNotIn('RUST_LOG', environment)
        # No lifecycle bypass flag or custom compiler is introduced by this mode.
        source = Path(__file__).parents[1].joinpath('test_stream_settings_model.py').read_text()
        self.assertIn('if preflight.returncode:', source)
        self.assertIn('return preflight.returncode', source)
        self.assertIn('"service-contract", ', source)

    def test_existing_target_must_match_observed_canonical_reference(self):
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary).resolve()
            report = {"binding": {"selected_artifact_refs": [str(target)]}}
            self.assertEqual(validate_observed_target(report, target), target)
            for changed in ({}, {"binding": {"selected_artifact_refs": [str(target / 'other')]}}):
                with self.assertRaises(ValueError):
                    validate_observed_target(changed, target)
            with self.assertRaises(ValueError):
                validate_observed_target({"binding": {"selected_artifact_refs": [str(target / 'absent')]}}, target / 'absent')

    @unittest.skipUnless(os.name == 'posix', 'POSIX activity lock')
    def test_activity_lock_excludes_second_runner_and_preserves_the_lock_inode(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / '.rust-storage').mkdir()
            with fixed_validation_lock(root):
                lock = root / '.rust-storage/fixed-validation.lock'
                before = lock.stat().st_ino
                with self.assertRaisesRegex(ValueError, 'activity lock'):
                    with fixed_validation_lock(root):
                        self.fail('second runner entered')
            self.assertEqual(lock.stat().st_ino, before)
            with fixed_validation_lock(root):
                self.assertEqual(lock.stat().st_ino, before)

    @unittest.skipUnless(os.name == 'posix', 'POSIX process group')
    def test_cancel_waits_for_owned_process_and_never_targets_other_groups(self):
        process = Mock(pid=12345)
        process.poll.return_value = None
        process.wait.side_effect = [subprocess.TimeoutExpired('owned', 10), 0]
        with patch('scripts.test_stream_settings_model.os.killpg') as kill:
            stop_owned_process(process)
        self.assertEqual(kill.call_args_list[0].args, (12345, signal.SIGTERM))
        self.assertEqual(kill.call_args_list[1].args, (12345, signal.SIGKILL))
        self.assertEqual(process.wait.call_count, 2)

    @unittest.skipUnless(os.name == 'posix', 'POSIX process group')
    def test_finished_parent_does_not_hide_remaining_owned_descendants(self):
        process = Mock(pid=12345)
        process.poll.return_value = 0
        with patch('scripts.test_stream_settings_model.os.killpg') as kill:
            stop_owned_process(process)
        self.assertEqual([call.args for call in kill.call_args_list],
                         [(12345, signal.SIGTERM), (12345, signal.SIGKILL)])
        process.wait.assert_called_once_with(timeout=10)

    def test_repeated_cancel_is_deferred_until_owned_transaction_finishes(self):
        before = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
        with finish_owned_transaction():
            for sig in before:
                self.assertEqual(signal.getsignal(sig), signal.SIG_IGN)
        for sig, handler in before.items():
            self.assertEqual(signal.getsignal(sig), handler)

    def test_service_contract_refuses_inherited_product_identity_inputs(self):
        for key in ('WEAVELO_BUILD_IDENTITY_FILE', 'WEAVELO_BUILD_IDENTITY_RS'):
            with self.subTest(key=key), patch.dict(os.environ, {key: '/fixture/identity'}):
                with self.assertRaisesRegex(ValueError, 'product identity inputs'):
                    validation_environment('service-contract')

    def run_fake_validation(self, *, output=b'running 1 test\ntest fixture ... ok\n',
                            interrupt=False, test_timeout=None, build_timeout=None,
                            post_code=0, source_changed=False, group_absent=True, cleanup_failure=False,
                            diagnostic_failure=False, mode='clock-host-auth',
                            elapsed=None, expected_build_timeout=None):
        with tempfile.TemporaryDirectory() as temporary, contextlib.ExitStack() as stack:
            root = Path(temporary)
            (root / '.rust-storage').mkdir()
            source = root / 'source.rs'
            source.write_text('stable')
            hashes = [hashlib.sha256(source.read_bytes()).hexdigest()]
            process = Mock(pid=12345, returncode=0)
            process.poll.side_effect = [None, 0]
            def launch(*_args, **kwargs):
                kwargs['stdout'].write(output)
                self.assertTrue(kwargs['start_new_session'])
                if source_changed:
                    source.write_text('changed')
                return process
            stack.enter_context(patch.object(runner.subprocess, 'Popen', side_effect=launch))
            post = stack.enter_context(patch.object(runner.subprocess, 'run', return_value=Mock(returncode=post_code, stderr='post failed', stdout='')))
            stopped = stack.enter_context(patch.object(runner, 'stop_owned_process'))
            if cleanup_failure:
                stopped.side_effect = [OSError('owned wait failed'), None]
            group_probe = stack.enter_context(patch.object(runner, 'owned_group_absent',
                side_effect=[False, True] if not group_absent else [True]))
            stack.enter_context(patch.object(runner.shutil, 'disk_usage', return_value=Mock(free=runner.RESERVE * 2)))
            stack.enter_context(patch.object(runner.time, 'sleep', side_effect=KeyboardInterrupt if interrupt else None))
            if elapsed is not None:
                stack.enter_context(patch.object(runner.time, 'monotonic', side_effect=[0, elapsed, elapsed]))
            if test_timeout is not None:
                stack.enter_context(patch.object(runner, 'TEST_TIMEOUT_SECONDS', test_timeout))
            if build_timeout is not None:
                stack.enter_context(patch.object(runner, 'BUILD_TIMEOUT_SECONDS', build_timeout))
            if diagnostic_failure:
                original_dumps = json.dumps
                def failed_wait_receipt(value, **kwargs):
                    if value.get('event') == 'cleanup_wait':
                        raise OSError('no space for receipt')
                    return original_dumps(value, **kwargs)
                def failed_wait_console(message='', **_kwargs):
                    if 'retaining activity lock' in str(message):
                        raise BrokenPipeError('console closed')
                stack.enter_context(patch.object(runner.json, 'dumps', side_effect=failed_wait_receipt))
                stack.enter_context(patch.object(runner, 'print', create=True, side_effect=failed_wait_console))
            def observed_after_cleanup(*_args, **_kwargs):
                self.assertEqual(stopped.call_count, 2 if cleanup_failure or not group_absent else 1)
                self.assertEqual(group_probe.call_count, 1 if group_absent else 2)
                return Mock(returncode=post_code, stderr='post failed', stdout='')
            post.side_effect = observed_after_cleanup
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            code = runner.execute_guarded_command(['cargo', 'test', '--lib'], root, root, {},
                                                  ['official-observe'], [source], hashes, mode)
            self.assertEqual(stopped.call_count, 2 if cleanup_failure or not group_absent else 1)
            stopped.assert_called_with(process)
            post.assert_called_once()
            self.assertEqual(post.call_args.kwargs['timeout'], runner.OBSERVATION_TIMEOUT_SECONDS)
            directory = root / '.rust-storage/validation-logs'
            log = next(directory.glob('*.log'))
            receipt = next(directory.glob('*.jsonl'))
            self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
            self.assertEqual(log.stat().st_mode & 0o777, 0o600)
            self.assertEqual(receipt.stat().st_mode & 0o777, 0o600)
            self.assertEqual(log.read_bytes(), output)
            records = [json.loads(line) for line in receipt.read_text().splitlines()]
            self.assertEqual(records[0]['event'], 'started')
            if expected_build_timeout is not None:
                self.assertEqual(records[0]['build_timeout_seconds'], expected_build_timeout)
            self.assertEqual(records[-1]['event'], 'finished')
            if not group_absent or cleanup_failure:
                self.assertEqual(group_probe.call_count, 1 if group_absent else 2)
                if not diagnostic_failure:
                    self.assertTrue(any(row.get('event') == 'cleanup_wait' and row['activity_lock_retained'] for row in records))
                self.assertTrue(records[-1]['owned_group_absent'])
            return code, records[-1]

    def test_private_live_log_and_success_receipt_preserve_output_and_guards(self):
        code, receipt = self.run_fake_validation()
        self.assertEqual(code, 0)
        self.assertTrue(receipt['sources_unchanged'])
        self.assertTrue(receipt['owned_group_absent'])
        self.assertEqual(receipt['tests_observed'], 1)

    def test_cancel_still_checks_sources_postobserve_and_writes_nonzero_receipt(self):
        code, receipt = self.run_fake_validation(interrupt=True)
        self.assertEqual(code, 130)
        self.assertEqual(receipt['reason'], 'cancelled')
        self.assertTrue(receipt['sources_unchanged'])
        self.assertEqual(receipt['post_observation_exit'], 0)

    def test_fixed_build_and_test_deadlines_terminate_owned_group_and_fail(self):
        for arguments, reason in (({'test_timeout': 0}, 'test_deadline'),
                                  ({'build_timeout': 0}, 'build_deadline')):
            with self.subTest(reason=reason):
                code, receipt = self.run_fake_validation(**arguments)
                self.assertEqual(code, 124)
                self.assertEqual(receipt['reason'], reason)
                self.assertEqual(receipt['post_observation_exit'], 0)

    def test_macos_compile_window_is_longer_but_still_bounded_and_mode_specific(self):
        for mode, elapsed, expected, deadline in (
            ('platform-macos-check', 1801, 0, 7200),
            ('platform-macos-check', 7201, 124, 7200),
            ('clock-host-auth', 1801, 124, 1800),
        ):
            with self.subTest(mode=mode, elapsed=elapsed):
                code, receipt = self.run_fake_validation(
                    mode=mode, elapsed=elapsed, expected_build_timeout=deadline,
                    output=b'Checking native dependencies\n')
                self.assertEqual(code, expected)
                self.assertEqual(receipt['reason'], 'completed' if expected == 0 else 'build_deadline')
                self.assertTrue(receipt['owned_group_absent'])
                self.assertTrue(receipt['sources_unchanged'])
                self.assertEqual(receipt['post_observation_exit'], 0)

    def test_source_change_failed_observation_and_live_group_cannot_report_success(self):
        for arguments in ({'source_changed': True}, {'post_code': 9}, {'group_absent': False}):
            with self.subTest(arguments=arguments):
                code, _ = self.run_fake_validation(**arguments)
                self.assertNotEqual(code, 0)

    def test_empty_library_filter_is_failure_not_pass(self):
        code, receipt = self.run_fake_validation(output=b'running 0 tests\ntest result: ok. 0 passed\n')
        self.assertNotEqual(code, 0)
        self.assertEqual(receipt['tests_observed'], 0)

    def test_cleanup_exception_keeps_waiting_until_confirmed_before_postobserve(self):
        code, receipt = self.run_fake_validation(cleanup_failure=True)
        self.assertNotEqual(code, 0)
        self.assertTrue(receipt['owned_group_absent'])
        self.assertIn('owned wait failed', receipt['cleanup_error'])

    def test_receipt_separates_started_tests_from_reported_results(self):
        code, receipt = self.run_fake_validation(output=(b'running 3 tests\n'
            b'test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured\n'))
        self.assertEqual(code, 0)
        self.assertEqual(receipt['tests_observed'], 3)
        self.assertEqual(receipt['reported_test_results'], {'passed': 2, 'failed': 0, 'ignored': 1})

    def test_cleanup_diagnostic_write_failure_cannot_release_live_group(self):
        code, receipt = self.run_fake_validation(group_absent=False, diagnostic_failure=True)
        self.assertNotEqual(code, 0)
        self.assertTrue(receipt['owned_group_absent'])
        self.assertEqual(receipt['post_observation_exit'], 0)

    def test_private_log_refuses_public_directory_or_symlink(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / '.rust-storage').mkdir()
            directory = root / '.rust-storage/validation-logs'
            directory.mkdir(mode=0o755)
            # A restrictive host umask must not turn this public fixture private.
            directory.chmod(0o755)
            with self.assertRaisesRegex(ValueError, 'private'):
                with runner.private_validation_log(root, 'clock-host-auth'):
                    self.fail('public logs admitted')
            directory.rmdir()
            directory.symlink_to(root, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'metadata'):
                with runner.private_validation_log(root, 'clock-host-auth'):
                    self.fail('symlink logs admitted')

    @unittest.skipUnless(os.name == 'posix', 'POSIX process group')
    def test_owned_group_absence_probe_never_targets_another_group(self):
        with patch.object(runner.os, 'killpg', side_effect=ProcessLookupError) as kill:
            self.assertTrue(runner.owned_group_absent(Mock(pid=12345)))
        kill.assert_called_once_with(12345, 0)

    @unittest.skipUnless(os.name == 'posix', 'POSIX owned-process integration')
    def test_real_owned_python_timeout_leaves_log_and_complete_cleanup_receipt(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / '.rust-storage').mkdir()
            source = root / 'source.rs'
            source.write_text('stable')
            command = [sys.executable, '-c', "import time; print('running 1 test', flush=True); time.sleep(60)"]
            with patch.object(runner, 'TEST_TIMEOUT_SECONDS', 0.01), \
                    patch.object(runner.shutil, 'disk_usage', return_value=Mock(free=runner.RESERVE * 2)), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = runner.execute_guarded_command(command, root, root, os.environ.copy(),
                    [sys.executable, '-c', 'pass'], [source],
                    [hashlib.sha256(source.read_bytes()).hexdigest()], 'clock-host-auth')
            self.assertEqual(code, 124)
            directory = root / '.rust-storage/validation-logs'
            self.assertIn(b'running 1 test', next(directory.glob('*.log')).read_bytes())
            receipt = json.loads(next(directory.glob('*.jsonl')).read_text().splitlines()[-1])
            self.assertEqual(receipt['reason'], 'test_deadline')
            self.assertTrue(receipt['owned_group_absent'])
            self.assertTrue(receipt['sources_unchanged'])
            self.assertEqual(receipt['post_observation_exit'], 0)
