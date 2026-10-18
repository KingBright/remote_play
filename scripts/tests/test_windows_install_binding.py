"""Original package binding and automatic rollback; no native owner/ACL operations."""
import hashlib
import json
import sys
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_identity_gate as g
import install_native_first_identity as m


class WindowsBindingTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(); self.addCleanup(tmp.cleanup)
        self.repo = Path(tmp.name).resolve()
        self.candidate = self.repo/'target/package/windows/Weavelo'; self.candidate.mkdir(parents=True)
        self.binary = self.repo/'target/release/weavelo.exe'; self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b'fixture compiler, never executed')
        self.value = g.hash_file(self.binary)
        source = {'commit':'1'*40, 'git_tree':'2'*40, 'state':'clean', 'patch_sha256':None,
                  'snapshot_sha256':'3'*64, 'file_count':1}
        config = g.configuration('windows', 'x86_64', '2.0.0-alpha.9', 'x86_64-pc-windows-msvc',
                                 'release', '4'*64, '5'*64)
        self.identity = g.identity(source, config)
        self.info = {'schema':1, 'product':'Weavelo', 'version':config['version'], 'platform':'windows',
                     'architecture':'x86_64', 'default_gui':g.RENDERER, 'original_gui_compiled':True,
                     'native_video_compiled':True, 'build_identity':self.identity,
                     'separated_lifecycle':False, 'frontend_entry':None, 'background_entry':None}
        self.receipt = {'schema':2, 'kind':'observed-build', 'build_identity':self.identity,
                        'compiler_binary_sha256':self.value, 'compiler_binary_bytes':self.binary.stat().st_size,
                        'product_info':self.info}
        self.receipt_path = self.candidate.parent/'windows-current-build-receipt.json'
        self.archive = self.candidate.parent/'Weavelo-Windows-x64.zip'
        self.old = self.repo/'old/remote_play.exe'; self.old.parent.mkdir(); self.old.write_bytes(b'old retained image')
        self.args = SimpleNamespace(build_receipt=self.receipt_path, build_receipt_sha256=None,
            release_archive=self.archive, archive_sha256=None, expected_commit=source['commit'],
            identity_sha256=self.identity['identity_sha256'], compiler_binary=self.binary,
            old_binary_sha256=g.hash_file(self.old))
        self.write_receipt()
        for name, data in {'weavelo.exe':self.binary.read_bytes(), 'application-identity.json':b'{}',
                           'ffmpeg-license.txt':b'fixture license', 'Weavelo.ico':b'fixture icon',
                           'bin/ffmpeg.exe':b'fixture private ffmpeg'}.items():
            p = self.candidate/name; p.parent.mkdir(exist_ok=True); p.write_bytes(data)
        self.metadata = {'source_commit':source['commit'], 'build_identity':self.identity,
            'build_receipt_sha256':self.args.build_receipt_sha256, 'weavelo_sha256':self.value,
            'package_version':config['version'], 'ffmpeg_sha256':g.hash_file(self.candidate/'bin/ffmpeg.exe'),
            'system_ffmpeg_fallback':False}
        self.write_package()
        # Only executable launch is replaced. Hashes, canonical identity validation,
        # ZIP CRC/member checks, JSON documents and rollback filesystem stay real.
        self.metadata_probe = patch.object(g, 'product_info', side_effect=self.product_info)
        self.metadata_probe.start(); self.addCleanup(self.metadata_probe.stop)

    def write_receipt(self):
        self.receipt_path.write_text(json.dumps(self.receipt))
        self.args.build_receipt_sha256 = g.hash_file(self.receipt_path)

    def write_package(self, extra=None):
        (self.candidate/'build-info.json').write_text(json.dumps(self.metadata))
        with zipfile.ZipFile(self.archive, 'w') as z:
            for p in self.candidate.rglob('*'):
                if p.is_file(): z.write(p, 'Weavelo/'+p.relative_to(self.candidate).as_posix())
            if extra: z.writestr(*extra)
        self.args.archive_sha256 = g.hash_file(self.archive)

    def product_info(self, path, expected, value):
        g.require(g.hash_file(path) == value, 'Unexpected binary')
        g.validate_product_info(self.info, expected)
        return self.info

    def binding(self):
        return m.reviewed_windows_binding(g, self.args, self.repo)

    def installer(self):
        binding = self.binding(); install = m.NativeInstall.__new__(m.NativeInstall)
        install.windows = True; install.platform = 'windows'; install.repo = self.repo; install.gate = g
        install.pin = binding['pin']; install.commit = binding['commit']
        install.receipt = binding['receipt']; install.compiler = binding['compiler']
        install.archive = binding['archive']; install.candidate = binding['candidate']
        install.live = self.repo/'live'; install.exe = install.live/'weavelo.exe'
        install.old_exe = self.old; install.old_app = self.old.parent; install.old_profiles = []
        install.stage = self.repo/'stage'; install.work = self.repo/'transaction'
        install.profile = self.repo/'profile/Mesh'; install.journal = install.work/'journal.json'
        install.shortcut = self.repo/'Weavelo.lnk'; install.old_pid = Mock(return_value=11)
        install.native = Mock(return_value={'verified':True})
        install.require_idle = Mock(return_value={'idle_verified':True})
        install.protected = Mock(return_value={})
        return install

    def test_explicit_nonhistorical_producer_binding_and_complete_payload(self):
        binding = self.binding()
        self.assertEqual(binding['commit'], '1'*40)
        self.assertEqual(binding['pin'], dict(binary=self.value, old=g.hash_file(self.old),
            identity=self.identity['identity_sha256'], receipt=self.args.build_receipt_sha256,
            archive=self.args.archive_sha256))
        self.assertEqual(len(binding['payload']), 6)
        self.assertFalse(hasattr(m, 'PINS')); self.assertFalse(hasattr(m, 'COMMIT'))

    def test_no_historical_default_or_incomplete_arguments(self):
        with self.assertRaisesRegex(RuntimeError, 'requires reviewed'):
            m.reviewed_windows_binding(g, None, self.repo)
        self.args.archive_sha256 = None
        with self.assertRaisesRegex(RuntimeError, 'requires reviewed'): self.binding()

    def test_wrong_receipt_hash_rejected(self):
        self.args.build_receipt_sha256 = '0'*64
        with self.assertRaises(g.IdentityRejected): self.binding()

    def test_relabelled_commit_or_identity_rejected(self):
        for name, value in [('expected_commit','9'*40), ('identity_sha256','9'*64)]:
            with self.subTest(name=name):
                before = getattr(self.args, name); setattr(self.args, name, value)
                with self.assertRaises(g.IdentityRejected): self.binding()
                setattr(self.args, name, before)

    def test_pending_receipt_rejected_even_with_correct_file_hash(self):
        self.receipt['kind'] = 'compiled-build-awaiting-native'; self.write_receipt()
        with self.assertRaisesRegex(RuntimeError, 'observed producer'): self.binding()

    def test_wrong_archive_pin_or_compiler_bytes_rejected(self):
        before = self.args.archive_sha256; self.args.archive_sha256 = '0'*64
        with self.assertRaisesRegex(RuntimeError, 'archive drift'): self.binding()
        self.args.archive_sha256 = before; self.binary.write_bytes(b'other compiler')
        with self.assertRaisesRegex(RuntimeError, 'Compiler size'): self.binding()

    def test_unrelated_receipt_path_rejected(self):
        self.args.build_receipt = self.repo/'elsewhere.json'
        with self.assertRaisesRegex(RuntimeError, 'original Windows'): self.binding()

    def test_build_info_cannot_relabel_source_receipt_or_binary(self):
        for key, value in [('source_commit','9'*40), ('build_receipt_sha256','9'*64),
                           ('weavelo_sha256','9'*64), ('ffmpeg_sha256','9'*64),
                           ('package_version','2.0.0-alpha.99'), ('system_ffmpeg_fallback',True)]:
            with self.subTest(key=key):
                before = self.metadata[key]; self.metadata[key] = value; self.write_package()
                with self.assertRaisesRegex(RuntimeError, 'provenance'): self.binding()
                self.metadata[key] = before

    def test_changed_expanded_payload_rejected(self):
        (self.candidate/'Weavelo.ico').write_bytes(b'changed icon')
        with self.assertRaisesRegex(RuntimeError, 'Archive/payload'): self.binding()

    def test_duplicate_case_collision_or_extra_archive_member_rejected(self):
        for name in ['Weavelo/weavelo.exe', 'Weavelo/WEAVELO.EXE', 'Weavelo/../outside']:
            with self.subTest(name=name):
                self.write_package((name,b'unreviewed'))
                with self.assertRaisesRegex(RuntimeError, 'archive member'): self.binding()

    def test_old_hash_must_be_complete_and_plan_records_both_images(self):
        self.args.old_binary_sha256 = 'short'
        with self.assertRaises(g.IdentityRejected): self.binding()
        self.args.old_binary_sha256 = g.hash_file(self.old)
        install = self.installer(); plan = install.plan()
        self.assertEqual(plan['source_commit'], self.identity['source']['commit'])
        self.assertEqual(plan['receipt_sha256'], self.args.build_receipt_sha256)
        self.assertEqual(plan['archive_sha256'], self.args.archive_sha256)
        self.assertEqual(plan['old_binary_sha256'], g.hash_file(self.old))
        self.assertEqual(plan['new_binary_sha256'], self.value)
        self.assertEqual(plan['payload'], m.inventory(self.candidate,g))
        install.native.assert_called_once_with(11,self.old,g.hash_file(self.old))

    def test_automatic_rollback_uses_reviewed_old_image_and_keeps_new_payload(self):
        install = self.installer(); plan = install.plan()
        new = Mock(pid=22); new.poll.return_value = None; old = Mock(pid=33)
        install.launch_windows = Mock(side_effect=[new,old])
        def private_fixture(path):
            path.mkdir(parents=True)
            for name in ('mesh.conf','mesh.secret'): (path/name).write_bytes(b'fixture only')
            return {'file_identity':{},'owner':'fixture','dacl_protected':True,'sddl':'fixture'}
        def command(args, **kwargs):
            if args[0] == str(install.exe): return SimpleNamespace(returncode=0,stdout='{"ok":true}')
            raise RuntimeError('fixture shortcut failure after verified startup')
        with patch('windows_private_profile.NativeSecurity') as security, \
             patch('windows_process_lifecycle.stop_verified') as stop, \
             patch.object(m.subprocess,'run',side_effect=command), patch.object(m.time,'sleep'):
            security.return_value.create_private_directory.side_effect = private_fixture
            with self.assertRaisesRegex(RuntimeError, 'observe original private journal'):
                install.apply(m.digest(plan))
            stop.assert_called_once_with(install,11,self.old,install.pin['old'])
        self.assertEqual(install.launch_windows.call_args_list[-1].kwargs, {'old':True})
        self.assertEqual(install.native.call_args.args, (33,self.old,install.pin['old']))
        new.terminate.assert_called_once(); self.assertEqual(g.hash_file(self.old),install.pin['old'])
        state = json.loads(install.journal.read_text())
        self.assertEqual(state['phase'],'rolled_back_old_owner_verified_new_files_retained')
        self.assertEqual(state['plan']['archive_sha256'],self.args.archive_sha256)
        self.assertEqual(state['plan']['payload'],m.inventory(install.live,g))
        self.assertEqual(state['plan']['receipt_sha256'],self.args.build_receipt_sha256)


if __name__ == '__main__': unittest.main()
