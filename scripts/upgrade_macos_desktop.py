#!/usr/bin/env python3
"""Version-bound upgrade of the installed independent Desktop app.

Uses the original signer/archive verifier and normal application/service quit.
The retained client, profile, launch policy and permissions are never migrated.
An interrupted transaction stays inspectable; apply never replays it.
"""
import argparse
from contextlib import contextmanager, closing
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import select
import shutil
import stat
import subprocess
import sys
import time

import build_identity_gate as gate
import desktop_identity as desktop
import install_macos_desktop as fresh
import macos_release_guard as guard

LIMIT = 65536
PHASES = ('verified', 'staged', 'stopping', 'stopped', 'exchanging',
          'exchanged', 'activated', 'registry_committed', 'completed')


def require(ok, text):
    if not ok:
        raise guard.ReleaseRejected(text)


def private(path, directory=False):
    info = path.lstat()
    require((stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode))
            and info.st_uid == os.getuid() and not info.st_mode & (0o077 if directory else 0o077)
            and (directory or info.st_nlink == 1), 'Private owned installation metadata required')
    return info


def owned_layout(home):
    require(os.getuid() != 0 and os.geteuid() != 0, 'Use the logged-in user')
    selected = desktop.paths(home)
    for path in (selected['app'], selected['profile'], selected['state'], selected['plist']):
        for parent in (path, *path.parents):
            if parent == Path(home).parent:
                break
            info = parent.lstat()
            require(not stat.S_ISLNK(info.st_mode) and info.st_uid == os.getuid()
                    and not info.st_mode & 0o022, 'Linked or writable installation destination')
    for key in ('profile', 'state'):
        private(selected[key], True)
    private(selected['plist'])
    return selected


def read_record(path):
    info = private(path)
    require(info.st_size <= LIMIT, 'Installation record exceeds limit')
    with path.open('rb') as source:
        raw = source.read(LIMIT + 1)
    require(len(raw) <= LIMIT and path.lstat().st_ino == info.st_ino,
            'Installation record changed while reading')
    data = json.loads(raw)
    require(isinstance(data, dict), 'Installation record must be an object')
    return data, hashlib.sha256(raw).hexdigest()


def atomic_record(path, data, expected):
    raw = (json.dumps(data, indent=2) + '\n').encode()
    require(len(raw) <= LIMIT, 'Installation history exceeds limit; nothing was discarded')
    actual = gate.hash_file(path) if path.exists() else None
    require(actual == expected and not path.is_symlink(), 'Installation record version conflict')
    temp = path.with_name(path.name + '.upgrade-tmp')
    fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as target:
        target.write(raw); target.flush(); os.fsync(target.fileno())
    require((gate.hash_file(path) if path.exists() else None) == expected,
            'Installation record changed before commit; inspect pending transaction')
    os.replace(temp, path)
    fsync(path.parent)
    return hashlib.sha256(raw).hexdigest()


