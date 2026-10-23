"""Owned directory fixtures. Synthetic ELF metadata is never physical acceptance."""
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import build_identity_gate as gate
import desktop_identity as desktop
import install_linux_independent as install
import package_linux_desktop as package
import release_artifact_lifecycle as lifecycle


class IndependentLinuxTests(unittest.TestCase):
    def setUp(self):
        tmp=tempfile.TemporaryDirectory(prefix='weavelo-independent-fixture-')
        self.addCleanup(tmp.cleanup);self.root=Path(tmp.name).resolve()
        self.home=self.root/'home';self.home.mkdir(mode=0o700)
        self.binary=self.root/'binary';self.binary.write_bytes(b'fixture-only-not-a-product')
        self.commit='b'*40
        config=gate.configuration('linux','x86_64','2.0.0-alpha.9','x86_64-unknown-linux-gnu',
            'release','c'*64,'d'*64)
        self.expected=gate.identity({'commit':self.commit,'git_tree':'e'*40,'state':'clean',
            'patch_sha256':None,'snapshot_sha256':'a'*64,'file_count':1},config)
        self.info={'schema':1,'product':'Weavelo','platform':'linux','architecture':'x86_64',
            'version':'2.0.0-alpha.9','default_gui':'restored-original-gpui','original_gui_compiled':True,
            'native_video_compiled':True,'build_identity':self.expected,'separated_lifecycle':True,
            'background_entry':'--background-service','frontend_entry':'--gui',
            'desktop_identity':desktop.identity(),'desktop_runtime':{'control_port':39273,
                'discovery_port':38119,'implicit_relay':False,'implicit_p2p':False}}
        self.receipt={'schema':2,'kind':'observed-build','build_identity':self.expected,
            'compiler_binary_sha256':gate.hash_file(self.binary),'fresh_target':False,
            'build_storage':{'mode':'shared-incremental','source_directory':str(self.root),
                'target_directory':str(self.root/'target')},'product_info':self.info}
        self.stage=self.root/'stage';self.stage.mkdir()
        with patch.object(gate,'product_info',return_value=self.info):
            self.sealed=package.assemble(self.stage,self.binary,self.receipt,self.commit,self.expected['identity_sha256'])
        self.zip=self.root/'package.zip';package.archive(self.stage,self.zip)

    def plan(self):
        return install.plan(self.home,self.zip,gate.hash_file(self.zip),self.sealed['manifest_sha256'],
            self.commit,self.expected['identity_sha256'])

    def reseal(self):
        (self.stage/gate.MANIFEST).unlink()
        with patch.object(gate,'product_info',return_value=self.info):
            self.sealed=gate.seal_tree(self.stage,self.receipt,self.commit,self.expected['identity_sha256'])
        self.zip.unlink();package.archive(self.stage,self.zip)

    def test_real_archive_verification_and_default_plan_write_nothing(self):
        before=list(self.home.iterdir());reviewed=self.plan()
        self.assertEqual(list(self.home.iterdir()),before)
        self.assertFalse(reviewed['service_started']);self.assertFalse(reviewed['pairing_imported'])
        self.assertEqual(reviewed['paths']['profile'],str(self.home/'.config/weavelo/desktop'))
        with zipfile.ZipFile(self.zip) as source:
            self.assertEqual(len(source.namelist()),len(install.PAYLOAD)+1)

    def test_missing_parameters_fail_before_installer_or_packager_work(self):
        for script,args in [('install_linux_independent.py',[]),('install_linux_independent.py',['--apply']),
                            ('package_linux_desktop.py',[])]:
            done=subprocess.run([sys.executable,'-B',str(desktop.ROOT/'scripts'/script),*args],
                                capture_output=True,text=True)
            self.assertEqual(done.returncode,2,done.stderr)
        self.assertEqual(list(self.home.iterdir()),[])

    def test_each_fresh_target_collision_is_rejected_without_replacement(self):
        for key in ('app','profile','launcher','service','desktop','icon','work','stage'):
            target=install.paths(self.home)[key];target.parent.mkdir(parents=True,exist_ok=True)
            target.write_bytes(b'preserve this')
            with self.subTest(key=key),self.assertRaisesRegex(ValueError,'collision'):
                self.plan()
            self.assertEqual(target.read_bytes(),b'preserve this');target.unlink()

    def test_linked_ancestor_is_rejected(self):
        destination=self.root/'other';destination.mkdir()
        (self.home/'.local').symlink_to(destination,target_is_directory=True)
        with self.assertRaisesRegex(ValueError,'Linked'):self.plan()
        self.assertEqual(list(destination.iterdir()),[])

    def test_archive_payload_tampering_is_rejected_even_with_fresh_archive_hash(self):
        with zipfile.ZipFile(self.zip,'a') as output:output.writestr('Weavelo/weavelo',b'tampered')
        with self.assertRaises(gate.IdentityRejected):self.plan()

    def test_source_identity_and_pins_are_required(self):
        with self.assertRaisesRegex(ValueError,'SHA256'):
            install.plan(self.home,self.zip,'0'*64,self.sealed['manifest_sha256'],self.commit,self.expected['identity_sha256'])
        with self.assertRaises(gate.IdentityRejected):
            install.plan(self.home,self.zip,gate.hash_file(self.zip),self.sealed['manifest_sha256'],'f'*40,self.expected['identity_sha256'])

    def test_unlisted_profile_content_is_rejected(self):
        p=self.stage/'Weavelo/private-profile/config.json';p.parent.mkdir();p.write_text('{}')
        self.reseal()
        with self.assertRaisesRegex(ValueError,'Unexpected independent package files'):self.plan()

    def test_retained_service_profile_is_rejected(self):
        p=self.stage/'Weavelo/deploy/linux/weavelo-desktop.service'
        p.write_text(p.read_text().replace('/weavelo/desktop','/weavelo/mesh'))
        self.reseal()
        with self.assertRaisesRegex(ValueError,'service profile'):self.plan()

    def test_conflicting_inherited_configuration_cannot_select_retained_profile(self):
        p=install.paths(self.home);p['app'].mkdir(parents=True)
        fixture=p['app']/'weavelo'
        fixture.write_text('#!'+sys.executable+'\nimport os,json;print(json.dumps({k:os.environ[k] for k in '
            '["WEAVELO_DEVICE_GROUP_DIR","WEAVELO_RELAY","WEAVELO_P2P","WEAVELO_HOST_BIND_ADDR","WEAVELO_DISCOVERY_PORT"]}))\n')
        fixture.chmod(0o755)
        entry=self.root/'launcher';entry.write_bytes(install.launcher(self.home));entry.chmod(0o755)
        env=dict(os.environ,WEAVELO_DEVICE_GROUP_DIR=str(self.home/'.config/weavelo/mesh'),
            XDG_CONFIG_HOME=str(self.root/'retained'),WEAVELO_RELAY='1',WEAVELO_P2P='1')
        done=subprocess.run([str(entry)],env=env,capture_output=True,text=True,check=True)
        values=json.loads(done.stdout)
        self.assertEqual(values,{'WEAVELO_DEVICE_GROUP_DIR':str(p['profile']),'WEAVELO_RELAY':'0',
            'WEAVELO_P2P':'0','WEAVELO_HOST_BIND_ADDR':'0.0.0.0:39273','WEAVELO_DISCOVERY_PORT':'38119'})

    @unittest.skipUnless(sys.platform.startswith('linux'),'Native Linux no-replace rename fixture')
    def test_apply_only_to_fixture_home_preserves_old_client_and_forbids_replay(self):
        old=self.home/'.config/weavelo/mesh';old.mkdir(parents=True);(old/'fixture').write_text('unchanged')
        reviewed=self.plan();state=install.apply(reviewed,install.plan_sha(reviewed))
        self.assertEqual(state['phase'],'installed-not-started')
        self.assertEqual((old/'fixture').read_text(),'unchanged')
        selected=install.paths(self.home)
        self.assertEqual(list(selected['profile'].iterdir()),[])
        self.assertEqual(selected['profile'].stat().st_mode&0o777,0o700)
        self.assertEqual((selected['app']/'weavelo').read_bytes(),self.binary.read_bytes())
        self.assertIn(b'WEAVELO_RELAY=0',selected['service'].read_bytes())
        with self.assertRaisesRegex(ValueError,'collision'):install.apply(reviewed,install.plan_sha(reviewed))

    def test_installed_service_and_launcher_share_the_canonical_profile(self):
        rendered=install.payloads(self.home,self.zip)['service'][0].decode()
        expected='Environment="WEAVELO_DEVICE_GROUP_DIR='+str(install.paths(self.home)['profile'])+'"'
        self.assertIn(expected,rendered.splitlines())
        self.assertNotIn('Environment=WEAVELO_DEVICE_GROUP_DIR=%h/.config/weavelo/desktop',rendered)
        self.assertIn(str(install.paths(self.home)['profile']),install.launcher(self.home).decode())
        self.plan()  # Rendering never rewrites the sealed original package.

    def test_service_profile_retains_home_and_template_rejections(self):
        with zipfile.ZipFile(self.zip) as source:
            template=source.read('Weavelo/deploy/linux/weavelo-desktop.service')
        for invalid in (self.root/'space % and "quote"', self.root/'invalid\nhome'):
            with self.subTest(home=str(invalid)), self.assertRaisesRegex(ValueError,'Unsupported home path'):
                install.service_for_home(invalid,template)
        with self.assertRaisesRegex(ValueError,'Unexpected service profile'):
            install.service_for_home(self.home,template+template)

    def test_wrong_plan_digest_never_writes(self):
        reviewed=self.plan()
        with self.assertRaisesRegex(ValueError,'plan SHA256'):install.apply(reviewed,'0'*64)
        self.assertEqual(list(self.home.iterdir()),[])

    def test_linux_uses_original_producer_and_only_native_output_root(self):
        calls=[]
        with patch.object(lifecycle,'ROOT',self.root),patch.object(lifecycle,'regular'), \
             patch.object(lifecycle,'linux_output_root',return_value=self.root/'target/package/linux'), \
             patch.object(lifecycle,'public',side_effect=lambda *args:calls.append(args) or {}):
            with lifecycle.producer('linux-desktop'):pass
        setup=calls[0]
        self.assertEqual(setup,('release-setup','--authority',self.root/lifecycle.AUTHORITY,
                               '--output-root',self.root/'target/package/linux'))
        self.assertIn('--abort-current',calls[-1])


if __name__=='__main__':unittest.main()
