from __future__ import annotations
import json
import os
from pathlib import Path
import plistlib
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import macos_release_guard as guard
import install_macos_unified as installer


class ReleaseGateTests(unittest.TestCase):
    def setUp(self):
        self.policy = guard.load_policy()
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def info(self, **changes):
        data = dict(bundle_id=self.policy.bundle_id, certificate_sha1=self.policy.certificate_sha1,
                    designated_requirement=self.policy.requirement, version='2.0.0-alpha.4',
                    build='20260929.4', executable_sha256='abc', info_sha256='def')
        data.update(changes)
        return data

    def archive(self, entries):
        path = self.root / 'candidate.zip'
        with zipfile.ZipFile(path, 'w') as out:
            for name, contents in entries: out.writestr(name, contents)
        return path

    def reject_archive(self, entries):
        with self.assertRaises(guard.ReleaseRejected):
            with guard.verified_archive(self.archive(entries), self.policy): pass

    def test_pinned_identity_is_not_a_friendly_name(self):
        with patch.object(guard, 'run', return_value='1) ' + 'f'*40 + ' "RemotePlay Local"\n'):
            with self.assertRaises(guard.ReleaseRejected): guard.preflight(self.policy)

    def test_missing_signer_fails_before_touching_artifact(self):
        output = self.root / 'output'; output.mkdir(); old = output / 'old.zip'; old.write_bytes(b'keep')
        with patch.object(guard, 'run', return_value='0 valid identities found'):
            with self.assertRaises(guard.ReleaseRejected):
                guard.package(self.root/'binary', output, '2.0.0-alpha.4', '20260929.4', self.policy)
        self.assertEqual(old.read_bytes(), b'keep')
        self.assertEqual(list(output.iterdir()), [old])

    def test_exact_signer_fingerprint_works(self):
        with patch.object(guard, 'run', return_value=' 1) ' + self.policy.certificate_sha1.upper() + ' "RemotePlay Local"\n'):
            self.assertEqual(guard.preflight(self.policy), self.policy.certificate_sha1.upper())

    def test_invalid_policy_rejected(self):
        data = json.loads(guard.POLICY_FILE.read_text()); data['certificate_sha1'] = 'RemotePlay Local'
        path=self.root/'policy.json'; path.write_text(json.dumps(data))
        with self.assertRaises(guard.ReleaseRejected): guard.load_policy(path)

    def test_numerical_alpha_version_order(self):
        self.assertGreater(guard.version_key('2.0.0-alpha.10'), guard.version_key('2.0.0-alpha.4'))
        self.assertGreater(guard.version_key('2.0.0'), guard.version_key('2.0.0-alpha.999'))

    def test_same_build_is_noop(self):
        self.assertEqual(guard.check_upgrade(self.info(), self.info()), 'already_installed')

    def test_same_version_different_executable_rejected(self):
        with self.assertRaises(guard.ReleaseRejected): guard.check_upgrade(self.info(),self.info(executable_sha256='other'))

    def test_same_version_different_plist_rejected(self):
        with self.assertRaises(guard.ReleaseRejected): guard.check_upgrade(self.info(),self.info(info_sha256='other'))

    def test_old_release_rejected(self):
        with self.assertRaises(guard.ReleaseRejected): guard.check_upgrade(self.info(),self.info(version='2.0.0-alpha.1',build='20261001.1'))

    def test_build_rollback_rejected(self):
        with self.assertRaises(guard.ReleaseRejected): guard.check_upgrade(self.info(),self.info(version='2.0.0-alpha.5',build='20260924.1'))

    def test_valid_upgrade_keeps_identity(self):
        self.assertEqual(guard.check_upgrade(self.info(),self.info(version='2.0.0-alpha.5',build='20260929.5',executable_sha256='new')), 'compatible_upgrade')

    def test_identity_changes_rejected(self):
        for field in ['bundle_id','certificate_sha1','designated_requirement']:
            with self.subTest(field=field), self.assertRaises(guard.ReleaseRejected):
                guard.check_upgrade(self.info(), self.info(**{field:'other'}))

    def test_requirement_parser_checks_real_designated_line(self):
        self.assertEqual(guard.designated_requirement('Executable=/tmp/app\n# designated => '+self.policy.requirement+'\n'), self.policy.requirement)
        with self.assertRaises(guard.ReleaseRejected): guard.designated_requirement('Identifier=com.remoteplay.unified')

    def test_zip_traversal_rejected(self): self.reject_archive([('../outside',b'x')])
    def test_zip_absolute_rejected(self): self.reject_archive([('/tmp/outside',b'x')])
    def test_zip_backslash_rejected(self): self.reject_archive([('RemotePlay.app\\..\\bad',b'x')])
    def test_zip_collision_rejected(self): self.reject_archive([('RemotePlay.app/A',b'x'),('remoteplay.app/a',b'y')])
    def test_zip_extra_app_rejected(self): self.reject_archive([('Other.app/Contents/Info.plist',b'x')])
    def test_zip_symlink_rejected(self):
        entry=zipfile.ZipInfo('RemotePlay.app/link');entry.create_system=3;entry.external_attr=(stat.S_IFLNK|0o777)<<16
        self.reject_archive([(entry,b'/Applications')])

    def test_stage_hash_is_not_accepted_as_signature(self):
        archive=self.archive([('RemotePlay.app/invalid',b'not signed')])
        (self.root/'desktop-releases.json').write_text(json.dumps({'releases':[dict(file='candidate-macos.zip',version='2.0.0-alpha.4',sha256=guard.sha256(archive),size_bytes=archive.stat().st_size)]}))
        archive.rename(self.root/'candidate-macos.zip')
        with self.assertRaises(guard.ReleaseRejected): guard.verify_stage(self.root,self.policy)

    def test_loaded_wrong_service_rejected(self):
        path=self.root/'agent.plist';exe=self.root/'RemotePlay.app/Contents/MacOS/remote_play'
        path.write_bytes(plistlib.dumps({'Label':'com.remoteplay.host','ProgramArguments':['/tmp/other']}))
        with self.assertRaises(guard.ReleaseRejected): installer.validate_service(path,exe,{'loaded':True,'program':str(exe)})

    def test_publish_guard_precedes_network(self):
        s=(guard.ROOT/'scripts/publish_desktop_nas.sh').read_text()
        self.assertLess(s.index('macos_release_guard.py'),s.index('scp -O'))

    def test_package_guard_precedes_build(self):
        s=(guard.ROOT/'scripts/package_macos_unified.sh').read_text()
        self.assertLess(s.index('macos_release_guard.py'),s.index('cargo build'))
        self.assertNotIn('Signing ad-hoc',s)


class UpgradeTransactionTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        root=Path(self.temp.name);self.old=root/'installed';self.new=root/'candidate';self.backup=root/'backup'
        self.old.mkdir();(self.old/'content').write_text('old')
        self.new.mkdir();(self.new/'content').write_text('new')
        self.calls=[]
    def stop(self): self.calls.append('stop')
    def start(self): self.calls.append('start')
    def test_bad_candidate_never_stops_service(self):
        def invalid(_):raise guard.ReleaseRejected('bad signature')
        with self.assertRaises(guard.ReleaseRejected):
            installer.replace_with_rollback(self.new,self.old,self.backup,invalid,self.stop,self.start,lambda:{})
        self.assertEqual(self.calls,[]);self.assertEqual((self.old/'content').read_text(),'old')
    def test_success_keeps_backup(self):
        result=installer.replace_with_rollback(self.new,self.old,self.backup,lambda _:None,self.stop,self.start,lambda:{'running':True})
        self.assertTrue(result['running']);self.assertEqual((self.backup/'content').read_text(),'old')
        self.assertEqual((self.old/'content').read_text(),'new')
    def test_failed_health_rolls_back(self):
        def health():raise guard.ReleaseRejected('not running')
        with self.assertRaisesRegex(guard.ReleaseRejected,'previous app was restored'):
            installer.replace_with_rollback(self.new,self.old,self.backup,lambda _:None,self.stop,self.start,health)
        self.assertEqual((self.old/'content').read_text(),'old');self.assertEqual(self.calls,['stop','start','stop','start'])
    def test_failed_start_rolls_back(self):
        def start():
            self.calls.append('start')
            if self.calls.count('start')==1:raise guard.ReleaseRejected('new service failed')
        with self.assertRaises(guard.ReleaseRejected):
            installer.replace_with_rollback(self.new,self.old,self.backup,lambda _:None,self.stop,start,lambda:{})
        self.assertEqual((self.old/'content').read_text(),'old')
    def test_failed_installed_verification_rolls_back(self):
        def verify(path):
            if path==self.old:raise guard.ReleaseRejected('installed checksum changed')
        with self.assertRaises(guard.ReleaseRejected):
            installer.replace_with_rollback(self.new,self.old,self.backup,verify,self.stop,self.start,lambda:{})
        self.assertEqual((self.old/'content').read_text(),'old')


