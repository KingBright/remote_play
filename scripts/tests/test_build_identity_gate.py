"""Identity boundary regressions using disposable non-GUI source/artifact fixtures."""
from copy import deepcopy
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_identity_gate as gate


class BuildIdentityGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='rp-identity-tests-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.repo = self.root / 'source'; self.repo.mkdir()
        self.run_git('init', '-q')
        self.run_git('config', 'user.name', 'Fixture')
        self.run_git('config', 'user.email', 'fixture@example.invalid')
        (self.repo / 'Cargo.toml').write_text('[workspace]\n')
        (self.repo / 'main.rs').write_text('fn main() {}\n')
        self.run_git('add', 'Cargo.toml', 'main.rs')
        self.run_git('commit', '-q', '-m', 'fixture')
        self.commit = self.run_git('rev-parse', 'HEAD').strip()
        self.source = gate.source_snapshot(self.repo, self.commit)
        self.config = gate.configuration('linux', 'x86_64', '2.0.0-alpha.8',
                                         'x86_64-unknown-linux-gnu', 'dev', 'a' * 64, 'b' * 64)
        self.identity = gate.identity(self.source, self.config)
        self.stage = self.root / 'stage'; self.stage.mkdir()
        self.app = self.stage / 'RemotePlay'; self.app.mkdir()
        self.binary = self.app / 'remote_play'; self.binary.write_bytes(b'compiled fixture bytes')
        self.binary.chmod(0o755)
        self.info = dict(schema=1, product='RemotePlay', version='2.0.0-alpha.8',
                         platform='linux', architecture='x86_64', default_gui=gate.RENDERER,
                         original_gui_compiled=True, native_video_compiled=True,
                         build_identity=self.identity)
        self.receipt = {'schema': 1, 'kind': 'observed-build', 'fresh_target': True,
                        'compiler_binary_sha256': gate.hash_file(self.binary),
                        'product_info': self.info, 'build_identity': self.identity}

    def run_git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args], text=True)

    def seal(self):
        with patch.object(gate, 'product_info', return_value=self.info):
            result = gate.seal_tree(self.stage, self.receipt, self.commit, self.identity['identity_sha256'])
        self.manifest_sha = result['manifest_sha256']; self.manifest = result['manifest']
        return result

    def zip(self, extra=None, omit_manifest=False):
        path = self.root / 'package.zip'
        with zipfile.ZipFile(path, 'w') as z:
            for f in self.stage.rglob('*'):
                if f.is_file() and not (omit_manifest and f.name == gate.MANIFEST):
                    z.write(f, f.relative_to(self.stage).as_posix())
            for name, data in (extra or {}).items(): z.writestr(name, data)
        return path

    def verify_package(self, path):
        return gate.verify_package(path, self.manifest_sha, self.commit, self.identity['identity_sha256'])

    def rewrite_manifest(self, data):
        raw = json.dumps(data).encode()
        (self.stage / gate.MANIFEST).write_bytes(raw)
        return gate.digest(raw)

    def fake_metadata(self, info):
        return type('Completed', (), {'returncode': 0, 'stdout': json.dumps(info).encode()})()

    def test_clean_source_is_actual_bytes_and_full_commit(self):
        self.assertEqual(self.source['state'], 'clean')
        self.assertEqual(self.source['file_count'], 2)
        with self.assertRaises(gate.IdentityRejected): gate.source_snapshot(self.repo, 'f' * 40)

    def test_unapproved_dirty_source_cannot_be_called_clean(self):
        (self.repo / 'main.rs').write_text('fn main() { panic!(); }\n')
        with self.assertRaises(gate.IdentityRejected): gate.source_snapshot(self.repo, self.commit)
        observed = gate.source_snapshot(self.repo, self.commit, inspect_only=True)
        accepted = gate.source_snapshot(self.repo, self.commit, expected_patch=observed['patch_sha256'])
        self.assertEqual(accepted['state'], 'patch')
        self.assertNotEqual(accepted['snapshot_sha256'], self.source['snapshot_sha256'])
        with self.assertRaises(gate.IdentityRejected):
            gate.source_snapshot(self.repo, self.commit, expected_patch='a' * 64)

    def test_untracked_implementation_is_in_the_approved_patch(self):
        (self.repo / 'native.c').write_text('int input(void) {return 1;}\n')
        observed = gate.source_snapshot(self.repo, self.commit, inspect_only=True)
        source = gate.source_snapshot(self.repo, self.commit, expected_patch=observed['patch_sha256'])
        self.assertIn('native.c', [f['path'] for f in source['files']])

    def test_private_file_is_rejected_before_hashing(self):
        secret = self.repo / 'mesh.secret'; secret.write_text('fixture must not be read')
        original = gate.hash_file
        with patch.object(gate, 'hash_file', wraps=original) as hashes:
            with self.assertRaises(gate.IdentityRejected):
                gate.source_snapshot(self.repo, self.commit, inspect_only=True)
            self.assertNotIn(secret, [call.args[0] for call in hashes.call_args_list])

    def test_deleted_source_is_bound_in_patch(self):
        (self.repo / 'main.rs').unlink()
        observed = gate.source_snapshot(self.repo, self.commit, inspect_only=True)
        source = gate.source_snapshot(self.repo, self.commit, expected_patch=observed['patch_sha256'])
        self.assertEqual(source['file_count'], 1)
        self.assertNotEqual(source['snapshot_sha256'], self.source['snapshot_sha256'])

    def test_gui_or_feature_change_is_not_an_approved_config(self):
        for key, value in [('gui_entry', 'egui-diagnostic'), ('features', ['egui']), ('default_features', 0)]:
            config = deepcopy(self.config); config[key] = value
            with self.assertRaises(gate.IdentityRejected): gate.identity(self.source, config)

    def test_source_commit_identity_cannot_be_forged_in_a_self_digest(self):
        fake = deepcopy(self.identity); fake['source']['commit'] = 'f' * 40
        fake = gate.identity(fake['source'], fake['configuration'])
        with self.assertRaises(gate.IdentityRejected):
            gate.validate_identity(fake, self.commit, fake['identity_sha256'])

    def test_changed_compiler_environment_is_rejected(self):
        target = self.root / 'new-target'
        with patch.object(gate, 'toolchain_digest', return_value='a' * 64), \
             patch.object(gate, 'environment_digest', return_value='c' * 64):
            with self.assertRaises(gate.IdentityRejected): gate.observed_build(self.repo, self.identity, target)
        self.assertFalse(target.exists())

    def test_unproven_cache_is_preserved_and_refused(self):
        target = self.root / 'signed-build-cache'; target.mkdir()
        old = target / 'recoverable-binary'; old.write_bytes(b'preserve')
        with patch.object(gate, 'toolchain_digest', return_value='a' * 64), \
             patch.object(gate, 'environment_digest', return_value='b' * 64):
            with self.assertRaises(gate.IdentityRejected): gate.observed_build(self.repo, self.identity, target)
        self.assertEqual(old.read_bytes(), b'preserve')

    def test_no_loose_binary_receipt_can_be_sealed(self):
        self.receipt['kind'] = 'manual-sidecar'
        with self.assertRaises(gate.IdentityRejected):
            gate.seal_tree(self.stage, self.receipt, self.commit, self.identity['identity_sha256'])

    def test_unknown_binary_is_never_executed_for_metadata(self):
        with patch.object(gate.subprocess, 'run') as execute:
            with self.assertRaises(gate.IdentityRejected): gate.product_info(self.binary, self.identity, 'f' * 64)
            execute.assert_not_called()

    def test_old_same_version_gui_without_compiled_identity_is_rejected(self):
        old = deepcopy(self.info); del old['build_identity']
        with patch.object(gate.subprocess, 'run', return_value=self.fake_metadata(old)):
            with self.assertRaises(gate.IdentityRejected):
                gate.product_info(self.binary, self.identity, gate.hash_file(self.binary))

    def test_diagnostic_gui_cannot_be_renamed_as_product(self):
        old = deepcopy(self.info); old['diagnostic_gui_requires_opt_in'] = True
        with patch.object(gate.subprocess, 'run', return_value=self.fake_metadata(old)):
            with self.assertRaises(gate.IdentityRejected):
                gate.product_info(self.binary, self.identity, gate.hash_file(self.binary))

    def test_metadata_claiming_another_snapshot_is_rejected(self):
        old = deepcopy(self.info); old['build_identity']['source']['snapshot_sha256'] = 'f' * 64
        with patch.object(gate.subprocess, 'run', return_value=self.fake_metadata(old)):
            with self.assertRaises(gate.IdentityRejected):
                gate.product_info(self.binary, self.identity, gate.hash_file(self.binary))

    def test_correct_source_and_feature_identity_is_verified_without_a_window(self):
        with patch.object(gate.subprocess, 'run', return_value=self.fake_metadata(self.info)) as execute:
            info = gate.product_info(self.binary, self.identity, gate.hash_file(self.binary))
        self.assertEqual(info['build_identity'], self.identity)
        self.assertEqual(execute.call_args.args[0], [str(self.binary), '--product-info-json'])

    def test_valid_package_binds_all_payload_but_never_authorizes_release(self):
        self.seal(); result = self.verify_package(self.zip())
        self.assertTrue(result['package_integrity_verified'])
        self.assertFalse(result['release_authorized'])
        self.assertEqual(result['manifest']['binary_sha256'], gate.hash_file(self.binary))

    def test_package_without_manifest_cannot_use_loose_build_info(self):
        self.seal()
        with self.assertRaises(gate.IdentityRejected): self.verify_package(self.zip(omit_manifest=True))

    def test_package_binary_tamper_is_rejected(self):
        self.seal(); self.binary.write_bytes(b'tampered fixture bytes')
        with self.assertRaises(gate.IdentityRejected): self.verify_package(self.zip())

    def test_package_manifest_tamper_is_rejected_against_external_anchor(self):
        self.seal(); m = deepcopy(self.manifest); m['build_identity']['source']['commit'] = 'f' * 40
        self.rewrite_manifest(m)
        with self.assertRaises(gate.IdentityRejected): self.verify_package(self.zip())

    def test_reanchored_diagnostic_manifest_still_fails_policy(self):
        self.seal(); m = deepcopy(self.manifest); m['product_info']['default_gui'] = 'egui-diagnostic'
        self.manifest_sha = self.rewrite_manifest(m)
        with self.assertRaises(gate.IdentityRejected): self.verify_package(self.zip())

    def test_zip_traversal_extra_file_and_case_alias_rejected(self):
        self.seal()
        for name in ('../outside', 'RemotePlay/unknown.dll', 'remoteplay/remote_play', 'RemotePlay/CON'):
            with self.subTest(name=name), self.assertRaises(gate.IdentityRejected):
                self.verify_package(self.zip({name: b'x'}))

    def test_case_alias_in_ancestor_is_rejected(self):
        self.seal()
        with self.assertRaises(gate.IdentityRejected):
            self.verify_package(self.zip({'RemotePlay/sub/a': b'x', 'RemotePlay/Sub/b': b'y'}))

    def test_tar_link_is_not_extracted_or_followed(self):
        self.seal(); archive = self.root / 'linked.tar'
        with tarfile.open(archive, 'w') as t:
            item = tarfile.TarInfo('RemotePlay/remote_play'); item.type = tarfile.SYMTYPE
            item.linkname = '/private/never-read'; t.addfile(item)
        with self.assertRaises(gate.IdentityRejected): self.verify_package(archive)

    def test_duplicate_json_and_boolean_schema_are_rejected(self):
        with self.assertRaises(gate.IdentityRejected): gate.json_bytes(b'{"schema":1,"schema":2}')
        fake = deepcopy(self.identity); fake['schema'] = True
        with self.assertRaises(gate.IdentityRejected):
            gate.validate_identity(fake, self.commit, self.identity['identity_sha256'])

    def test_old_app_cannot_keep_the_daily_entry_even_with_same_bytes(self):
        self.seal(); old = self.root / 'old_remote_play'; shutil.copy2(self.binary, old)
        with self.assertRaises(gate.IdentityRejected):
            gate.verify_install(self.app, self.stage / gate.MANIFEST, self.manifest_sha,
                                self.commit, self.identity['identity_sha256'], self.binary, old)

    def test_installation_is_checked_against_sealed_manifest(self):
        self.seal()
        with patch.object(gate, 'product_info', return_value=self.info):
            result = gate.verify_install(self.app, self.stage / gate.MANIFEST, self.manifest_sha,
                                         self.commit, self.identity['identity_sha256'], self.binary, self.binary)
        self.assertTrue(result['daily_entry_verified'])
        self.assertFalse(result['release_authorized'])
        self.binary.write_bytes(b'old cached GUI')
        with self.assertRaises(gate.IdentityRejected):
            gate.verify_install(self.app, self.stage / gate.MANIFEST, self.manifest_sha,
                                self.commit, self.identity['identity_sha256'], self.binary, self.binary)

    def test_missing_install_manifest_and_linked_entry_fail(self):
        self.seal(); link = self.root / 'daily_entry'; link.symlink_to(self.binary)
        with self.assertRaises(gate.IdentityRejected):
            gate.verify_install(self.app, self.stage / gate.MANIFEST, self.manifest_sha,
                                self.commit, self.identity['identity_sha256'], self.binary, link)
        with self.assertRaises(OSError):
            gate.verify_install(self.app, self.root / 'missing.json', self.manifest_sha,
                                self.commit, self.identity['identity_sha256'], self.binary, self.binary)

    def test_cannot_overwrite_a_versioned_manifest(self):
        self.seal()
        with self.assertRaises(gate.IdentityRejected): self.seal()

    def test_unavailable_native_process_evidence_has_no_argv_fallback(self):
        with patch.object(gate.sys, 'platform', 'linux'), patch.object(gate.os, 'readlink', side_effect=PermissionError):
            with self.assertRaises(gate.IdentityRejected):
                gate.verify_running_linux(1, self.binary, gate.hash_file(self.binary))

    def test_other_platform_process_claim_is_not_linux_acceptance(self):
        with patch.object(gate.sys, 'platform', 'darwin'):
            with self.assertRaises(gate.IdentityRejected):
                gate.verify_running_linux(1, self.binary, gate.hash_file(self.binary))

    @unittest.skipUnless(sys.platform.startswith('linux'), 'Linux loaded-image test requires native /proc')
    def test_actual_owned_non_gui_process_binds_kernel_inode_and_hash(self):
        executable = self.root / 'process_fixture'; shutil.copy2('/bin/sleep', executable)
        with subprocess.Popen([str(executable), '15']) as process:
            try:
                result = gate.verify_running_linux(process.pid, executable, gate.hash_file(executable))
                self.assertTrue(result['actual_loaded_image_verified'])
                replacement = self.root / 'different'; shutil.copy2('/bin/true', replacement)
                replacement.replace(executable)
                with self.assertRaises(gate.IdentityRejected):
                    gate.verify_running_linux(process.pid, executable, gate.hash_file(executable))
            finally:
                process.terminate(); process.wait(timeout=5)



class IndependentNativeIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='rp-identity-native-')
        self.root = Path(self.temp.name).resolve()
        self.addCleanup(self.temp.cleanup)
        self.source = {'commit': '1' * 40, 'git_tree': '2' * 40, 'state': 'clean',
                       'patch_sha256': None, 'snapshot_sha256': '3' * 64,
                       'file_count': 1, 'files': []}
        self.config = gate.configuration('linux', 'x86_64', '2.0.0-alpha.8',
                                         'x86_64-unknown-linux-gnu', 'release', 'a' * 64, 'b' * 64)
        self.identity = gate.identity(self.source, self.config)

    def pe_fixture(self, record=b'public identity'):
        import struct
        image = bytearray(0x3000)
        image[:2] = b'MZ'; struct.pack_into('<I', image, 60, 128)
        image[128:132] = b'PE\0\0'
        struct.pack_into('<HH', image, 132, 0x8664, 1)
        struct.pack_into('<H', image, 148, 240)
        struct.pack_into('<H', image, 152, 0x20b)
        struct.pack_into('<II', image, 208, len(image), 512)
        section = 128 + 24 + 240
        image[section:section + 8] = b'.rpbuild'
        struct.pack_into('<II', image, section + 8, len(record) + 16, 0x1000)
        struct.pack_into('<I', image, section + 36, 0x40000040)
        image[0x1000:0x1000 + len(record)] = record
        return image, section

    def test_pe_parser_reads_only_headers_and_public_identity(self):
        from windows_process_identity import verify_identity_section
        image, _ = self.pe_fixture(); reads = []
        def read(address, size):
            reads.append((address, size)); return bytes(image[address:address + size])
        result = verify_identity_section(read, b'public identity')
        self.assertEqual(result['identity_section_rva'], 0x1000)
        self.assertEqual([a for a, _ in reads], [0, 128, 152, 392, 0x1000])
        self.assertTrue(all(0 < n <= 65536 for _, n in reads))

    def test_pe_mutable_executable_missing_duplicate_or_unbounded_section_fails(self):
        from windows_process_identity import verify_identity_section
        import struct
        for kind in ('write', 'execute', 'missing', 'outside', 'too_large', 'wrong_bytes', 'extra_bytes',
                     'too_many_sections', 'bad_optional', 'duplicate'):
            image, section = self.pe_fixture()
            if kind == 'write': struct.pack_into('<I', image, section + 36, 0xc0000040)
            if kind == 'execute': struct.pack_into('<I', image, section + 36, 0x60000040)
            if kind == 'missing': image[section:section + 8] = b'.missing'
            if kind == 'outside': struct.pack_into('<I', image, section + 12, 0x3000)
            if kind == 'too_large': struct.pack_into('<I', image, section + 8, 65537)
            if kind == 'wrong_bytes': image[0x1000] = 0
            if kind == 'extra_bytes': image[0x1000 + len(b'public identity')] = 1
            if kind == 'too_many_sections': struct.pack_into('<H', image, 134, 97)
            if kind == 'bad_optional': struct.pack_into('<H', image, 152, 0x10b)
            if kind == 'duplicate':
                struct.pack_into('<H', image, 134, 2)
                image[section + 40:section + 80] = image[section:section + 40]
            with self.subTest(kind=kind), self.assertRaises(RuntimeError):
                verify_identity_section(lambda a, n: bytes(image[a:a + n]), b'public identity')

    def test_wrong_platform_windows_process_is_rejected_before_api_access(self):
        import windows_process_identity as native
        with patch.object(native.sys, 'platform', 'darwin'):
            with self.assertRaises(RuntimeError): native.observe(1, self.root / 'app', b'public identity')

    def test_compiled_module_uses_exact_bytes_and_immutable_native_section(self):
        text = gate.compiled_module(self.identity).decode()
        payload = text.split(' = [', 1)[1].split('];', 1)[0]
        raw = bytes(int(v) for v in payload.split(','))
        self.assertEqual(gate.json_bytes(raw), self.identity)
        self.assertEqual(raw, json.dumps(self.identity, sort_keys=True, indent=2).encode() + b'\n')
        self.assertIn('unsafe(link_section = ".rpbuild")', text)
        self.assertIn('unsafe(link_section = "__TEXT,__rpbuild")', text)

    def test_source_change_after_build_never_mints_receipt(self):
        changed = deepcopy(self.source); changed['snapshot_sha256'] = 'f' * 64
        repo = self.root / 'source'; repo.mkdir()
        target = self.root / 'new-target'
        with patch.object(gate, 'source_snapshot', side_effect=[self.source, changed]), \
             patch.object(gate, 'toolchain_digest', return_value='a' * 64), \
             patch.object(gate, 'environment_digest', return_value='b' * 64), \
             patch.object(gate, 'build_environment', return_value={}), \
             patch.object(gate.shutil, 'disk_usage', return_value=type('Disk', (), {'free': 8 * 1024 ** 3})()), \
             patch.object(gate.subprocess, 'run', return_value=type('Exit', (), {'returncode': 0})()) as execute, \
             patch.object(gate, 'product_info') as metadata:
            with self.assertRaises(gate.IdentityRejected):
                gate.observed_build(repo, self.identity, target)
        metadata.assert_not_called()
        self.assertEqual(execute.call_args.args[0][0:2], ['cargo', 'build'])
        self.assertIn('REMOTEPLAY_BUILD_IDENTITY_RS', execute.call_args.kwargs['env'])

    def test_macos_lifecycle_metadata_and_launch_role_are_required(self):
        config = gate.configuration('macos', 'aarch64', '2.0.0-alpha.8',
                                    'aarch64-apple-darwin', 'release', 'a' * 64, 'b' * 64)
        expected = gate.identity(self.source, config)
        binary = self.root / 'fixture'; binary.write_bytes(b'not executed')
        info = dict(schema=1, product='RemotePlay', version=config['version'], platform='macos',
                    architecture='aarch64', default_gui=gate.RENDERER, original_gui_compiled=True,
                    native_video_compiled=True, build_identity=expected, separated_lifecycle=True,
                    frontend_entry='--gui', background_entry='--background-service')
        for key, value in ((None, None), ('separated_lifecycle', False), ('frontend_entry', None),
                           ('background_entry', '--gui')):
            candidate = deepcopy(info)
            if key: candidate[key] = value
            completed = type('Exit', (), {'returncode': 0, 'stdout': json.dumps(candidate).encode()})()
            with patch.object(gate.subprocess, 'run', return_value=completed):
                if key:
                    with self.assertRaises(gate.IdentityRejected):
                        gate.product_info(binary, expected, gate.hash_file(binary))
                else:
                    self.assertEqual(gate.product_info(binary, expected, gate.hash_file(binary)), info)

    def test_native_signing_rejection_is_reported_without_raw_diagnostics(self):
        from macos_release_guard import ReleaseRejected
        with patch('macos_release_guard.verify_app', side_effect=ReleaseRejected('private fixture diagnostic')):
            with self.assertRaises(gate.IdentityRejected) as caught:
                gate.authorized_mac_app(self.root)
        self.assertNotIn('private fixture diagnostic', str(caught.exception))
        self.assertEqual(str(caught.exception), 'Native authorized macOS signature verification failed')

    def test_unverified_or_denied_macos_observer_has_no_fallback(self):
        binary = self.root / 'fixture'; binary.write_bytes(b'app')
        observer = self.root / 'observer'; observer.write_bytes(b'observer')
        with patch.object(gate.sys, 'platform', 'darwin'), patch.object(gate.subprocess, 'run') as execute:
            with self.assertRaises(gate.IdentityRejected):
                gate.verify_running_macos(1, binary, gate.hash_file(binary), observer, 'f' * 64)
            execute.assert_not_called()
            execute.return_value = type('Exit', (), {'returncode': 1, 'stdout': b''})()
            with self.assertRaises(gate.IdentityRejected):
                gate.verify_running_macos(1, binary, gate.hash_file(binary), observer, gate.hash_file(observer))
            self.assertEqual(execute.call_count, 1)

    @unittest.skipUnless(sys.platform == 'darwin', 'Native macOS observer requires libproc')
    def test_native_macos_owned_non_gui_image_then_changed_file_fails(self):
        observer = self.root / 'observer'
        native_source = Path(gate.__file__).with_name('native_process_identity.c')
        subprocess.run(['/usr/bin/cc', '-std=c11', '-Wall', '-Wextra', '-Werror',
                        str(native_source), '-o', str(observer), '-lproc'], check=True, capture_output=True)
        source = self.root / 'fixture.c'; source.write_text('#include <unistd.h>\nint main(void) { sleep(15); return 0; }\n')
        executable = self.root / 'fixture'
        subprocess.run(['/usr/bin/cc', str(source), '-o', str(executable)], check=True, capture_output=True)
        observer_hash = gate.hash_file(observer)
        with subprocess.Popen([str(executable)]) as process:
            try:
                result = gate.verify_running_macos(process.pid, executable, gate.hash_file(executable),
                                                   observer, observer_hash)
                self.assertTrue(result['actual_loaded_image_verified'])
                self.assertFalse(result['release_authorized'])
                os.utime(executable, None)
                with self.assertRaises(gate.IdentityRejected):
                    gate.verify_running_macos(process.pid, executable, gate.hash_file(executable),
                                               observer, observer_hash)
            finally:
                process.terminate(); process.wait(timeout=5)

if __name__ == '__main__':
    unittest.main()
