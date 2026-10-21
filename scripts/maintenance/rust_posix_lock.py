"""Original cleanup PID-file protocol on Linux, with a pinned trusted home alias.

No replacement lock path. Native shlock remains preferred. The Linux fallback
uses the same decimal-PID file, atomic hard-link admission, live-PID rejection
and stale-PID recovery. An advisory lock on its existing parent directory only
serializes fallback acquisition/reaping/release; it is not the admission token.
"""
from contextlib import contextmanager
import fcntl
import os
from pathlib import Path
import pwd
import stat
import subprocess
import sys
import uuid


class LockRefused(RuntimeError):
    pass


def require(ok, reason):
    if not ok:
        raise LockRefused(reason)


def identity(info):
    return info.st_dev, info.st_ino, info.st_uid, info.st_mode


def strict(path):
    path=Path(os.path.abspath(path))
    require('..' not in path.parts,'Cleanup path traversal')
    for part in (path,*path.parents):
        require(not part.is_symlink(),'Cleanup symlink below trusted root: '+str(part))
    return path


class PinnedPath:
    def __init__(self,path,home=None,account_home=None):
        raw=Path(path)
        require(raw.is_absolute() and '..' not in raw.parts,'Absolute cleanup path required')
        home=Path.home() if home is None else Path(home)
        account_home=Path(pwd.getpwuid(os.getuid()).pw_dir) if account_home is None else Path(account_home)
        self.pins=[]
        if raw.is_relative_to(home):
            require(home.is_absolute() and home.resolve(strict=True)==account_home.resolve(strict=True),
                    'Environment home differs from the enrolled local account')
            real=home.resolve(strict=True)
            info=real.lstat()
            require(stat.S_ISDIR(info.st_mode) and info.st_uid==os.getuid()
                    and not info.st_mode&0o022,'Untrusted canonical home')
            # Only aliases in the established account-home prefix may resolve.
            # Any symlink in Library/Caches or another suffix remains forbidden.
            for part in (home,*home.parents,real,*real.parents):
                info=part.lstat()
                if part.is_symlink():
                    parent=part.parent.stat()
                    require(info.st_uid in (0,os.getuid()) and parent.st_uid in (0,os.getuid())
                            and not parent.st_mode&0o022,'Untrusted home alias owner/parent')
                    link=os.readlink(part)
                else:
                    require(stat.S_ISDIR(info.st_mode) and info.st_uid in (0,os.getuid()),
                            'Untrusted home ancestor')
                    # Root-owned sticky temp ancestors are permitted for isolated fixtures.
                    require(not info.st_mode&0o022 or (info.st_uid==0 and info.st_mode&stat.S_ISVTX),
                            'Writable home ancestor')
                    link=None
                self.pins.append((part,identity(info),link))
            self.path=strict(real/raw.relative_to(home))
        else:
            # Existing explicit fixture/contract paths gain no alias exception.
            self.path=strict(raw)
        self.check()

    def check(self):
        for path,before,link in self.pins:
            require(identity(path.lstat())==before,'Trusted home path replaced')
            require((os.readlink(path) if path.is_symlink() else None)==link,'Trusted home alias replaced')
        strict(self.path)


def read_pid(directory,name):
    try:
        fd=os.open(name,os.O_RDONLY|os.O_NOFOLLOW,dir_fd=directory)
    except FileNotFoundError:
        return None
    with os.fdopen(fd,'rb') as incoming:
        before=os.fstat(incoming.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_uid==os.getuid()
                and before.st_nlink in (1,2) and not before.st_mode&0o022,'Unsafe existing cleanup PID file')
        raw=incoming.read(129)
        require(len(raw)<=128 and raw.strip().isdigit(),'Malformed cleanup PID file; preserve it')
        pid=int(raw.strip());require(pid>1,'Invalid cleanup PID; preserve it')
        require(identity(os.fstat(incoming.fileno()))==identity(before),'Cleanup PID inode changed')
        return pid,identity(before)


def live(pid):
    try:
        os.kill(pid,0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


@contextmanager
def directory_guard(fd):
    try:
        fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
    except BlockingIOError as error:
        raise LockRefused('Shared cleanup PID admission/recovery is active') from error
    try:
        yield
    finally:
        fcntl.flock(fd,fcntl.LOCK_UN)


def fallback_acquire(directory,name):
    require(sys.platform.startswith('linux'),'Missing native shlock; fallback is Linux only')
    with directory_guard(directory):
        previous=read_pid(directory,name)
        if previous:
            require(not live(previous[0]),'Existing cleanup/build owns the shared cleanup PID')
            require(read_pid(directory,name)==previous,'Stale cleanup PID changed; preserve it')
            os.unlink(name,dir_fd=directory)
        temporary='.'+name+'.'+uuid.uuid4().hex+'.pid'
        fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600,dir_fd=directory)
        try:
            with os.fdopen(fd,'wb') as out:
                out.write((str(os.getpid())+'\n').encode());out.flush();os.fsync(out.fileno())
            try:
                os.link(temporary,name,src_dir_fd=directory,dst_dir_fd=directory,follow_symlinks=False)
            except FileExistsError as error:
                raise LockRefused('Existing cleanup/build won shared PID admission') from error
        finally:
            os.unlink(temporary,dir_fd=directory)
        result=read_pid(directory,name)
        require(result and result[0]==os.getpid(),'Cleanup PID admission unconfirmed')
        return result


@contextmanager
def cleanup_lock(path,native_tool=None):
    pin=PinnedPath(path);path=pin.path
    path.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
    pin.check()
    parent=path.parent.stat()
    require(parent.st_uid==os.getuid() and not parent.st_mode&0o022,'Unsafe cleanup PID directory')
    directory=os.open(path.parent,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    owned=None
    try:
        require(identity(os.fstat(directory))==identity(parent),'Cleanup directory replaced')
        if native_tool:
            previous=read_pid(directory,path.name)
            if previous:require(not live(previous[0]),'Existing cleanup/build owns the shared cleanup PID')
            result=subprocess.run([native_tool,'-p',str(os.getpid()),'-f',str(path)],capture_output=True,timeout=10)
            require(result.returncode==0,'Existing cleanup/build owns the shared cleanup lock')
            owned=read_pid(directory,path.name)
            require(owned and owned[0]==os.getpid(),'Native cleanup admission unconfirmed')
        else:
            owned=fallback_acquire(directory,path.name)
        pin.check()
        require(identity(path.parent.stat())==identity(parent),'Cleanup directory replaced before entry')
        yield
    finally:
        try:
            if owned:
                # The FD remains bound to the checked directory. Never unlink a
                # replacement owner's PID file or follow a replacement home alias.
                with directory_guard(directory):
                    if read_pid(directory,path.name)==owned:
                        os.unlink(path.name,dir_fd=directory)
        finally:
            os.close(directory)
