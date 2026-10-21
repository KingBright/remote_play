#!/usr/bin/env python3
"""Review/apply the narrow Linux cleanup adapter to the original installed helper.

The helper source hash is mandatory. Preserve its other code, version, original
PID-file location and project authority. This changes code, never OS permissions.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat


BODY='''@contextlib.contextmanager
def cleanup_lock(enabled=True):
    """Same original PID-file scope, with trusted-home/Linux compatibility."""
    if not enabled:
        yield
        return
    adapter=companion('rust_posix_lock')
    try:
        with adapter.cleanup_lock(GLOBAL_LOCK,native_tool=shutil.which('shlock')):
            yield
    except adapter.LockRefused as error:
        raise Refusal(str(error)) from error


'''

LOCK='''@contextlib.contextmanager
def lock(project, global_cleanup=False):
    root, policy, registry = load(project)
    with (root / STATE / 'lock').open('r') as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Refusal('project build/registration/cleanup lock is held')
        try:
            with cleanup_lock(global_cleanup):
                yield load(root)
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


'''


def sha(raw):return hashlib.sha256(raw).hexdigest()


def patched(raw):
    text=raw.decode()
    if '@contextlib.contextmanager\ndef cleanup_lock(' not in text:
        # HO5 retains the earlier helper with the same PID protocol inlined in
        # lock(). Upgrade only that checked function, retaining all companions.
        start=text.index('@contextlib.contextmanager\ndef lock(')
        end=text.index('def artifact_path(',start)
        previous=text[start:end]
        if not all(x in previous for x in ['owned = False','safe_absolute(GLOBAL_LOCK)',
                "shutil.which('shlock')",'GLOBAL_LOCK.unlink()','yield load(root)']):
            raise ValueError('Unrecognized inline cleanup lock; review exact source')
        return (text[:start]+BODY+LOCK+text[end:]).encode()
    start=text.index('@contextlib.contextmanager\ndef cleanup_lock(')
    end=text.index('@contextlib.contextmanager\ndef lock(',start)
    previous=text[start:end]
    if "adapter=companion('rust_posix_lock')" in previous:return raw
    if not all(x in previous for x in ['safe_absolute(GLOBAL_LOCK)',"shutil.which('shlock')",'GLOBAL_LOCK.unlink()']):
        raise ValueError('Unrecognized original cleanup lock; review exact source')
    return (text[:start]+BODY+text[end:]).encode()


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--expected-helper-sha256',required=True)
    p.add_argument('--apply',action='store_true');args=p.parse_args()
    directory=Path.home().resolve()/'.codex/skills/rust-project-lifecycle/scripts'
    helper=directory/'rust_storage.py';before=helper.read_bytes();info=helper.lstat()
    if helper.is_symlink() or info.st_uid!=os.getuid() or info.st_mode&0o022 or sha(before)!=args.expected_helper_sha256:
        raise ValueError('Installed helper identity/ownership drift; no write')
    companion=Path(__file__).with_name('rust_posix_lock.py').read_bytes();after=patched(before)
    result={'helper':str(helper),'before_sha256':sha(before),'after_sha256':sha(after),
        'companion_sha256':sha(companion),'applied':False,'global_pid_path_preserved':True}
    if args.apply:
        target=directory/'rust_posix_lock.py'
        if target.exists():
            if target.is_symlink() or target.read_bytes()!=companion:raise ValueError('Existing companion differs; no overwrite')
        else:
            with target.open('xb') as output:output.write(companion);output.flush();os.fsync(output.fileno())
        pending=directory/'.rust_storage.py.linux.pending'
        fd=os.open(pending,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,stat.S_IMODE(info.st_mode))
        os.fchmod(fd,stat.S_IMODE(info.st_mode))  # Preserve the original mode despite umask.
        with os.fdopen(fd,'wb') as out:out.write(after);out.flush();os.fsync(out.fileno())
        if helper.read_bytes()!=before or helper.lstat().st_ino!=info.st_ino:
            raise ValueError('Helper changed during review; pending preserved')
        os.replace(pending,helper);result['applied']=True
    print(json.dumps(result,indent=2))


if __name__=='__main__':main()