def fsync(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def membership_metadata(profile):
    result = []
    for name in ('mesh.conf', 'mesh.secret'):
        m = private(profile / name)
        result.append((m.st_dev, m.st_ino, m.st_size, m.st_mtime_ns,
                       m.st_ctime_ns, stat.S_IMODE(m.st_mode), m.st_uid))
    return result  # Metadata only: never read credential bodies.


def tree_digest(path):
    return gate.digest(gate.canonical(gate.files_in(path)))


def product_current(executable, record, signed):
    source = record['source']
    env = clean_environment(Path(record['paths']['profile']))
    result = subprocess.run([str(executable), '--product-info-json'], env=env,
                            capture_output=True, timeout=15)
    require(result.returncode == 0 and len(result.stdout) <= LIMIT,
            'Installed product identity unavailable')
    info = json.loads(result.stdout)
    identity = gate.validate_identity(info['build_identity'], source['commit'], source['identity_sha256'])
    gate.validate_product_info(info, identity)
    require(info.get('desktop_identity') == desktop.identity()
            and gate.hash_file(executable) == signed['executable_sha256'],
            'Installed executable differs from its independent identity')
    return info


def validate_policy(data, selected):
    executable = str(selected['app'] / 'Contents/MacOS/weavelo')
    require(data.get('Label') == desktop.identity()['service_label']
            and data.get('ProgramArguments') == [executable, '--background-service']
            and data.get('Program', executable) == executable,
            'Original independent launch identity required')
    require(data.get('EnvironmentVariables', {}).get('WEAVELO_DEVICE_GROUP_DIR') == str(selected['profile']),
            'Launch policy selects another profile')
    require(not set(data).intersection(('StartInterval', 'StartCalendarInterval', 'WatchPaths',
                                       'QueueDirectories', 'StartOnMount', 'LaunchEvents',
                                       'Sockets', 'MachServices')), 'External launch activation requires review')
    keep = data.get('KeepAlive', False)
    require(keep is False or (isinstance(keep, dict) and bool(keep)
            and all((k == 'SuccessfulExit' and v is False) or (k == 'Crashed' and v is True)
                    for k, v in keep.items())), 'Normal exit must not restart the old owner')


def inspect(home, expected=None, transaction_sha=None):
    selected = owned_layout(home)
    record_path = selected['state'] / 'install.json'
    record, record_sha = read_record(record_path)
    require(record.get('application_id') == desktop.identity()['application_id']
            and record.get('applied') is True
            and record.get('phase') in ('committed_not_started', 'upgrade_completed'),
            'Original completed independent installation required')
    for key in ('app', 'profile', 'plist', 'state'):
        require(record.get('paths', {}).get(key) == str(selected[key]), 'Installation path binding changed')
    transaction = selected['state'] / 'upgrade.json'
    previous = None
    if transaction.exists() or transaction.is_symlink():
        previous, previous_sha = read_record(transaction)
        require(previous.get('schema') == 1 and (previous.get('phase') == 'completed'
                or (transaction_sha is not None and previous_sha == transaction_sha)), 
                'Existing upgrade is incomplete; inspect --status, never replay apply')
    plist_sha = gate.hash_file(selected['plist'])
    validate_policy(plistlib.loads(selected['plist'].read_bytes()), selected)
    policy = guard.load_policy(guard.DESKTOP_POLICY_FILE)
    signed = guard.verify_app(selected['app'], policy)
    executable = selected['app'] / 'Contents/MacOS/weavelo'
    product = product_current(executable, record, signed)
    actual = {'executable': signed['executable_sha256'], 'install': record_sha, 'plist': plist_sha}
    if expected is not None:
        require(actual == {k: gate.sha(v) for k, v in expected.items()}, 'Current installation version conflict')
    return {'paths': selected, 'record': record, 'record_path': record_path, 'signed': signed,
            'product': product, 'pins': actual, 'membership': membership_metadata(selected['profile']),
            'tree': tree_digest(selected['app']), 'previous_transaction': previous}


def clean_environment(profile):
    env = {k: v for k, v in os.environ.items() if not k.startswith('WEAVELO_') and k != 'RUST_LOG'}
    env.update(WEAVELO_DEVICE_GROUP_DIR=str(profile), WEAVELO_HEADLESS='1')
    return env


class NativeControl:
    def __init__(self, selected):
        self.paths = selected
        self.exe = selected['app'] / 'Contents/MacOS/weavelo'
        self.domain = 'gui/' + str(os.getuid())
        self.job = self.domain + '/' + desktop.identity()['service_label']

    def query(self, flag, required=True):
        result = subprocess.run([str(self.exe), flag], env=clean_environment(self.paths['profile']),
                                capture_output=True, timeout=12)
        if result.returncode:
            require(not required, 'Native application command refused: ' + flag)
            return None
        require(len(result.stdout) <= LIMIT, 'Native status exceeds limit')
        return json.loads(result.stdout)

    def loaded(self):
        r = subprocess.run(['/bin/launchctl', 'print', self.job], capture_output=True, text=True, timeout=10)
        if r.returncode:
            require('Could not find service' in r.stderr, 'Cannot determine original launchd job')
            return False
        program = re.search(r'^\s*program = (.+)$', r.stdout, re.M)
        require(program and program[1].strip() == str(self.exe), 'Loaded job points to another executable')
        return True

    def owners(self):
        raw = subprocess.check_output(['/bin/ps', '-axo', 'pid=,comm='], text=True, timeout=10)
        return {int(parts[0]) for line in raw.splitlines()
                if len(parts := line.strip().split(None, 1)) == 2 and parts[1] == str(self.exe)}

    def before(self, expected_sha):
        background = self.query('--background-status-json', False)
        gui = self.query('--gui-network-diagnostics-json', False)
        pids = set()
        if background:
            require(background.get('role') == 'background-service'
                    and background.get('executable_sha256') == expected_sha
                    and background.get('profile') == str(self.paths['profile']),
                    'Unexpected background owner')
            require(type(background.get('pid')) is int and background['pid'] > 1,
                    'Authenticated background process id required')
            pids.add(background['pid'])
        if gui and gui.get('ok') is True:
            require(gui.get('executable_sha256') == expected_sha, 'Unexpected GUI owner')
            require(type(gui.get('pid')) is int and gui['pid'] > 1,
                    'Authenticated GUI process id required')
            pids.add(gui['pid'])
        else:
            gui = None
        require(self.owners() == pids, 'Unidentified independent process; no process was stopped')
        return {'background': background, 'gui': gui, 'loaded': self.loaded()}

    def stop(self, before):
        gui = before['gui']
        if gui:
            normal_gui_quit(gui['pid'], self.exe)
        background = self.query('--background-status-json', False)
        if background:
            require(before['background'] and background.get('pid') == before['background']['pid'],
                    'Background owner changed during quit')
            stopped = self.query('--background-stop-json')
            require(stopped.get('cleanup_verified') is True and stopped.get('process_exit_observed') is True,
                    'Native background exit was not confirmed')
        require(not self.owners(), 'Independent process remains; application was not replaced')

    def start(self, before, expected_sha):
        if self.loaded():
            subprocess.run(['/bin/launchctl', 'kickstart', self.job], check=True,
                           capture_output=True, timeout=15)
        else:
            subprocess.run(['/bin/launchctl', 'bootstrap', self.domain, str(self.paths['plist'])],
                           check=True, capture_output=True, timeout=15)
        deadline = time.monotonic() + 40
        while True:
            status = self.query('--background-status-json', False)
            if status and status.get('role') == 'background-service' and status.get('executable_sha256') == expected_sha:
                break
            require(time.monotonic() < deadline, 'Updated background did not become ready')
            time.sleep(.5)
        pairing = self.query('--pairing-status-json')
        require(pairing.get('schema') == 1 and pairing.get('accepted') is True,
                'Running executable does not expose the new native pairing CLI')
        if before['gui']:
            subprocess.run(['/usr/bin/open', '-n', str(self.paths['app']), '--args', '--gui'],
                           check=True, capture_output=True, timeout=15)
            while True:
                gui = self.query('--gui-network-diagnostics-json', False)
                if gui and gui.get('ok') is True and gui.get('executable_sha256') == expected_sha:
                    require(gui.get('pid') != status.get('pid'), 'GUI and background ownership collided')
                    break
                require(time.monotonic() < deadline, 'Updated GUI did not become ready')
                time.sleep(.5)
        else:
            gui = None
        old = ((before['background'] or {}).get('network_diagnostics') or {}).get('network') or {}
        new = (status.get('network_diagnostics') or {}).get('network') or {}
        groups = lambda n: {k: (n.get('relay_groups') or {}).get(k)
                            for k in ('control_sha256', 'discovery_sha256')}
        require(before['background'] is None or groups(old) == groups(new),
                'Runtime membership changed across application upgrade')
        return {'background_pid': status['pid'], 'frontend_pid': gui.get('pid') if gui else None,
                'executable_sha256': expected_sha, 'native_pairing_cli_verified': True,
                'runtime_membership_preserved': True}


@contextmanager
def process_exit_watch(pid):
    # kqueue has close(), but does not implement Python's context protocol.
    with closing(select.kqueue()) as watch:
        event = select.kevent(pid, filter=select.KQ_FILTER_PROC,
                              flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                              fflags=select.KQ_NOTE_EXIT)
        watch.control([event], 0, 0)
        yield watch


def normal_gui_quit(pid, executable):
    # A kernel exit watch is registered before the normal AppKit quit request.
    # No AppleScript, input events, forced termination or permission repair.
    import ctypes.util
    lib = ctypes.CDLL('/usr/lib/libproc.dylib')
    lib.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    lib.proc_pidpath.restype = ctypes.c_int
    def path():
        buf = ctypes.create_string_buffer(4096)
        return os.fsdecode(buf.value) if lib.proc_pidpath(pid, buf, len(buf)) > 0 else None
    require(path() == str(executable), 'GUI process identity changed before quit')
    with process_exit_watch(pid) as watch:
        require(path() == str(executable), 'GUI process identity changed before quit watch')
        ctypes.CDLL('/System/Library/Frameworks/AppKit.framework/AppKit')
        objc = ctypes.CDLL('/usr/lib/libobjc.A.dylib')
        objc.objc_getClass.argtypes = [ctypes.c_char_p]; objc.objc_getClass.restype = ctypes.c_void_p
        objc.sel_registerName.argtypes = [ctypes.c_char_p]; objc.sel_registerName.restype = ctypes.c_void_p
        cls = objc.objc_getClass(b'NSRunningApplication')
        application = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int)(
            ('objc_msgSend', objc))(cls, objc.sel_registerName(b'runningApplicationWithProcessIdentifier:'), pid)
        require(application, 'Exact GUI application unavailable')
        accepted = ctypes.CFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)(
            ('objc_msgSend', objc))(application, objc.sel_registerName(b'terminate'))
        require(accepted, 'Application declined normal quit; no force termination attempted')
        require(bool(watch.control(None, 1, 30)), 'Normal GUI exit not observed; no application replacement')


