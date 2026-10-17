#!/usr/bin/env python3
"""Original-install-journal Linux lifecycle executor. No credential generation.

Every effect has a persisted intent. Duplicate apply never replays unfinished
work; explicit recovery inspects the same unit, staged inode and original journal.
The native backend uses only the installed fixed binary and user systemd unit.
"""
import argparse
import copy
import ctypes
import fcntl
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import time

import linux_background_lifecycle_plan as policy

KEY = 'linux_lifecycle'


def checked_plan(plan, expected):
    value = dict(plan); supplied = value.pop('plan_sha256', None)
    policy.require(supplied == expected == policy.digest(value), 'Reviewed plan digest differs')
    policy.require(plan['kind'] == 'linux-same-profile-background-migration-plan' and plan['applied'] is False,
                   'Expected original unapplied lifecycle proposal')


def execute(backend, plan, expected, recover=False):
    checked_plan(plan, expected)
    root = backend.load()
    policy.require(root.get('phase') == 'installed_and_running_verified', 'Original installed journal required')
    state = root.get(KEY)
    if state:
        policy.require(state['plan_sha256'] == expected, 'Different existing lifecycle transaction')
        if state['phase'] in ('committed', 'rolled_back'):
            backend.verify_terminal(plan, state)
            return copy.deepcopy(state)
        policy.require(recover, 'Unfinished request; inspect/recover original, never replay apply')
    else:
        policy.require(not recover, 'No original lifecycle transaction to recover')
        backend.preflight(plan)
        state = {'plan_sha256': expected, 'phase': 'prepared',
                 'before_unit_utf8': backend.unit_bytes().decode(), 'native_gui_close_verified': False}

    def save(phase):
        state['phase'] = phase; root[KEY] = copy.deepcopy(state); backend.save(root)

    def rollback():
        backend.rollback(plan, state)
        save('rolled_back')

    if recover:
        rollback()
        return copy.deepcopy(state)
    save('prepared')
    try:
        state['stage'] = backend.stage(plan)
        save('stop_requested')
        backend.stop_old(plan)
        save('old_stopped')
        save('exchange_requested')
        state['after_unit'] = backend.publish(plan, state)
        save('unit_published')
        save('background_start_requested')
        state['background'] = backend.start_background(plan, state)
        save('background_verified')
        save('gui_test_requested')
        state['gui_test'] = backend.gui_test(plan, state, lambda: save('gui_started'))
        backend.verify_terminal(plan, dict(state, phase='committed'))
        state['native_gui_close_verified'] = True
        save('committed')
    except Exception:
        # Do not print application output/credentials. Drift during recovery
        # leaves the original intent and evidence; never declare a rollback.
        try:
            rollback()
        except Exception:
            save('recovery_required')
            raise RuntimeError('Recovery incomplete; preserve original journal and inspect owned effects') from None
        raise RuntimeError('Cutover failed; original owner restored; same request is not replayable') from None
    return copy.deepcopy(state)


def checked_installed_product(gate, executable, expected, binary_sha256):
    # A proposal cannot upgrade a binary's actual compiled capability claim.
    info = gate.product_info(executable, expected, binary_sha256)
    policy.require_separated_entries(info)
    return info


