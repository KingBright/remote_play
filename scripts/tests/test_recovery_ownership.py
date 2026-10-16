import fcntl
import json
import os
from pathlib import Path
import types
import unittest
from unittest.mock import patch
import test_identity_migration as fixtures
import migrate_macos_identity as mac
import install_linux_desktop as linux
from macos_release_guard import ReleaseRejected


class MacRecoveryRegression(unittest.TestCase):
    tearDown=fixtures.MacMigrationTests.tearDown
    reviewed=fixtures.MacMigrationTests.reviewed

    def setUp(self):
        fixtures.MacMigrationTests.setUp(self)
        self.failed_job=False;original=self.backend.state
        def state(label):
            value=original(label)
            if label==mac.NEW_LABEL and value['loaded'] and self.failed_job:
                value.update(running=False,pid=None)
            return value
        self.backend.state=state
        verify=self.backend.verify_owner
        def strict(value,exe,binary):
            if not value['running'] or value['pid'] is None:raise ReleaseRejected('live owner is not running')
            return verify(value,exe,binary)
        self.backend.verify_owner=strict
        if hasattr(mac.MacBackend,'verify_cleanup_owner'):
            self.backend.verify_cleanup_owner=types.MethodType(mac.MacBackend.verify_cleanup_owner,self.backend)

    def test_no_pid_failed_job_rolls_back_and_restores_old_service(self):
        h=self.reviewed();self.failed_job=True;self.backend.fail_health=True
        with self.assertRaises(ReleaseRejected):self.m.apply(self.candidate,self.contract,h)
        self.assertTrue(self.backend.old_loaded,'old service must be restored')
        self.assertFalse(self.backend.new_loaded,'unstarted new job must be removed')
        self.assertEqual(self.m.old_exe.read_bytes(),b'old')

    def test_original_journal_recovers_failed_job_with_no_pid(self):
        h=self.reviewed();self.failed_job=True
        self.backend.health=lambda *a: (_ for _ in ()).throw(KeyboardInterrupt())
        with self.assertRaises(KeyboardInterrupt):self.m.apply(self.candidate,self.contract,h)
        document=json.loads(self.m.journal.read_text())
        self.m.rollback(document,self.contract)
        self.assertTrue(self.backend.old_loaded);self.assertFalse(self.backend.new_loaded)

    def test_legacy_installer_lock_blocks_migration(self):
        h=self.reviewed();path=self.m.apps/'.remoteplay-install.lock'
        with path.open('w') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            with self.assertRaises(ReleaseRejected):self.m.apply(self.candidate,self.contract,h)
        self.assertNotIn('stop_old',self.backend.events)

    def test_live_unverified_pid_cannot_be_cleared(self):
        h=self.reviewed();self.backend.health=lambda *a: (_ for _ in ()).throw(KeyboardInterrupt())
        with self.assertRaises(KeyboardInterrupt):self.m.apply(self.candidate,self.contract,h)
        self.backend.verify_owner=lambda *a: (_ for _ in ()).throw(ReleaseRejected('native image unavailable'))
        with self.assertRaisesRegex(ReleaseRejected,'native image unavailable'):
            self.m.rollback(json.loads(self.m.journal.read_text()),self.contract)
        self.assertNotIn('stop_new',self.backend.events)


class LinuxOwnershipRegression(unittest.TestCase):
    setUp=fixtures.LinuxDesktopTests.setUp
    tearDown=fixtures.LinuxDesktopTests.tearDown
    reviewed=fixtures.LinuxDesktopTests.reviewed

    def test_foreign_file_created_at_publication_is_never_overwritten(self):
        h=self.reviewed();target=self.t.targets()['icon-16.png'];foreign=b'concurrent user icon'
        name='exclusive_rename' if hasattr(linux,'exclusive_rename') else None
        real=getattr(linux,name) if name else linux.os.replace
        def race(source,destination,*a,**kw):
            if Path(destination)==target and not target.exists():target.write_bytes(foreign)
            return real(source,destination,*a,**kw)
        manager=patch.object(linux,name,side_effect=race) if name else patch.object(linux.os,'replace',side_effect=race)
        with manager:
            with self.assertRaises(Exception):self.t.apply(self.assets,self.exe,self.provenance,h)
        self.assertEqual(target.read_bytes(),foreign)

    def test_failure_before_publication_preserves_identical_foreign_file(self):
        h=self.reviewed();target=self.t.targets()['icon-16.png'];raw=(self.assets/'icon-16.png').read_bytes()
        save=self.t.save
        def fail(document):
            save(document)
            if document['phase']=='installing':
                target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(raw)
                raise OSError('publication has not started')
        with patch.object(self.t,'save',side_effect=fail):
            with self.assertRaises(Exception):self.t.apply(self.assets,self.exe,self.provenance,h)
        self.assertTrue(target.exists(),'matching bytes do not establish transaction ownership')
        self.assertEqual(target.read_bytes(),raw)

    def test_identical_replacement_after_install_is_not_deleted_by_rollback(self):
        h=self.reviewed();self.t.apply(self.assets,self.exe,self.provenance,h)
        target=self.t.targets()['icon-16.png'];replacement=target.with_name('foreign.png')
        replacement.write_bytes(target.read_bytes());foreign_inode=replacement.stat().st_ino
        os.replace(replacement,target)
        try:self.t.rollback(json.loads(self.t.journal.read_text()),h)
        except ValueError:pass
        self.assertTrue(target.exists());self.assertEqual(target.stat().st_ino,foreign_inode)


if __name__=='__main__':unittest.main()