class PreservedConfigurationTests(unittest.TestCase):
    def test_restarted_activation_endpoint_is_not_network_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            home=Path(directory);mesh=home/'Library/Application Support/RemotePlay/NativeMesh';mesh.mkdir(parents=True)
            (mesh/'mesh.conf').write_text('synthetic-device')
            (mesh/'mesh.secret').write_text('synthetic-test-key')
            (mesh/'desktop-instance.addr').write_text('127.0.0.1:30001')
            before=installer.preserve_hashes(home,home/'not-present.plist')
            (mesh/'desktop-instance.addr').write_text('127.0.0.1:30002')
            (mesh/'desktop-instance.lock').write_text('runtime')
            self.assertEqual(before,installer.preserve_hashes(home,home/'not-present.plist'))
    def test_persistent_secret_still_detects_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            home=Path(directory);mesh=home/'Library/Application Support/RemotePlay/NativeMesh';mesh.mkdir(parents=True)
            (mesh/'mesh.secret').write_text('synthetic-old')
            before=installer.preserve_hashes(home,home/'not-present.plist')
            (mesh/'mesh.secret').write_text('synthetic-new')
            self.assertNotEqual(before,installer.preserve_hashes(home,home/'not-present.plist'))
    def test_unknown_persistent_config_remains_protected(self):
        with tempfile.TemporaryDirectory() as directory:
            home=Path(directory);mesh=home/'Library/Application Support/RemotePlay/NativeMesh';mesh.mkdir(parents=True)
            (mesh/'settings.json').write_text('{}')
            before=installer.preserve_hashes(home,home/'not-present.plist')
            (mesh/'settings.json').write_text('{"new":true}')
            self.assertNotEqual(before,installer.preserve_hashes(home,home/'not-present.plist'))


@unittest.skipUnless(sys.platform=='darwin' and os.environ.get('RP_SIGNING_INTEGRATION')=='1','explicit macOS integration run only')
class NativeSignatureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp=tempfile.TemporaryDirectory();cls.root=Path(cls.temp.name);cls.policy=guard.load_policy()
        source=cls.root/'fixture.c';source.write_text('int main(void) { return 0; }\n')
        guard.run(['/usr/bin/clang',str(source),'-o',str(cls.root/'fixture')])
        cls.report=guard.package(cls.root/'fixture',cls.root/'out','2.0.0-alpha.99','29991231.1',cls.policy)
        cls.archive=Path(cls.report['archive'])
    @classmethod
    def tearDownClass(cls):cls.temp.cleanup()
    def test_real_signed_roundtrip(self):
        with guard.verified_archive(self.archive,self.policy) as (_,report):
            self.assertTrue(report['signature_verified']);self.assertEqual(report['certificate_sha1'],self.policy.certificate_sha1)
    def test_real_ad_hoc_replacement_is_rejected(self):
        with guard.verified_archive(self.archive,self.policy) as (app,_):
            guard.run(['/usr/bin/codesign','--force','--sign','-',str(app)])
            with self.assertRaises(guard.ReleaseRejected):guard.verify_app(app,self.policy)
    def test_real_signed_resource_tamper_is_rejected(self):
        with guard.verified_archive(self.archive,self.policy) as (app,_):
            (app/'Contents/Resources/unapproved.txt').write_text('modified after signing')
            with self.assertRaises(guard.ReleaseRejected):guard.verify_app(app,self.policy)
    def test_published_good_archive_passes_same_policy(self):
        path=guard.ROOT.parent/'Workspace/.rp-four-gui-20260929/artifacts/RemotePlay-macos-arm64-2.0.0-alpha.4.zip'
        with guard.verified_archive(path,self.policy) as (_,report):self.assertEqual(report['version'],'2.0.0-alpha.4')
    def test_rehashed_ad_hoc_archive_is_rejected_before_publication(self):
        with guard.verified_archive(self.archive,self.policy) as (app,_):
            guard.run(['/usr/bin/codesign','--force','--sign','-',str(app)])
            with tempfile.TemporaryDirectory() as directory:
                stage=Path(directory);bad=stage/'RemotePlay-macos-bad.zip'
                guard.run(['/usr/bin/ditto','-c','-k','--sequesterRsrc','--keepParent',str(app),str(bad)])
                metadata={'releases':[{'file':bad.name,'version':'2.0.0-alpha.99',
                    'sha256':guard.sha256(bad),'size_bytes':bad.stat().st_size}]}
                (stage/'desktop-releases.json').write_text(json.dumps(metadata))
                with self.assertRaises(guard.ReleaseRejected):guard.verify_stage(stage,self.policy)

    def test_second_versioned_package_cannot_overwrite_first(self):
        before=guard.sha256(self.archive)
        with self.assertRaises(guard.ReleaseRejected):
            guard.package(self.root/'fixture',self.root/'out','2.0.0-alpha.99','29991231.1',self.policy)
        self.assertEqual(before,guard.sha256(self.archive))


if __name__=='__main__':unittest.main()