class Native:
    def __init__(self, plan):
        policy.require(sys.platform.startswith('linux'), 'Native Linux required')
        import build_identity_gate as gate
        from install_native_first_identity import PINS, COMMIT
        self.gate = gate; self.plan = plan; self.home = Path.home().resolve(strict=True)
        policy.require(str(self.home) == plan['home'] and plan['source_commit'] == COMMIT and
                       plan['binary_sha256'] == PINS['linux']['binary'], 'Existing fixed installation only')
        self.exe = Path(plan['executable']); self.profile = Path(plan['profile'])
        policy.require(self.exe == self.home/'.local/opt/weavelo/weavelo' and
                       self.profile == self.home/'.config/weavelo/mesh', 'Canonical existing installation paths required')
        self.work = self.home/'.local/state/weavelo-identity-migration'
        self.journal = self.work/'journal.json'
        self.unit = self.home/'.config/systemd/user/weavelo.service'
        self.pending = self.unit.with_name('.weavelo.service.lifecycle.pending')
        self.marker = self.profile/policy.MARKER
        self.gui_receipt = self.work/'linux-lifecycle-gui-receipt.json'
        for path in (self.work, self.profile):
            m = path.lstat(); policy.require(not path.is_symlink() and stat.S_ISDIR(m.st_mode) and
                m.st_uid == os.getuid() and stat.S_IMODE(m.st_mode) == 0o700, 'Existing private installation directory required')
        self.file_state(self.journal)
        policy.require(all(hasattr(os, name) for name in ('listxattr', 'getxattr', 'setxattr')), 'Native security metadata APIs required')
        policy.require(hasattr(os, 'pidfd_open') and hasattr(signal, 'pidfd_send_signal'), 'Held process identity required')
        self.libc = ctypes.CDLL(None, use_errno=True)
        policy.require(hasattr(self.libc, 'renameat2'), 'Atomic unit exchange unavailable')
        self.rename = self.libc.renameat2
        self.rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        receipt = Path(__file__).resolve().parents[2]/'.remoteplay-lifecycle-helper-current/weavelo-linux-observed-build-receipt.json'
        policy.require(gate.hash_file(receipt) == PINS['linux']['receipt'], 'Original observed receipt drift')
        expected = gate.read_json(receipt)['build_identity']
        gate.validate_identity(expected, COMMIT, PINS['linux']['identity'])
        checked_installed_product(gate, self.exe, expected, PINS['linux']['binary'])
        self.lock_fd = None

    def __enter__(self):
        path = self.work/'lifecycle.lock'
        self.lock_fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        m = os.fstat(self.lock_fd)
        policy.require(stat.S_ISREG(m.st_mode) and m.st_uid == os.getuid() and m.st_nlink == 1 and
                       stat.S_IMODE(m.st_mode) == 0o600, 'Unknown lifecycle lock')
        fcntl.flock(self.lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return self

    def __exit__(self, *unused):
        if self.lock_fd is not None: os.close(self.lock_fd)

    def file_state(self, path, marker=False):
        if not path.exists() and not path.is_symlink(): return None
        self.gate.regular(path)
        m = path.lstat()
        policy.require(m.st_uid == os.getuid() and m.st_nlink == 1, 'Unknown ordinary file owner')
        value = {'device': m.st_dev, 'inode': m.st_ino, 'uid': m.st_uid,
                 'mode': stat.S_IMODE(m.st_mode), 'links': m.st_nlink, 'sha256': self.gate.hash_file(path)}
        if marker: value.update(path=str(path), size=m.st_size)
        return value

    def security(self, path):
        m = path.lstat()
        return policy.digest({'uid': m.st_uid, 'gid': m.st_gid, 'mode': stat.S_IMODE(m.st_mode),
            'xattrs': {name: policy.sha(os.getxattr(path, name)) for name in sorted(os.listxattr(path))}})

    def profile_state(self):
        m = self.profile.lstat()
        return {'directory': {'device': m.st_dev, 'inode': m.st_ino, 'uid': m.st_uid, 'mode': stat.S_IMODE(m.st_mode)},
                'files': {n: self.file_state(self.profile/n) for n in ('mesh.conf', 'mesh.secret')}}

    def load(self):
        self.journal_state = self.file_state(self.journal)
        policy.require(self.journal_state and self.journal_state['mode'] == 0o600, 'Original private journal required')
        self.root = self.gate.read_json(self.journal)
        return copy.deepcopy(self.root)

    def save(self, root):
        from install_native_first_identity import atomic_state
        policy.require(self.file_state(self.journal) == self.journal_state, 'Original journal changed; preserve it')
        atomic_state(self.journal, root)
        self.journal_state = self.file_state(self.journal); self.root = copy.deepcopy(root)

    def unit_bytes(self): return self.unit.read_bytes()

    def ctl(self, action):
        subprocess.run(['systemctl', '--user', action, 'weavelo.service'], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=25)

    def service(self):
        raw = subprocess.check_output(['systemctl', '--user', 'show', 'weavelo.service',
            '-p', 'MainPID', '-p', 'DropInPaths'], text=True, timeout=8)
        fields = dict(x.split('=', 1) for x in raw.splitlines())
        policy.require(not fields['DropInPaths'], 'Unknown unit drop-in')
        return int(fields['MainPID'])

    def snapshot(self):
        from acceptance.inspect_connection_lifecycle import request
        return request(self.profile, 'Snapshot')['Snapshot']

    def gui_status(self):
        result = subprocess.run([str(self.exe), '--gui-status-json'], env=self.environment(),
                                capture_output=True, text=True, timeout=8)
        policy.require(result.returncode == 0, 'Existing GUI status unavailable')
        value = json.loads(result.stdout); sessions = value.get('sessions')
        policy.require(value.get('ok') is True and not value.get('lease') and
            isinstance(sessions, dict) and 'sessions' in sessions and 'pending' in sessions and
            not sessions['sessions'] and not sessions['pending'],
            'GUI busy or session observation unavailable')
        return value

    def native(self, pid):
        return self.gate.verify_running_linux(pid, self.exe, self.plan['binary_sha256'])

    def same_profile(self):
        policy.require(self.profile_state() == self.plan['profile_state'], 'Original identity/profile drift')

    def guard(self):
        self.same_profile()
        policy.require(self.file_state(self.journal) == self.journal_state, 'Original journal changed; no effects')
        policy.require(self.gate.hash_file(self.exe) == self.plan['binary_sha256'], 'Installed executable drift')

    def preflight(self, plan):
        self.same_profile()
        policy.require(plan.get('unit_security_sha256') == self.security(self.unit),
            'Fresh reviewed unit uid/gid/mode/xattr security pin required')
        policy.require(policy.rewrite_unit(self.unit_bytes(), str(self.exe), str(self.profile)).decode() == plan['unit_after_utf8'],
                       'Reviewed after-unit differs from narrow original producer policy')
        policy.require(self.file_state(self.unit) == plan['unit_state'] and self.file_state(self.pending) is None and
            self.file_state(self.marker) is None, 'Unit, stage or role-marker drift')
        policy.require(self.service() == plan['observation']['pid'], 'Original owner changed')
        self.native(self.service()); snapshot = self.snapshot(); self.gui_status()
        policy.require(snapshot['owner_mode'] == 'Standalone' and snapshot['generation'] == plan['observation']['generation'], 'Original generation changed')
        enabled = subprocess.run(['systemctl', '--user', 'is-enabled', 'weavelo.service'], capture_output=True, timeout=8).returncode == 0
        policy.require(enabled == plan['unit_enabled_after'], 'Startup intent changed')
        policy.require(not any(p.exists() or p.is_symlink() for p in
            (self.gui_receipt, self.gui_receipt.with_suffix('.progress.json'))), 'Existing GUI acceptance evidence preserved')

    def stage(self, plan):
        self.preflight(plan)
        fd = os.open(self.pending, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, plan['unit_state']['mode'])
        with os.fdopen(fd, 'wb') as out:
            out.write(plan['unit_after_utf8'].encode()); os.fchmod(out.fileno(), plan['unit_state']['mode']); out.flush(); os.fsync(out.fileno())
        policy.require(self.pending.stat().st_gid == self.unit.stat().st_gid, 'Group ownership needs separate review')
        # Preserve existing ACL/security labels rather than widening/replacing them.
        for name in os.listxattr(self.unit):
            value = os.getxattr(self.unit, name)
            if name not in os.listxattr(self.pending) or os.getxattr(self.pending, name) != value:
                os.setxattr(self.pending, name, value)
        policy.require(set(os.listxattr(self.pending)) == set(os.listxattr(self.unit)) and
            all(os.getxattr(self.pending, name) == os.getxattr(self.unit, name) for name in os.listxattr(self.unit)),
            'Unit ACL/security metadata cannot be preserved without permission changes')
        return self.file_state(self.pending)

    def stop_old(self, plan):
        self.guard(); policy.require(self.service() == plan['observation']['pid'], 'Owner changed before stop')
        self.native(self.service()); self.gui_status(); self.ctl('stop')
        policy.require(self.service() == 0, 'Original unit did not stop')

    def exchange(self):
        if self.rename(-100, os.fsencode(self.unit), -100, os.fsencode(self.pending), 2):
            raise OSError(ctypes.get_errno(), 'Unit exchange failed')
        fd = os.open(self.unit.parent, os.O_RDONLY | os.O_DIRECTORY)
        try: os.fsync(fd)
        finally: os.close(fd)

    def publish(self, plan, state):
        self.guard()
        policy.require(self.security(self.unit) == plan['unit_security_sha256'] and
            self.security(self.pending) == plan['unit_security_sha256'], 'Unit security metadata drift')
        policy.require(self.file_state(self.unit) == plan['unit_state'] and
            self.file_state(self.pending) == state['stage'], 'Unknown unit change before exchange')
        self.exchange()
        policy.require(self.file_state(self.unit) == state['stage'] and
            self.file_state(self.pending) == plan['unit_state'], 'Concurrent unit replacement; preserve both inodes')
        return self.file_state(self.unit)

    def start_background(self, plan, state):
        self.guard()
        policy.require(self.security(self.unit) == plan['unit_security_sha256'], 'Unit security changed before start')
        policy.require(self.file_state(self.unit) == state['stage'] and
            self.file_state(self.pending) == plan['unit_state'], 'Unit changed before background start')
        subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True, timeout=20)
        policy.require(self.file_state(self.marker) is None, 'Role marker appeared before start; preserve it')
        self.frontend_clear()
        self.ctl('start'); deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            try:
                pid = self.service(); self.native(pid); value = self.snapshot()
                policy.require(value['pid'] == pid and value['owner_mode'] == 'Background', 'Background not ready')
                self.same_profile(); marker = self.file_state(self.marker, True)
                policy.require(marker and marker['size'] == 0 and marker['mode'] == 0o600, 'Unknown role marker')
                return {'pid': pid, 'generation': value['generation'], 'marker': marker}
            except (OSError, ValueError, self.gate.IdentityRejected): time.sleep(.1)
        raise RuntimeError('Background readiness timed out; inspect original start intent')

    def environment(self):
        env = {k: v for k, v in os.environ.items() if not k.startswith(('WEAVELO_', 'REMOTE_PLAY_', 'RP_'))}
        env['WEAVELO_DEVICE_GROUP_DIR'] = str(self.profile)
        return env

    def gui_test(self, plan, state, persist):
        self.guard()
        self.frontend_clear()
        env = self.environment(); env.update(WEAVELO_RESTORED_TEST_SECONDS='5', WEAVELO_RESTORED_TEST_OUTPUT=str(self.gui_receipt))
        child = subprocess.Popen([str(self.exe), '--gui'], env=env, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        state['gui_pid'] = child.pid
        state['gui_start_ticks'] = self.start_ticks(child.pid); persist()
        deadline = time.monotonic() + 5
        while True:
            try: self.native(child.pid); break
            except (OSError, ValueError, self.gate.IdentityRejected):
                policy.require(child.poll() is None and time.monotonic() < deadline, 'Owned GUI image not verified')
                time.sleep(.05)
        policy.require(child.wait(timeout=15) == 0, 'Timed native GUI did not close normally')
        receipt = self.gate.read_json(self.gui_receipt)
        policy.require(receipt.get('render_calls', 0) > 0, 'No actual GUI render evidence')
        return {'pid': child.pid, 'native_image_verified': True, 'normal_exit': True,
                'render_calls': receipt['render_calls'], 'close_kind': 'existing bounded product GUI acceptance hook'}

    def verify_terminal(self, plan, state):
        self.same_profile()
        policy.require(self.security(self.unit) == plan['unit_security_sha256'], 'Terminal unit security metadata drift')
        if state['phase'] == 'committed':
            policy.require(self.file_state(self.unit) == state['after_unit'], 'Committed unit drift')
            policy.require(self.file_state(self.marker, True) == state['background']['marker'], 'Committed role marker changed')
            policy.require(self.service() == state['background']['pid'], 'Background PID changed during GUI close')
            self.native(self.service()); value = self.snapshot()
            policy.require(value['owner_mode'] == 'Background' and value['generation'] == state['background']['generation'], 'Background generation changed')
        else:
            policy.require(self.file_state(self.unit) == plan['unit_state'] and self.file_state(self.marker) is None, 'Restored unit/marker changed')
            self.native(self.service()); snapshot = self.snapshot()
            policy.require(snapshot['owner_mode'] == 'Standalone', 'Old role not restored')
            if state.get('restored'):
                policy.require({'pid': snapshot['pid'], 'generation': snapshot['generation']} == state['restored'], 'Restored owner subsequently changed')

    def start_ticks(self, pid):
        return Path('/proc', str(pid), 'stat').read_bytes().rsplit(b')', 1)[1].split()[19].decode()

    def frontend_clear(self):
        path = self.profile/'gui-state/desktop-instance.lock'
        if not path.exists():
            policy.require(not path.is_symlink(), 'Unknown frontend lock')
            return
        self.file_state(path)
        fd = os.open(path, os.O_RDWR | os.O_NOFOLLOW)
        try: fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finally: os.close(fd)

    def stop_gui(self, state):
        pid = state.get('gui_pid')
        if not pid: return
        try: fd = os.pidfd_open(pid)
        except ProcessLookupError: return
        try:
            policy.require(state.get('gui_start_ticks') == self.start_ticks(pid), 'GUI PID was reused or start identity missing')
            self.native(pid); signal.pidfd_send_signal(fd, signal.SIGTERM)
            import select
            policy.require(select.select([fd], [], [], 8)[0], 'Owned GUI did not stop')
        finally: os.close(fd)

    def rollback(self, plan, state):
        self.same_profile()
        policy.require(self.file_state(self.journal) == self.journal_state, 'Original journal changed; no recovery effects')
        unit = self.file_state(self.unit); pending = self.file_state(self.pending)
        policy.require(unit == plan['unit_state'] or
            (unit == state.get('stage') and pending == plan['unit_state']), 'Unknown recovery unit layout')
        policy.require(self.security(self.unit) == plan['unit_security_sha256'], 'Unknown unit security metadata')
        self.stop_gui(state)
        self.frontend_clear()
        if unit == plan['unit_state']:
            policy.require(pending is None or pending == state.get('stage'), 'Unknown pending unit preserved')
            policy.require(self.file_state(self.marker) is None, 'Unknown marker before background publication')
            policy.require(self.service() in (0, plan['observation']['pid']), 'Unknown original owner')
            if self.service():
                self.native(self.service()); policy.require(self.snapshot()['generation'] == plan['observation']['generation'], 'Original generation changed')
        else:
            policy.require(unit == state.get('stage') and pending == plan['unit_state'], 'Unknown unit in recovery')
            pid = self.service(); marker = self.file_state(self.marker, True)
            if pid:
                self.native(pid); value = self.snapshot()
                policy.require(value['pid'] == pid and value['owner_mode'] == 'Background', 'Unknown recovery owner')
                if not state.get('background'):
                    # A marker without the persisted verified-start receipt is
                    # unknown even if it is empty. Never claim it retroactively.
                    policy.require(marker is None, 'Unreceipted role marker; preserve for explicit review')
                else:
                    policy.require(pid == state['background']['pid'] and value['generation'] == state['background']['generation'], 'Another background generation owns service')
                self.ctl('stop'); policy.require(self.service() == 0, 'Background did not stop')
            owned = state.get('background', {}).get('marker')
            policy.rollback_preview(plan, expected_plan_sha256=state['plan_sha256'],
                original_unit=state['before_unit_utf8'].encode(), current_unit=self.unit_bytes(),
                current_profile_state=self.profile_state(), owned_unit=state['stage'], current_unit_state=unit,
                owned_marker=owned, current_marker=marker, background_stopped=True, attached_gui_pids=[],
                marker_absence_verified=marker is None)
            if marker:
                policy.require(self.file_state(self.marker, True) == owned, 'Role marker changed before removal')
                self.marker.unlink()
            policy.require(self.file_state(self.unit) == unit and self.file_state(self.pending) == pending, 'Unit changed before restore')
            self.exchange()
        subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True, timeout=20)
        if self.service() == 0: self.ctl('start')
        deadline = time.monotonic() + 20
        while True:
            try: self.verify_terminal(plan, dict(state, phase='rolled_back')); break
            except (OSError, ValueError, self.gate.IdentityRejected):
                policy.require(time.monotonic() < deadline, 'Restored owner not verified')
                time.sleep(.1)
        snapshot = self.snapshot(); state['restored'] = {'pid': snapshot['pid'], 'generation': snapshot['generation']}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--plan', type=Path, required=True); p.add_argument('--expected-plan-sha256', required=True)
    action = p.add_mutually_exclusive_group(required=True)
    action.add_argument('--apply', action='store_true'); action.add_argument('--recover', action='store_true')
    a = p.parse_args(); plan = json.loads(a.plan.read_text()); checked_plan(plan, a.expected_plan_sha256)
    with Native(plan) as backend: result = execute(backend, plan, a.expected_plan_sha256, a.recover)
    print(json.dumps({k: result.get(k) for k in ('phase', 'plan_sha256', 'native_gui_close_verified')}))


if __name__ == '__main__': main()