@contextmanager
def idle_locks(profile):
    held = []
    try:
        for path in (profile / 'desktop-instance.lock', profile / 'gui-state/desktop-instance.lock'):
            if not path.exists():
                continue
            private(path)
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            require(os.fstat(fd).st_ino == path.lstat().st_ino, 'Profile lock changed')
            held.append(fd)
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield
    finally:
        for fd in held:
            os.close(fd)


def exchange(source, target):
    native = ctypes.CDLL('/usr/lib/libSystem.B.dylib', use_errno=True)
    native.renamex_np.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
    native.renamex_np.restype = ctypes.c_int
    if native.renamex_np(os.fsencode(source), os.fsencode(target), 2):
        raise OSError(ctypes.get_errno(), 'Atomic independent application exchange failed')
    fsync(target.parent)


def apply(archive, home, candidate_pins, archive_sha, current_pins, resume_sha=None):
    require(gate.hash_file(archive) == gate.sha(archive_sha), 'Candidate archive digest differs')
    policy = guard.load_policy(guard.DESKTOP_POLICY_FILE)
    with guard.verified_archive(archive, policy, identity_expectations=candidate_pins) as (candidate, signed):
        fresh.verified_product(candidate, archive, candidate_pins)
        if resume_sha is not None:
            resume_sha = gate.sha(resume_sha)
        context = inspect(home, current_pins, transaction_sha=resume_sha)
        selected = context['paths']; registry = context['record_path']
        if resume_sha is not None:
            prior = context['previous_transaction']
            require(prior and prior.get('phase') == 'staged' and prior.get('source') == candidate_pins
                    and prior.get('archive_sha256') == archive_sha and prior.get('current') == current_pins,
                    'Only the exact verified pre-stop staged transaction may resume')
        decision = guard.check_upgrade(context['signed'], signed)
        if decision == 'already_installed':
            return {'applied': False, 'outcome': 'already_installed', 'source': candidate_pins}
        lock = selected['state'].parent / '.desktop-install.lock'
        fd = os.open(lock, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'r+') as held:
            private(lock); require(os.fstat(held.fileno()).st_ino == lock.lstat().st_ino, 'Installer lock changed')
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            context = inspect(home, current_pins, transaction_sha=resume_sha)
            stage = selected['app'].parent / '.desktop-upgrade.pending'
            require(resume_sha is not None or (not stage.exists() and not stage.is_symlink()),
                    'Prior upgrade staging exists; inspect original transaction')
            journal = selected['state'] / 'upgrade.json'
            prior_journal_sha = gate.hash_file(journal) if journal.exists() else None
            if resume_sha is not None:
                data, observed_sha = read_record(journal)
                require(observed_sha == resume_sha and data.get('phase') == 'staged'
                        and data.get('kind') == 'independent-desktop-upgrade'
                        and data.get('staging') == str(stage) and data.get('source') == candidate_pins
                        and data.get('archive_sha256') == archive_sha and data.get('current') == current_pins,
                        'Staged transaction version or bindings changed')
                private(stage, True)
                data['resumed_from_sha256'] = resume_sha
            else:
                data = {'schema': 1, 'kind': 'independent-desktop-upgrade', 'source': candidate_pins,
                        'archive_sha256': archive_sha, 'current': current_pins,
                        'staging': str(stage), 'retained_client_modified': False}
            def checkpoint(phase):
                nonlocal prior_journal_sha
                require(phase in PHASES, 'Unknown transaction phase')
                data['phase'] = phase
                prior_journal_sha = atomic_record(journal, data, prior_journal_sha)
            new_record = dict(context['record'])
            previous = dict(new_record); previous.pop('history', None)
            require(isinstance(new_record.get('history', []), list), 'Unknown installation history format')
            history = list(new_record.get('history', []))
            history.append(previous)
            new_record.update(history=history, source=candidate_pins, archive_sha256=archive_sha,
                              phase='upgrade_completed', service_started=True,
                              launch_agent_preserved_sha256=current_pins['plist'])
            require(len(json.dumps(new_record).encode()) < LIMIT - 4096, 'Installation history full; no content discarded')
            staged = stage / 'Weavelo.app'
            if resume_sha is None:
                checkpoint('verified')
                stage.mkdir(mode=0o700)
                shutil.copytree(candidate, staged, symlinks=False)
            guard.verify_app(staged, policy)
            candidate_tree = tree_digest(candidate)
            require(tree_digest(staged) == candidate_tree, 'Staged signed application differs')
            checkpoint('staged')
            control = NativeControl(selected)
            before = control.before(current_pins['executable'])
            data['owners_before'] = {'background_pid': (before.get('background') or {}).get('pid'),
                                     'frontend_pid': (before.get('gui') or {}).get('pid')}
            checkpoint('stopping')
            control.stop(before)
            checkpoint('stopped')
            with idle_locks(selected['profile']):
                latest = inspect(home, current_pins, transaction_sha=prior_journal_sha)
                require(latest['membership'] == context['membership'] and latest['tree'] == context['tree'],
                        'Installation or membership changed before exchange')
                require(gate.hash_file(archive) == archive_sha, 'Candidate archive changed')
                checkpoint('exchanging')
                exchange(staged, selected['app'])
                checkpoint('exchanged')
                require(tree_digest(selected['app']) == candidate_tree
                        and tree_digest(staged) == context['tree'], 'Application exchange version conflict; retain stage')
                guard.verify_app(selected['app'], policy)
                require(membership_metadata(selected['profile']) == context['membership']
                        and gate.hash_file(selected['plist']) == current_pins['plist'], 'Profile or launch policy changed')
            runtime = control.start(before, signed['executable_sha256'])
            data['runtime'] = runtime
            checkpoint('activated')
            require(membership_metadata(selected['profile']) == context['membership']
                    and gate.hash_file(selected['plist']) == current_pins['plist'],
                    'Membership or launch policy changed during activation')
            new_record['runtime'] = runtime
            new_record['signed_application_verified'] = True
            atomic_record(registry, new_record, current_pins['install'])
            checkpoint('registry_committed')
            require(tree_digest(staged) == context['tree'] and tree_digest(selected['app']) == candidate_tree,
                    'Owned old staging changed; cleanup refused')
            shutil.rmtree(stage)
            checkpoint('completed')
            return dict(data, applied=True, profile_preserved=True, launch_policy_preserved=True,
                        installation_sha256=gate.hash_file(registry))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', nargs='?', type=Path)
    parser.add_argument('--apply', action='store_true')
    parser.add_argument('--status', action='store_true')
    parser.add_argument('--resume-staged', action='store_true')
    parser.add_argument('--expected-upgrade-sha256')
    for name in ('commit', 'identity-sha256', 'manifest-sha256', 'archive-sha256',
                 'current-executable-sha256', 'current-install-sha256', 'current-plist-sha256'):
        parser.add_argument('--expected-' + name)
    args = parser.parse_args(); home = Path.home()
    require(not (args.status and args.apply), 'Choose status or apply')
    require((not args.resume_staged and not args.expected_upgrade_sha256)
            or (args.apply and args.resume_staged and args.expected_upgrade_sha256),
            'Explicit staged resume requires apply and the observed upgrade-record digest')
    if args.status:
        path = desktop.paths(home)['state'] / 'upgrade.json'
        result = read_record(path)[0] if path.exists() else {'phase': 'not_started'}
    elif args.apply:
        require(sys.platform == 'darwin', 'Native macOS upgrade required')
        pins = {'commit': args.expected_commit, 'identity_sha256': args.expected_identity_sha256,
                'manifest_sha256': args.expected_manifest_sha256}
        current = {'executable': args.expected_current_executable_sha256,
                   'install': args.expected_current_install_sha256, 'plist': args.expected_current_plist_sha256}
        require(args.archive and all(pins.values()) and args.expected_archive_sha256
                and all(current.values()), 'Candidate and current independent version pins required')
        result = apply(args.archive, home, pins, args.expected_archive_sha256, current,
                       args.expected_upgrade_sha256 if args.resume_staged else None)
    else:
        c = inspect(home)
        result = {'kind': 'independent-desktop-upgrade-plan', 'applied': False,
                  'current': c['pins'], 'version': c['signed']['version'], 'build': c['signed']['build'],
                  'source': c['record']['source'], 'profile_preserved': True, 'launch_policy_preserved': True}
    print(json.dumps(result, indent=2))


def cli():
    try:
        main()
    except (guard.ReleaseRejected, gate.IdentityRejected, OSError, ValueError, KeyError,
            subprocess.SubprocessError) as error:
        print(json.dumps({'applied': None if '--apply' in sys.argv else False,
                          'outcome': 'not_confirmed' if '--apply' in sys.argv else 'not_applied',
                          'error': type(error).__name__,
                          'detail': str(error) if isinstance(error, guard.ReleaseRejected) else 'Required evidence unavailable',
                          'next_action': 'Read --status and actual state; do not replay an incomplete upgrade'}),
              file=sys.stderr)
        sys.exit(1)


if __name__ == '__main__':
    cli()
