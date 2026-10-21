"""Bounded real-process lock fixtures; no product builds or user-home writes."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

HERE=Path(__file__).resolve().parent
MODULE=HERE/'rust_posix_lock.py'
if not MODULE.exists():MODULE=HERE.parent/'maintenance/rust_posix_lock.py'
spec=importlib.util.spec_from_file_location('posix_lock_fixture',MODULE)
p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)


class TrustedPathTests(unittest.TestCase):
    def setUp(self):
        tmp=tempfile.TemporaryDirectory(prefix='cleanup-home-fixture-');self.addCleanup(tmp.cleanup)
        self.root=Path(tmp.name).resolve();self.home=self.root/'realhome';self.home.mkdir(mode=0o700)
        self.alias=self.root/'alias';self.alias.symlink_to(self.home,target_is_directory=True)

    def test_only_enrolled_home_prefix_resolves(self):
        pin=p.PinnedPath(self.alias/'Library/cleanup.pid',self.alias,self.alias)
        self.assertEqual(pin.path,self.home/'Library/cleanup.pid');pin.check()

    def test_suffix_symlink_still_rejected(self):
        elsewhere=self.root/'outside';elsewhere.mkdir()
        (self.home/'Library').symlink_to(elsewhere,target_is_directory=True)
        with self.assertRaisesRegex(p.LockRefused,'symlink'):
            p.PinnedPath(self.alias/'Library/cleanup.pid',self.alias,self.alias)
        self.assertEqual(list(elsewhere.iterdir()),[])

    def test_other_account_target_is_rejected(self):
        outside=self.root/'other-home';outside.mkdir(mode=0o700)
        with self.assertRaisesRegex(p.LockRefused,'enrolled'):
            p.PinnedPath(self.alias/'cleanup.pid',self.alias,outside)

    def test_writable_home_rejected(self):
        self.home.chmod(0o777)
        try:
            with self.assertRaisesRegex(p.LockRefused,'Untrusted'):
                p.PinnedPath(self.alias/'cleanup.pid',self.alias,self.alias)
        finally:self.home.chmod(0o700)

    def test_alias_replacement_is_detected_before_following_it(self):
        pin=p.PinnedPath(self.alias/'cleanup.pid',self.alias,self.alias)
        outside=self.root/'other';outside.mkdir(mode=0o700)
        self.alias.unlink();self.alias.symlink_to(outside,target_is_directory=True)
        with self.assertRaisesRegex(p.LockRefused,'replaced'):pin.check()
        self.assertEqual(list(outside.iterdir()),[])

    def test_explicit_unrelated_symlink_gets_no_exception(self):
        with self.assertRaisesRegex(p.LockRefused,'symlink'):
            p.PinnedPath(self.alias/'cleanup.pid',self.home,self.home)

    def test_traversal_rejected(self):
        with self.assertRaises(p.LockRefused):
            p.PinnedPath(self.alias/'..'/'escape',self.alias,self.alias)


@unittest.skipUnless(sys.platform.startswith('linux'),'Real Linux fallback PID-file fixtures')
class LinuxPidTests(unittest.TestCase):
    def setUp(self):
        tmp=tempfile.TemporaryDirectory(prefix='cleanup-pid-fixture-');self.addCleanup(tmp.cleanup)
        self.root=Path(tmp.name).resolve();self.lock=self.root/'cleanup.pid'

    def worker(self,code,*arguments,**kwargs):
        return subprocess.Popen([sys.executable,'-B','-c',
            'import importlib.util,sys,os,time;from pathlib import Path;'
            's=importlib.util.spec_from_file_location("pidcompat",sys.argv[1]);'
            'm=importlib.util.module_from_spec(s);s.loader.exec_module(m);'
            'lock=Path(sys.argv[2]);'+code,str(MODULE),str(self.lock),*map(str,arguments)],
            stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,**kwargs)

    def wait_file(self,file,process):
        deadline=time.monotonic()+5
        while not file.exists() and time.monotonic()<deadline:
            if process.poll() is not None:self.fail(process.communicate()[1])
            time.sleep(.01)
        self.assertTrue(file.exists())

    def test_decimal_protocol_and_exception_release(self):
        with self.assertRaisesRegex(RuntimeError,'fixture'):
            with p.cleanup_lock(self.lock):
                self.assertEqual(self.lock.read_text().strip(),str(os.getpid()))
                raise RuntimeError('fixture')
        self.assertFalse(self.lock.exists())
        self.assertEqual(list(self.root.iterdir()),[])

    def test_live_legacy_pid_is_never_stolen(self):
        self.lock.write_text(str(os.getpid())+'\n')
        before=self.lock.stat().st_ino
        with self.assertRaisesRegex(p.LockRefused,'owns'):
            with p.cleanup_lock(self.lock):pass
        self.assertEqual(self.lock.stat().st_ino,before)

    def test_malformed_pid_preserved(self):
        self.lock.write_text('unknown owner')
        with self.assertRaisesRegex(p.LockRefused,'Malformed'):
            with p.cleanup_lock(self.lock):pass
        self.assertEqual(self.lock.read_text(),'unknown owner')

    def test_stale_exited_pid_recovered(self):
        child=subprocess.Popen([sys.executable,'-c','pass']);child.wait(timeout=5)
        self.lock.write_text(str(child.pid)+'\n')
        with p.cleanup_lock(self.lock):self.assertEqual(int(self.lock.read_text()),os.getpid())
        self.assertFalse(self.lock.exists())

    def test_independent_process_contention_and_crash_recovery(self):
        ready=self.root/'ready'
        code='\nwith m.cleanup_lock(lock):\n Path(sys.argv[3]).write_text("held")\n sys.stdin.read(1)\n'
        child=self.worker(code,ready,stdin=subprocess.PIPE)
        try:
            self.wait_file(ready,child)
            with self.assertRaises(p.LockRefused):
                with p.cleanup_lock(self.lock):pass
            child.kill();child.wait(timeout=5)
            self.assertTrue(self.lock.exists())
            with p.cleanup_lock(self.lock):self.assertEqual(int(self.lock.read_text()),os.getpid())
            self.assertFalse(self.lock.exists())
        finally:
            if child.poll() is None:child.kill();child.wait(timeout=5)
            child.communicate()

    def test_six_competing_processes_do_not_enter_together(self):
        child=subprocess.Popen([sys.executable,'-c','pass']);child.wait(timeout=5)
        self.lock.write_text(str(child.pid)+'\n')
        marker=self.root/'critical';audit=self.root/'audit'
        code='''
for turn in range(5):
 for attempt in range(1000):
  try:
   with m.cleanup_lock(lock):
    fd=os.open(sys.argv[3],os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600)
    os.close(fd)
    time.sleep(.002)
    with open(sys.argv[4],"a") as out:out.write(str(os.getpid())+"\\n")
    os.unlink(sys.argv[3])
   break
  except m.LockRefused:time.sleep(.002)
 else:raise RuntimeError("contention deadline")
'''
        children=[self.worker(code,marker,audit) for _ in range(6)]
        try:
            for process in children:
                _,err=process.communicate(timeout=20);self.assertEqual(process.returncode,0,err)
        finally:
            for process in children:
                if process.poll() is None:process.kill();process.communicate()
        self.assertEqual(len(audit.read_text().splitlines()),30)
        self.assertFalse(marker.exists());self.assertFalse(self.lock.exists())

    def test_replacement_inode_is_not_released(self):
        with p.cleanup_lock(self.lock):
            replacement=self.root/'replacement';replacement.write_text(str(os.getpid())+'\n')
            os.replace(replacement,self.lock);inode=self.lock.stat().st_ino
        self.assertEqual(self.lock.stat().st_ino,inode)

    def test_symlink_lock_is_rejected_without_touching_target(self):
        other=self.root/'other';other.write_text('untouched');self.lock.symlink_to(other)
        with self.assertRaises(p.LockRefused):
            with p.cleanup_lock(self.lock):pass
        self.assertEqual(other.read_text(),'untouched')

    def test_pinned_directory_fd_does_not_follow_replacement(self):
        parent=self.root/'locks';parent.mkdir(mode=0o700);lock=parent/'cleanup.pid'
        moved=self.root/'moved';outside=self.root/'outside';outside.mkdir(mode=0o700)
        with p.cleanup_lock(lock):
            parent.rename(moved);parent.symlink_to(outside,target_is_directory=True)
        self.assertFalse((moved/'cleanup.pid').exists())
        self.assertEqual(list(outside.iterdir()),[])

    def test_wrong_owner_is_rejected(self):
        self.lock.write_text(str(os.getpid())+'\n')
        fd=os.open(self.root,os.O_RDONLY|os.O_DIRECTORY)
        try:
            with patch.object(p.os,'getuid',return_value=os.getuid()+1),self.assertRaises(p.LockRefused):
                p.read_pid(fd,self.lock.name)
        finally:os.close(fd)


if __name__=='__main__':unittest.main()
