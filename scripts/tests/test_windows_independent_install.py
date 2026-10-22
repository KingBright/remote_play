"""Synthetic package and filesystem fixtures. No real apps, profiles or GUI."""
import contextlib
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import install_windows_independent as install
import install_native_first_identity as original
import build_identity_gate as gate
import desktop_identity as desktop


class IndependentWindowsTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.home = self.root/'home'
        self.home.mkdir()
        self.old = self.home/'AppData/Local/RemotePlay'
        self.old.mkdir(parents=True)
        (self.old/'retained').write_text('old-client-untouched')
        self.candidate = self.root/'payload/Weavelo'
        (self.candidate/'bin').mkdir(parents=True)
        data = {'weavelo.exe':b'synthetic-executable', 'bin/ffmpeg.exe':b'synthetic-media',
                'Weavelo.ico':b'synthetic-icon', 'ffmpeg-license.txt':b'synthetic-license',
                'application-identity.json':json.dumps({'name':'Weavelo','appUserModelId':desktop.identity()['application_id'],'icon':'Weavelo.ico'}).encode(),
                'desktop-identity.json':json.dumps(desktop.identity()).encode()}
        for name,value in data.items():
            (self.candidate/name).write_bytes(value)
        self.binary = gate.hash_file(self.candidate/'weavelo.exe')
        self.receipt = {'build_identity':{'source':{'commit':'a'*40},'configuration':{'version':'2.0.0-alpha.9'}}}
        self.receipt_sha = 'b'*64
        info = {'source_commit':'a'*40,'build_identity':self.receipt['build_identity'],
                'build_receipt_sha256':self.receipt_sha,'weavelo_sha256':self.binary,
                'package_version':'2.0.0-alpha.9','ffmpeg_sha256':gate.hash_file(self.candidate/'bin/ffmpeg.exe'),
                'system_ffmpeg_fallback':False}
        (self.candidate/'build-info.json').write_text(json.dumps(info))
        self.archive = self.root/'formal.zip'
        self.archive_payload()
        self.bound = {'candidate':self.candidate, 'archive':self.archive,
                      'receipt':self.root/'receipt.json','receipt_sha256':self.receipt_sha,
                      'binary_sha256':self.binary,'identity_sha256':'c'*64,
                      'commit':'a'*40,'archive_sha256':gate.hash_file(self.archive),
                      'files':original.inventory(self.candidate,gate)}
        self.bound['receipt'].write_text(json.dumps({'build_identity':{},'product_info':{}}))

    def archive_payload(self):
        with zipfile.ZipFile(self.archive,'w') as z:
            for p in self.candidate.rglob('*'):
                if p.is_file():z.write(p,'Weavelo/'+p.relative_to(self.candidate).as_posix())

    def pin(self):
        return {'archive':gate.hash_file(self.archive),'binary':self.binary,'receipt':self.receipt_sha}

    def test_formal_seven_member_package_requires_explicit_independent_scope(self):
        with self.assertRaisesRegex(RuntimeError,'payload differs'):
            original.reviewed_windows_payload(gate,self.candidate,self.archive,self.receipt,self.pin())
        files=original.reviewed_windows_payload(gate,self.candidate,self.archive,self.receipt,self.pin(),independent=True)
        self.assertEqual(len(files),7)

    def test_wrong_desktop_identity_rejected_before_any_destination_write(self):
        path=self.candidate/'desktop-identity.json'
        path.write_text(json.dumps({'application_id':'retained-or-unknown'}))
        self.archive_payload()
        with self.assertRaisesRegex(RuntimeError,'Independent'):
            original.reviewed_windows_payload(gate,self.candidate,self.archive,self.receipt,self.pin(),independent=True)
        self.assertFalse(install.paths(self.home)['app'].exists())

    def test_archive_and_payload_drift_rejected(self):
        (self.candidate/'ffmpeg-license.txt').write_text('changed-after-package')
        with self.assertRaisesRegex(RuntimeError,'Archive/payload'):
            original.reviewed_windows_payload(gate,self.candidate,self.archive,self.receipt,self.pin(),independent=True)

    def test_plan_readonly_and_separate_from_retained_paths(self):
        before=(self.old/'retained').stat()
        value=install.plan(self.home,self.bound)
        self.assertEqual((self.old/'retained').stat(),before)
        self.assertFalse(value['applied'])
        self.assertFalse(value['service_or_GUI_started'])
        self.assertNotEqual(Path(value['paths']['app']),self.old)
        self.assertFalse(install.paths(self.home)['state'].exists())

    def test_each_existing_destination_blocks_replay(self):
        for name in ('app','profile','state','stage'):
            path=install.paths(self.home)[name]
            path.mkdir(parents=True)
            with self.assertRaisesRegex(RuntimeError,'exists'):
                install.plan(self.home,self.bound)
            path.rmdir()

    def test_plan_mismatch_never_creates_transaction(self):
        with self.assertRaisesRegex(RuntimeError,'plan changed'):
            install.apply(self.home,self.bound,'d'*64)
        self.assertFalse(install.paths(self.home)['state'].exists())

    def test_new_install_preserves_old_app_and_blocks_replay(self):
        class SyntheticSecurity:
            def create_private_directory(self,path):
                path.mkdir()
        def move(source,destination):
            if destination.exists():raise FileExistsError(destination)
            source.rename(destination)
        @contextlib.contextmanager
        def fixture():
            with patch.object(install.private,'NativeSecurity',return_value=SyntheticSecurity()), \
                 patch.object(install,'move_no_replace',side_effect=move), \
                 patch.object(install,'revalidate',return_value=self.bound['files']), \
                 patch.object(install.gate,'read_json',return_value={'build_identity':{},'product_info':{}}), \
                 patch.object(install.gate,'product_info',return_value={}):
                yield
        initial=install.plan(self.home,self.bound)
        with fixture():
            result=install.apply(self.home,self.bound,initial['plan_sha256'])
        self.assertEqual(result['phase'],'committed_not_started')
        self.assertEqual((self.old/'retained').read_text(),'old-client-untouched')
        self.assertEqual(original.inventory(install.paths(self.home)['app'],gate),self.bound['files'])
        with self.assertRaisesRegex(RuntimeError,'exists'):
            install.plan(self.home,self.bound)

    @unittest.skipUnless(sys.platform=='win32','Native Windows directory primitive')
    def test_native_move_refuses_existing_destination(self):
        source=self.root/'native-source';source.mkdir();(source/'owned').write_text('fixture')
        destination=self.root/'native-destination';destination.mkdir();(destination/'kept').write_text('preserved')
        with self.assertRaises(OSError):
            install.move_no_replace(source,destination)
        self.assertEqual((destination/'kept').read_text(),'preserved')
        self.assertTrue((source/'owned').exists())

    def test_status_accepts_inherited_private_file_without_repair(self):
        selected=install.paths(self.home)
        selected['state'].mkdir(parents=True)
        record=dict(install.plan(self.home/'another-home',self.bound))
        record.update(schema=1,kind='independent-weavelo-windows-desktop-install',
                      paths={k:str(v) for k,v in selected.items()},phase='moving_new_app')
        (selected['state']/'install.json').write_text(json.dumps(record))
        class SyntheticSecurity:
            user='fixture-owner'
            def snapshot(self,path):
                return {'owner':self.user,'dacl_protected':path.is_dir(),
                        'aces':[{'type':0,'sid':self.user}]}
        with patch.object(install.private,'NativeSecurity',return_value=SyntheticSecurity()):
            self.assertEqual(install.status(self.home),record)
        with self.assertRaisesRegex(RuntimeError,'exists'):
            install.plan(self.home,self.bound)

    def test_status_refuses_foreign_file_grant(self):
        selected=install.paths(self.home);selected['state'].mkdir(parents=True)
        (selected['state']/'install.json').write_text('{}')
        class SyntheticSecurity:
            user='fixture-owner'
            def snapshot(self,path):
                return {'owner':self.user,'dacl_protected':True,
                        'aces':[{'type':0,'sid':'foreign-reader'}]}
        with patch.object(install.private,'NativeSecurity',return_value=SyntheticSecurity()):
            with self.assertRaisesRegex(gate.IdentityRejected,'not private'):
                install.status(self.home)

    @unittest.skipUnless(sys.platform=='win32','Native Windows fresh private profile')
    def test_native_fresh_install_and_private_status_leave_retained_fixture_unchanged(self):
        before=(self.old/'retained').read_bytes()
        initial=install.plan(self.home,self.bound)
        with patch.object(install,'revalidate',return_value=self.bound['files']), \
             patch.object(install.gate,'read_json',return_value={'build_identity':{},'product_info':{}}), \
             patch.object(install.gate,'product_info',return_value={}):
            result=install.apply(self.home,self.bound,initial['plan_sha256'])
        self.assertEqual(install.status(self.home),result)
        native=install.private.NativeSecurity()
        profile=native.snapshot(install.paths(self.home)['profile'])
        self.assertEqual(profile['owner'],native.user)
        self.assertTrue(profile['dacl_protected'])
        self.assertEqual((self.old/'retained').read_bytes(),before)
