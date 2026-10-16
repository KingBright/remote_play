import copy
from dataclasses import dataclass
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import migrate_macos_identity as mac
import install_linux_desktop as linux
import install_macos_unified as upgrade
from macos_release_guard import ReleaseRejected, check_upgrade


@dataclass
class Expected:
    commit: str = 'a'*40
    identity_sha256: str = 'b'*64
    manifest_sha256: str = 'c'*64
    observer: str = '/unused-test-observer'
    observer_sha256: str = 'd'*64


class FakeMac:
    verify_cleanup_owner = mac.MacBackend.verify_cleanup_owner
    def __init__(self, home):
        self.home = home; self.deadline = upgrade.Deadline(20)
        self.events = []; self.old_loaded = True; self.new_loaded = False
        self.fail_health = False; self.refuse_exit = False; self.candidate_calls = 0

    def assert_no_other_products(self, allowed_pid=None): self.events.append('product_owners_checked')
    def no_system_services(self): self.events.append('system_checked')
    def old_identity(self, app): return {'binary_sha256': mac.digest(b'old'), 'bundle_id': 'com.remoteplay.unified'}
    def candidate_identity(self, app, contract):
        self.candidate_calls += 1
        if (app / 'Contents/MacOS/weavelo').read_bytes() != b'new': raise ReleaseRejected('bad candidate')
        return {'binary_sha256': mac.digest(b'new'), 'bundle_id': 'fun.hackerlife.weavelo'}
    def state(self, label):
        old = label == mac.OLD_LABEL; loaded = self.old_loaded if old else self.new_loaded
        exe = self.home / ('Applications/RemotePlay.app/Contents/MacOS/remote_play' if old else 'Applications/Weavelo.app/Contents/MacOS/weavelo')
        return {'loaded': loaded, 'running': loaded, 'pid': (42 if old else 43) if loaded else None,
                'program': str(exe), 'arguments': [str(exe), '--background-service']}
    def verify_owner(self, state, executable, binary_sha256):
        if not state['loaded']: raise ReleaseRejected('not owned')
        self.events.append('old_verified' if executable.name == 'remote_play' else 'new_verified')
    def assert_absent(self, label, executable, old_pid=None):
        if (self.old_loaded if label == mac.OLD_LABEL else self.new_loaded) or (self.refuse_exit and label == mac.OLD_LABEL):
            raise ReleaseRejected('owner still present')
        self.events.append('old_absent' if label == mac.OLD_LABEL else 'new_absent')
    def stop(self, label):
        self.events.append('stop_old' if label == mac.OLD_LABEL else 'stop_new')
        if label == mac.OLD_LABEL: self.old_loaded = False
        else: self.new_loaded = False
    def start(self, plist):
        new = plist.name == mac.NEW_LABEL + '.plist'
        self.events.append('start_new' if new else 'start_old')
        if new: self.new_loaded = True
        else: self.old_loaded = True
    def health(self, app, contract, profile):
        self.events.append('health')
        if self.fail_health: raise ReleaseRejected('health failed')
        return {'actual_loaded_image_verified': True}


class MacMigrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.home = Path(self.temp.name).resolve()
        self.backend = FakeMac(self.home); self.m = mac.Migration(self.home, self.backend)
        for p in [self.m.old_exe.parent, self.m.old_plist.parent, self.m.old_profile]: p.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.m.old_exe.write_bytes(b'old'); self.m.old_exe.chmod(0o700)
        self.m.old_plist.write_bytes(plistlib.dumps({'Label':mac.OLD_LABEL,'ProgramArguments':[str(self.m.old_exe),'--background-service']}))
        mac.write_private(self.m.old_profile/'mesh.secret', b'private-test-placeholder')
        self.candidate = self.home/'candidate/Weavelo.app'; exe=self.candidate/'Contents/MacOS/weavelo';exe.parent.mkdir(parents=True);exe.write_bytes(b'new');exe.chmod(0o700)
        self.contract={'expectations':Expected(),'raw_manifest':b'{}','manifest':{'binary_sha256':mac.digest(b'new')}}

    def tearDown(self): self.temp.cleanup()
    def reviewed(self): return mac.plan_hash(self.m.plan(self.candidate,self.contract))

    def test_plan_is_read_only_and_first_install_does_not_require_new_app(self):
        before=list(self.home.rglob('*')); plan=self.m.plan(self.candidate,self.contract)
        self.assertEqual(before,list(self.home.rglob('*')));self.assertFalse(plan['applied']);self.assertFalse(self.m.new.exists())

    def test_existing_new_identity_or_service_is_not_overwritten(self):
        self.m.new.mkdir()
        with self.assertRaisesRegex(ReleaseRejected,'absent new'):self.reviewed()

    def test_drift_rejects_before_stop_and_no_new_transaction(self):
        h=self.reviewed();self.m.old_exe.write_bytes(b'changed')
        with self.assertRaisesRegex(ReleaseRejected,'plan drift'):self.m.apply(self.candidate,self.contract,h)
        self.assertNotIn('stop_old',self.backend.events);self.assertFalse(self.m.work.exists())

    def test_old_owner_exit_precedes_new_start_and_backup_is_recoverable(self):
        result=self.m.apply(self.candidate,self.contract,self.reviewed())
        self.assertTrue(result['applied']);self.assertLess(self.backend.events.index('old_absent'),self.backend.events.index('start_new'))
        self.assertFalse(self.m.old.exists());self.assertEqual((self.m.work/'backup/RemotePlay.app/Contents/MacOS/remote_play').read_bytes(),b'old')
        self.assertEqual((self.m.work/'backup/profile/mesh.secret').read_bytes(),b'private-test-placeholder')
        self.assertFalse((self.m.new_profile/'mesh.secret').exists())

    def test_old_process_not_exited_never_starts_new(self):
        h=self.reviewed();self.backend.refuse_exit=True
        with self.assertRaises(ReleaseRejected):self.m.apply(self.candidate,self.contract,h)
        self.assertNotIn('start_new',self.backend.events);self.assertTrue(self.m.old.exists());self.assertTrue(self.backend.old_loaded)

    def test_failed_health_stops_new_then_restores_old_app_agent_and_data(self):
        h=self.reviewed();self.backend.fail_health=True;before=self.m.old_plist.read_bytes()
        with self.assertRaisesRegex(ReleaseRejected,'health failed'):self.m.apply(self.candidate,self.contract,h)
        self.assertTrue(self.backend.old_loaded);self.assertFalse(self.backend.new_loaded)
        self.assertEqual(self.m.old_plist.read_bytes(),before);self.assertEqual(self.m.old_exe.read_bytes(),b'old')
        self.assertTrue((self.m.work/'failed/Weavelo.app').is_dir())
        self.assertEqual(json.loads(self.m.journal.read_text())['phase'],'rolled_back')
        self.assertLess(self.backend.events.index('stop_new'),self.backend.events.index('start_old'))

    def test_linked_profile_rejected_without_service_mutation(self):
        (self.m.old_profile/'mesh.conf').symlink_to(self.m.old_profile/'mesh.secret')
        with self.assertRaisesRegex(ReleaseRejected,'Linked'):self.reviewed()
        self.assertNotIn('stop_old',self.backend.events)

    def test_interrupted_original_journal_recovers_without_second_apply(self):
        h=self.reviewed();real=self.backend.health
        def interrupted(*args):raise KeyboardInterrupt()
        self.backend.health=interrupted
        with self.assertRaises(KeyboardInterrupt):self.m.apply(self.candidate,self.contract,h)
        document=json.loads(self.m.journal.read_text());self.backend.health=real
        with self.assertRaisesRegex(ReleaseRejected,'transaction already exists'):self.m.apply(self.candidate,self.contract,h)
        self.assertTrue(self.m.rollback(document,self.contract)['rolled_back']);self.assertTrue(self.backend.old_loaded)

    def test_interruption_after_each_identity_rename_is_recovered_from_original_journal(self):
        for boundary in ['RemotePlay.app', mac.OLD_LABEL+'.plist', 'Weavelo.app', 'NativeMesh']:
            with self.subTest(boundary=boundary):
                if boundary != 'RemotePlay.app': self.tearDown(); self.setUp()
                h=self.reviewed();real=upgrade.durable_rename
                def interrupt(source,destination):
                    real(source,destination)
                    if destination.name == boundary: raise KeyboardInterrupt()
                with patch.object(upgrade,'durable_rename',side_effect=interrupt):
                    with self.assertRaises(KeyboardInterrupt):self.m.apply(self.candidate,self.contract,h)
                document=json.loads(self.m.journal.read_text())
                self.assertTrue(self.m.rollback(document,self.contract)['rolled_back'])
                self.assertTrue(self.backend.old_loaded);self.assertFalse(self.backend.new_loaded)
                self.assertEqual(self.m.old_exe.read_bytes(),b'old')

    def test_tampered_original_plan_rejected_before_recovery_actions(self):
        h=self.reviewed();self.backend.health=lambda *a: (_ for _ in ()).throw(KeyboardInterrupt())
        with self.assertRaises(KeyboardInterrupt):self.m.apply(self.candidate,self.contract,h)
        document=json.loads(self.m.journal.read_text());document['plan']['old_files']=[]
        events=list(self.backend.events)
        with self.assertRaisesRegex(ReleaseRejected,'Invalid original'):self.m.rollback(document,self.contract)
        self.assertEqual(self.backend.events,events)

    def test_preferences_have_private_backup_and_explicit_one_time_import(self):
        mac.write_private(self.m.old_preferences,b'{"ui":{"scale_mode":"fill"}}')
        self.m.import_preferences=True
        self.m.apply(self.candidate,self.contract,self.reviewed())
        self.assertEqual(self.m.old_preferences.read_bytes(),self.m.new_preferences.read_bytes())
        self.assertEqual((self.m.work/'backup/preferences.json').read_bytes(),self.m.old_preferences.read_bytes())
        self.assertFalse((self.m.new_profile/'mesh.secret').exists())

    def test_invalid_preferences_import_rejected_before_stopping(self):
        mac.write_private(self.m.old_preferences,b'not-json');self.m.import_preferences=True
        with self.assertRaises(mac.gate.IdentityRejected):self.reviewed()
        self.assertNotIn('stop_old',self.backend.events)

    def test_ordinary_compatible_upgrade_still_rejects_different_identity(self):
        with self.assertRaisesRegex(ReleaseRejected,'authorized identity'):
            check_upgrade({'bundle_id':'com.remoteplay.unified'},{'bundle_id':'fun.hackerlife.weavelo'})


    def test_legacy_0644_preferences_backup_and_import_preserve_original_mode(self):
        raw=b'{"ui":{"scale_mode":"fill"}}'
        self.m.old_preferences.write_bytes(raw);self.m.old_preferences.chmod(0o644)
        self.m.import_preferences=True
        self.m.apply(self.candidate,self.contract,self.reviewed())
        self.assertEqual(self.m.old_preferences.stat().st_mode & 0o777,0o644)
        self.assertEqual(self.m.old_preferences.read_bytes(),raw)
        for p in [self.m.new_preferences,self.m.work/'backup/preferences.json']:
            self.assertEqual(p.read_bytes(),raw);self.assertEqual(p.stat().st_mode & 0o777,0o600)

    def test_preferences_reader_allows_only_owned_0600_and_0644(self):
        p=self.m.old_preferences;p.write_bytes(b'{}')
        for mode in [0o600,0o644]:
            p.chmod(mode);self.assertEqual(mac.preferences_bytes(p),b'{}')
        for mode in [0o640,0o666,0o700]:
            p.chmod(mode)
            with self.subTest(mode=mode),self.assertRaises(ReleaseRejected):mac.preferences_bytes(p)
            self.assertEqual(p.stat().st_mode & 0o777,mode)

    def test_preferences_reader_rejects_foreign_descriptor_owner(self):
        p=self.m.old_preferences;p.write_bytes(b'{}');p.chmod(0o644)
        real=p.stat()
        foreign=os.stat_result((real.st_mode,real.st_ino,real.st_dev,real.st_nlink,real.st_uid+1,
                                real.st_gid,real.st_size,real.st_atime,real.st_mtime,real.st_ctime))
        with patch.object(mac.os,'fstat',return_value=foreign),self.assertRaises(ReleaseRejected):mac.preferences_bytes(p)
        self.assertEqual(p.read_bytes(),b'{}')

    def test_preferences_reader_rejects_link_directory_fifo_and_hardlink(self):
        p=self.m.old_preferences;target=p.with_name('settings-target');target.write_bytes(b'{}');target.chmod(0o644)
        p.symlink_to(target)
        with self.assertRaises((ReleaseRejected,mac.gate.IdentityRejected)):mac.preferences_bytes(p)
        p.unlink();p.mkdir()
        with self.assertRaises((ReleaseRejected,mac.gate.IdentityRejected)):mac.preferences_bytes(p)
        p.rmdir();os.mkfifo(p)
        with self.assertRaises((ReleaseRejected,mac.gate.IdentityRejected)):mac.preferences_bytes(p)
        p.unlink();os.link(target,p)
        with self.assertRaises(ReleaseRejected):mac.preferences_bytes(p)
        self.assertEqual(target.read_bytes(),b'{}')

    def test_preferences_reader_refuses_descriptor_or_path_drift(self):
        p=self.m.old_preferences;p.write_bytes(b'{}');p.chmod(0o644)
        real=mac.os.fstat;calls=0
        def drift(fd):
            nonlocal calls
            calls+=1
            result=real(fd)
            if calls==2:p.write_bytes(b'foreign')
            return result
        with patch.object(mac.os,'fstat',side_effect=drift),self.assertRaises(ReleaseRejected):mac.preferences_bytes(p)
        self.assertEqual(p.read_bytes(),b'foreign')

    def test_release_state_still_requires_private_0600(self):
        p=self.m.apps/'.remoteplay-release-identity.json';p.write_bytes(b'{}');p.chmod(0o644)
        with self.assertRaises(mac.gate.IdentityRejected):self.m.old_release_state_hash()
        self.assertNotIn('stop_old',self.backend.events)


class LinuxDesktopTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.home=Path(self.temp.name).resolve();self.t=linux.DesktopTransaction(self.home)
        self.assets=self.home/'assets';self.assets.mkdir()
        for name in self.t.targets():
            if name!='desktop':(self.assets/name).write_bytes(('asset:'+name).encode())
        self.exe=self.home/'Apps with spaces/weavelo';self.exe.parent.mkdir();self.exe.write_bytes(b'product')
        self.provenance={'identity':'independently-pinned-test','binary_sha256':linux.sha(b'product')}
    def tearDown(self):self.temp.cleanup()
    def reviewed(self):return linux.plan_hash(self.t.plan(self.assets,self.exe,self.provenance))

    def test_readonly_plan_and_all_hicolor_destinations(self):
        plan=self.t.plan(self.assets,self.exe,self.provenance)
        self.assertEqual(len(plan['entries']),9);self.assertFalse(self.t.work.exists())
        self.assertIn('hicolor/scalable/apps/',plan['entries'][-2]['path'])
    def test_install_binds_desktop_icon_and_quoted_absolute_executable(self):
        result=self.t.apply(self.assets,self.exe,self.provenance,self.reviewed())
        self.assertEqual(result['installed_files'],9)
        desktop=self.t.targets()['desktop'].read_text();self.assertIn(f'Exec="{self.exe}"',desktop);self.assertIn(f'Icon={linux.APP_ID}',desktop)
        for name,path in self.t.targets().items():
            if name!='desktop':self.assertEqual(path.read_bytes(),(self.assets/name).read_bytes())
    def test_plan_drift_preserves_existing_icon(self):
        h=self.reviewed();path=self.t.targets()['icon-16.png'];path.parent.mkdir(parents=True);path.write_bytes(b'user change')
        with self.assertRaisesRegex(ValueError,'differs|plan drift'):self.t.apply(self.assets,self.exe,self.provenance,h)
        self.assertEqual(path.read_bytes(),b'user change')
    def test_partial_failure_preserves_identical_preexisting_file_and_mode(self):
        path=self.t.targets()['icon-16.png'];path.parent.mkdir(parents=True)
        raw=(self.assets/'icon-16.png').read_bytes();path.write_bytes(raw);path.chmod(0o640);inode=path.stat().st_ino
        h=self.reviewed();real=linux.exclusive_rename
        def fail(source,destination):
            if destination==self.t.targets()['icon-32.png']:raise OSError('injected disk failure')
            return real(source,destination)
        with patch.object(linux,'exclusive_rename',side_effect=fail):
            with self.assertRaisesRegex(OSError,'injected'):self.t.apply(self.assets,self.exe,self.provenance,h)
        self.assertEqual(path.read_bytes(),raw);self.assertEqual(path.stat().st_mode&0o777,0o640)
        self.assertEqual(path.stat().st_ino,inode);self.assertFalse(self.t.targets()['desktop'].exists())

    def test_recovery_refuses_concurrent_user_edit(self):
        h=self.reviewed();self.t.apply(self.assets,self.exe,self.provenance,h)
        self.t.targets()['desktop'].write_bytes(b'user replacement');document=json.loads(self.t.journal.read_text())
        with self.assertRaisesRegex(ValueError,'outside original'):self.t.rollback(document,h)
        self.assertEqual(self.t.targets()['desktop'].read_bytes(),b'user replacement')
    def test_custom_xdg_data_home_is_bound_and_external_home_rejected(self):
        custom=self.home/'desktop-data';t=linux.DesktopTransaction(self.home,custom)
        self.assertTrue(t.targets()['desktop'].is_relative_to(custom))
        with self.assertRaisesRegex(ValueError,'authorized home'):linux.DesktopTransaction(self.home,Path('/tmp'))

    def test_executable_drift_rejected_before_publication(self):
        h=self.reviewed();self.exe.write_bytes(b'other product')
        with self.assertRaisesRegex(ValueError,'executable changed'):self.t.apply(self.assets,self.exe,self.provenance,h)
        self.assertFalse(self.t.targets()['desktop'].exists())

    def test_symlink_destination_rejected(self):
        target=self.t.targets()['desktop'];target.parent.mkdir(parents=True);target.symlink_to(self.exe)
        with self.assertRaisesRegex(ValueError,'Linked'):self.reviewed()


class RetiredLauncherTests(unittest.TestCase):
    def test_historical_entry_never_runs_build_or_kill(self):
        root=Path(__file__).resolve().parents[2]
        result=subprocess.run(['/bin/bash',str(root/'start.sh'),'host'],capture_output=True,text=True)
        self.assertEqual(result.returncode,64);self.assertIn('migrate_macos_identity.py',result.stderr)
        self.assertNotIn('Ad-Hoc',result.stderr)


if __name__=='__main__':unittest.main()
