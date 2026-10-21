"""Private synthetic app/profile fixtures; no system signing, permissions or services."""
import json
import contextlib
import os
from pathlib import Path
import plistlib
import sys
import tempfile
import subprocess
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import desktop_identity as desktop
import install_macos_desktop as installer
import macos_release_guard as guard
import prepare_macos_desktop as preparation
import release_artifact_lifecycle as lifecycle


class ParallelDesktopTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.home=Path(self.temp.name).resolve()/'home';self.home.mkdir(mode=0o700)
        self.old_app=self.home/'Applications/Weavelo.app'
        self.old_profile=self.home/'Library/Application Support/Weavelo/NativeMesh'
        self.old_plist=self.home/'Library/LaunchAgents/fun.hackerlife.weavelo.host.plist'
        for p in (self.old_app,self.old_profile,self.old_plist.parent):p.mkdir(parents=True,exist_ok=True)
        self.old_app.joinpath('sentinel').write_text('retained-client')
        self.old_profile.joinpath('mesh.secret').write_text('synthetic-private-fixture')
        self.old_plist.write_text('retained-service')

    def snapshot(self):
        return {str(p.relative_to(self.home)):(p.stat().st_ino,p.stat().st_mtime_ns,p.read_bytes())
                for p in self.home.rglob('*') if p.is_file()}

    def test_readonly_plan_leaves_retained_app_profile_and_service_unchanged(self):
        before=self.snapshot();result=installer.plan(self.home)
        self.assertEqual(before,self.snapshot())
        self.assertFalse(result['applied'])
        self.assertEqual(result['paths']['app'],str(self.home/'Applications/Weavelo/Weavelo.app'))
        self.assertEqual(result['paths']['profile'],str(self.home/'Library/Application Support/Weavelo/Desktop'))
        self.assertNotEqual(result['paths']['app'],str(self.old_app))

    def test_ipc_and_service_names_do_not_reach_retained_owner(self):
        result=installer.plan(self.home);p=desktop.paths(self.home)
        self.assertNotEqual(p['background_socket'],self.old_profile/'background.sock')
        self.assertNotEqual(p['gui_socket'],self.old_profile/'gui-state/gui-receive.sock')
        agent=result['launch_agent']
        self.assertEqual(agent['Label'],'fun.hackerlife.weavelo.desktop.host')
        self.assertNotEqual(agent['Label'],self.old_plist.stem)
        env=agent['EnvironmentVariables']
        self.assertEqual(env['WEAVELO_RELAY'],'0');self.assertEqual(env['WEAVELO_P2P'],'0')
        self.assertNotEqual(env['WEAVELO_HOST_BIND_ADDR'],'0.0.0.0:39271')
        self.assertNotEqual(env['WEAVELO_DISCOVERY_PORT'],'38117')
        self.assertEqual(agent['KeepAlive'],{'SuccessfulExit':False})

    def test_new_bundle_preserves_original_signer_but_not_legacy_app_id(self):
        old=guard.load_policy();new=guard.load_policy(guard.DESKTOP_POLICY_FILE)
        self.assertEqual(old.certificate_sha1,new.certificate_sha1)
        self.assertNotEqual(old.bundle_id,new.bundle_id)
        self.assertEqual(new.bundle_id,desktop.identity()['application_id'])
        self.assertEqual(installer.gate.macos_policy_for_product({}).bundle_id,old.bundle_id)
        self.assertEqual(installer.gate.macos_policy_for_product({'desktop_identity':desktop.identity()}).bundle_id,new.bundle_id)
        with self.assertRaises(installer.gate.IdentityRejected):
            installer.gate.macos_policy_for_product({'desktop_identity':{'application_id':'unknown'}})

    def test_existing_independent_destinations_or_journal_block_replay(self):
        for key in ('app','profile','plist','state'):
            with self.subTest(key=key):
                dest=desktop.paths(self.home)[key];dest.parent.mkdir(parents=True,exist_ok=True)
                dest.mkdir() if key!='plist' else dest.write_text('prior independent install')
                with self.assertRaisesRegex(ValueError,'already exists'):
                    installer.plan(self.home)
                dest.rmdir() if key!='plist' else dest.unlink()

    def test_redirected_new_app_parent_is_rejected_without_touching_legacy(self):
        before=self.snapshot();redirect=self.home/'Applications/Weavelo'
        redirect.symlink_to(self.old_app,target_is_directory=True)
        with self.assertRaisesRegex(ValueError,'symlink'):installer.plan(self.home)
        redirect.unlink();self.assertEqual(before,self.snapshot())

    def test_root_and_group_writable_destinations_are_rejected(self):
        with patch.object(installer.os,'geteuid',return_value=0):
            with self.assertRaises(guard.ReleaseRejected):installer.plan(self.home)
        self.home.joinpath('Applications').chmod(0o777)
        with self.assertRaisesRegex(ValueError,'writable'):installer.plan(self.home)

    def test_bad_archive_digest_is_rejected_before_destination_creation(self):
        archive=self.home/'candidate.zip';archive.write_bytes(b'synthetic')
        before=self.snapshot()
        with self.assertRaises(guard.ReleaseRejected):installer.apply(archive,self.home,{},'a'*64)
        self.assertEqual(before,self.snapshot())

    @unittest.skipUnless(sys.platform=='darwin','native exclusive install rename')
    def test_fixture_fresh_install_commits_without_starting_or_modifying_retained_client(self):
        candidate=self.home/'candidate/Weavelo.app'
        (candidate/'Contents/MacOS').mkdir(parents=True)
        (candidate/'Contents/MacOS/weavelo').write_bytes(b'synthetic-native-install-fixture')
        archive=self.home/'signed-fixture.zip';archive.write_bytes(b'synthetic-archive')
        before={p:(p.stat().st_ino,p.read_bytes()) for p in
                [self.old_app/'sentinel',self.old_profile/'mesh.secret',self.old_plist]}
        @contextlib.contextmanager
        def verified(*args,**kwargs):yield candidate,{'signature_verified':True}
        with patch.object(guard,'verified_archive',verified),patch.object(guard,'verify_app',return_value={}), \
             patch.object(installer,'verified_product',return_value={'desktop_identity':desktop.identity()}):
            result=installer.apply(archive,self.home,{},installer.gate.hash_file(archive))
        self.assertEqual(result['phase'],'committed_not_started')
        self.assertTrue(result['applied']);self.assertFalse(result['service_started'])
        self.assertEqual(before,{p:(p.stat().st_ino,p.read_bytes()) for p in before})
        selected=desktop.paths(self.home)
        self.assertEqual(list(selected['profile'].iterdir()),[])
        self.assertEqual(plistlib.loads(selected['plist'].read_bytes())['Label'],desktop.identity()['service_label'])
        with self.assertRaisesRegex(ValueError,'already exists'):installer.plan(self.home)

    def test_bundle_assembly_requires_real_compiled_new_namespace(self):
        binary=self.home/'binary';binary.write_bytes(b'synthetic-executable')
        stage=self.home/'stage';stage.mkdir()
        policy=guard.load_policy(guard.DESKTOP_POLICY_FILE)
        with patch.object(guard,'run',return_value=json.dumps({'product':'Weavelo'})):
            with self.assertRaisesRegex(guard.ReleaseRejected,'namespace'):
                guard.assemble_app(stage,binary,'2.0.0-alpha.9','20261021.1',policy)
        self.assertEqual(list(stage.iterdir()),[])

    def test_shared_bundle_layout_and_original_installer_reject_preparation(self):
        stage=self.home/'stage';stage.mkdir();binary=self.home/'binary';binary.write_bytes(b'synthetic-executable')
        id=desktop.identity()
        info={'desktop_identity':id,'desktop_runtime':{'control_port':id['control_port'],
              'discovery_port':id['discovery_port'],'implicit_relay':False,'implicit_p2p':False}}
        with patch.object(guard,'run',return_value=json.dumps(info)):
            app=guard.assemble_app(stage,binary,'2.0.0-alpha.9','20261021.1',guard.load_policy(guard.DESKTOP_POLICY_FILE))
        bundle=plistlib.loads((app/'Contents/Info.plist').read_bytes())
        self.assertEqual(bundle['CFBundleIdentifier'],id['application_id'])
        self.assertEqual(bundle['CFBundleDisplayName'],'Weavelo')
        archive=stage/'prepared.zip'
        with zipfile.ZipFile(archive,'w') as out:
            out.writestr(preparation.MARKER,json.dumps({'installable':False}))
        with self.assertRaisesRegex(guard.ReleaseRejected,'top-level'):
            with guard.verified_archive(archive,guard.load_policy(guard.DESKTOP_POLICY_FILE)):pass

    @unittest.skipUnless(Path('/bin/bash').exists(),'Bash argument-composition fixture')
    def test_system_bash_keeps_empty_legacy_and_nonempty_desktop_guard_arguments(self):
        fixture=self.home/'shell-fixture';(fixture/'scripts').mkdir(parents=True)
        (fixture/'scripts/macos_release_guard.py').write_text('import sys,json;print(json.dumps(sys.argv[1:]))')
        text=(desktop.ROOT/'scripts/package_macos_unified.sh').read_text()
        lines=[line.rstrip().rstrip('\\').rstrip() for line in text.splitlines()
               if line.startswith('python3 "$ROOT_DIR/scripts/macos_release_guard.py"')]
        self.assertEqual(len(lines),2)
        for args,selected in [('()',[]),('( --desktop )',['--desktop'])]:
            for line,operation in zip(lines,['preflight','package']):
                command='ROOT_DIR='+str(fixture)+'; DESKTOP_ARGS='+args+'; '+line
                done=subprocess.run(['/bin/bash','-uc',command],capture_output=True,text=True)
                self.assertEqual(done.returncode,0,done.stderr)
                self.assertEqual(json.loads(done.stdout),[operation,*selected])

    def test_signed_desktop_mode_retains_preflight_and_explicit_preparation_uses_original_producer(self):
        with patch.object(lifecycle,'producer',side_effect=RuntimeError('original producer')), \
             patch.object(lifecycle.subprocess,'run') as run:
            run.return_value.returncode=0
            with self.assertRaisesRegex(RuntimeError,'original producer'):lifecycle.run('macos',['--desktop'])
            self.assertEqual(run.call_args.args[0][-2:],['preflight','--desktop'])
            run.reset_mock()
            with self.assertRaisesRegex(RuntimeError,'original producer'):lifecycle.run('macos',['--prepare-desktop'])
            run.assert_not_called()


if __name__=='__main__':unittest.main()
