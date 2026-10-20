"""Same-profile Linux update adapter for the original native installer.

The original installation journal and every completed update stay immutable when
starting a separately identified next update. Packaging uses the original release sealer; no compiler, new identity,
service configuration change, credential copy or public release is involved.
"""
from contextlib import contextmanager
import ctypes
import fcntl
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import time
import uuid

import build_identity_gate as gate
from install_native_first_identity import atomic_state, digest, inventory, real_path, require
from install_linux_desktop import exclusive_rename, sync


def state(path):
    real_path(path)
    m = path.lstat()
    require(m.st_uid == os.getuid() and not stat.S_IMODE(m.st_mode) & 0o7000,
            'Update path owner/mode differs')
    result = dict(device=m.st_dev, inode=m.st_ino, uid=m.st_uid, gid=m.st_gid,
                  mode=stat.S_IMODE(m.st_mode))
    if stat.S_ISREG(m.st_mode):
        require(m.st_nlink == 1, 'Hardlinked update file')
        result['sha256'] = gate.hash_file(path)
    else:
        require(stat.S_ISDIR(m.st_mode), 'Special update file')
    result['xattrs'] = {n: gate.digest(os.getxattr(path, n)) for n in sorted(os.listxattr(path))}
    return result


def tree(path):
    result = {'directory': state(path)}
    result['files'] = {n: state(path/n) for n in inventory(path, gate)}
    return result


def require_idle(document):
    sessions = document.get('sessions')
    require(document.get('ok') is True and document.get('renderer') == gate.RENDERER
            and isinstance(sessions, dict) and isinstance(sessions.get('sessions'), list)
            and isinstance(sessions.get('pending'), int)
            and not sessions['sessions'] and sessions['pending'] == 0
            and not document.get('lease'), 'Owner is unavailable or has active/pending sessions')
    require(isinstance(document.get('generation'), str) and document['generation'],
            'Owner generation unavailable')
    return document['generation']


def resource_drift(before, after, expected, path, raw):
    """Review one externally maintained CPU weight without changing any setting."""
    require(digest(after) == expected, 'Observed configuration digest differs')
    require(before['unit'] == after['unit'] and before['enabled'] == after['enabled']
            and set(before['dropins']) == set(after['dropins']), 'Service entry or drop-in set changed')
    changed = [key for key in before['dropins'] if before['dropins'][key] != after['dropins'][key]]
    require(changed == [str(path)], 'Only the exact observed runtime CPUWeight drop-in may differ')
    lines = [line.strip() for line in raw.splitlines()
             if line.strip() and not line.lstrip().startswith(('#', ';'))]
    require(len(lines) == 2 and lines[0] == '[Service]' and lines[1].startswith('CPUWeight='),
            'Resource drop-in contains another policy')
    weight = lines[1].split('=', 1)[1]
    require(weight == 'idle' or (weight.isascii() and weight.isdecimal() and 1 <= int(weight) <= 10000),
            'Unknown CPU weight')
    require(gate.digest(raw.encode()) == after['dropins'][str(path)], 'Resource file changed while reviewing')
    return {'path': str(path), 'before_sha256': before['dropins'][str(path)],
            'after_sha256': after['dropins'][str(path)], 'cpu_weight': weight,
            'resource_setting_modified_by_installer': False}


class LinuxUpdate:
    def __init__(self, args):
        require(sys.platform.startswith('linux') and os.getuid() != 0, 'Native ordinary-user Linux only')
        require(all(hasattr(os, name) for name in ('listxattr', 'getxattr')),
                'Native security metadata APIs required')
        self.args = args
        self.repo = Path(__file__).resolve().parents[1]
        self.home = Path.home().resolve(strict=True)
        self.live = self.home/'.local/opt/weavelo'
        self.exe = self.live/'weavelo'
        self.profile = self.home/'.config/weavelo/mesh'
        self.unit = self.home/'.config/systemd/user/weavelo.service'
        self.work = self.home/'.local/state/weavelo-identity-migration'
        self.update_id = getattr(args, 'update_id', None)
        if self.update_id is not None:
            require(str(uuid.UUID(self.update_id)) == self.update_id,
                    'Update ID must be a canonical UUID')
        self.original_journal = self.work/'journal.json'
        original = gate.read_json(self.original_journal)
        require(original.get('phase') == 'installed_and_running_verified'
                and original.get('plan_sha256') == digest(original.get('plan')),
                'Original completed installation journal required')
        self.transaction = self.work/'updates'/self.update_id if self.update_id else self.work
        self.journal = self.transaction/'journal.json'
        self.payload = self.transaction/'update-payload'
        self.stage = self.payload/'Weavelo'
        self.previous = self.transaction/'previous-app'
        self.manifest = self.payload/gate.MANIFEST
        for directory in (self.work, self.profile):
            require(state(directory)['mode'] == 0o700, 'Existing private installation directory required')
        require(state(self.original_journal)['mode'] == 0o600, 'Existing private installation journal required')
        if self.update_id and not self.journal.exists():
            require(not self.transaction.exists() and not self.transaction.is_symlink(),
                    'Unrecorded transaction directory preserved; inspect its original operation')
            self.root = {'schema': 1, 'kind': 'weavelo-linux-update-transaction',
                         'update_id': self.update_id}
            self.journal_state = None
        else:
            require(state(self.journal)['mode'] == 0o600, 'Private transaction journal required')
            self.root = gate.read_json(self.journal)
            if self.update_id:
                require(self.root.get('schema') == 1
                        and self.root.get('kind') == 'weavelo-linux-update-transaction'
                        and self.root.get('update_id') == self.update_id,
                        'Transaction identity differs')
            self.journal_state = state(self.journal)

    def journals(self):
        """Bounded metadata inventory under the one original update authority."""
        yield self.original_journal, gate.read_json(self.original_journal), self.work
        directory = self.work/'updates'
        if not directory.exists() and not directory.is_symlink():
            return
        require(state(directory)['mode'] == 0o700, 'Private update namespace required')
        entries = list(directory.iterdir())
        require(len(entries) <= 256, 'Update journal review limit reached; preserve history')
        for entry in sorted(entries):
            require(str(uuid.UUID(entry.name)) == entry.name and state(entry)['mode'] == 0o700,
                    'Unknown update directory preserved')
            path = entry/'journal.json'
            require(state(path)['mode'] == 0o600, 'Incomplete transaction; inspect original operation')
            document = gate.read_json(path)
            require(document.get('schema') == 1
                    and document.get('kind') == 'weavelo-linux-update-transaction'
                    and document.get('update_id') == entry.name, 'Update journal identity differs')
            yield path, document, entry

    def guard_other_transactions(self):
        for path, document, _ in self.journals():
            if path == self.journal:
                continue
            previous = document.get('linux_update')
            if previous is None:
                require(path == self.original_journal, 'Incomplete transaction; inspect original operation')
                continue
            require(previous.get('phase') in ('committed', 'rolled_back')
                    and previous.get('plan_sha256') == digest(previous.get('plan')),
                    'Another update is pending or uncertain; observe/recover its original transaction')

    def old_release(self):
        """Resolve the actual installed inode to exactly one verified predecessor."""
        if not self.update_id:
            old = self.root['plan']
            return (self.work/'installed-manifest.json', self.root['manifest_sha256'],
                    old['source_commit'], old['build_identity_sha256'], old['new_binary_sha256'])
        live = tree(self.live)
        matches = []
        for path, document, folder in self.journals():
            if path == self.journal:
                continue
            previous = document.get('linux_update')
            if previous and previous['phase'] == 'committed':
                plan = previous['plan']
                if plan['binary_sha256'] == self.args.old_binary_sha256 and plan['candidate_app'] == live:
                    matches.append((path, previous['plan_sha256'], folder/'update-payload'/gate.MANIFEST,
                                    plan['manifest_sha256'], plan['source_commit'],
                                    plan['identity_sha256'], plan['binary_sha256']))
            if path == self.original_journal:
                plan = document['plan']
                if plan['new_binary_sha256'] == self.args.old_binary_sha256:
                    matches.append((path, document['plan_sha256'], folder/'installed-manifest.json',
                                    document['manifest_sha256'], plan['source_commit'],
                                    plan['build_identity_sha256'], plan['new_binary_sha256']))
        require(len(matches) == 1, 'Installed image has no unique committed predecessor; preserve state')
        path, plan_sha, manifest, manifest_sha, commit, identity, binary = matches[0]
        self.root['predecessor'] = {'journal': str(path), 'state': state(path), 'plan_sha256': plan_sha}
        return manifest, manifest_sha, commit, identity, binary

    @contextmanager
    def lock(self):
        path = self.work/'update.lock'
        real_path(path)
        fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        try:
            m = os.fstat(fd)
            require(m.st_uid == os.getuid() and m.st_nlink == 1
                    and stat.S_ISREG(m.st_mode) and stat.S_IMODE(m.st_mode) == 0o600,
                    'Unknown update lock')
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            observed = state(self.journal) if self.journal.exists() or self.journal.is_symlink() else None
            require(observed == self.journal_state, 'Installation journal changed')
            self.guard_other_transactions()
            yield
        finally:
            os.close(fd)

    def save(self, update):
        observed = state(self.journal) if self.journal.exists() or self.journal.is_symlink() else None
        require(observed == self.journal_state, 'Installation journal changed externally')
        self.root['linux_update'] = update
        atomic_state(self.journal, self.root, initial=self.journal_state is None)
        self.journal_state = state(self.journal)

    def service(self):
        keys = ['MainPID', 'ActiveState', 'SubState', 'UnitFileState', 'FragmentPath',
                'DropInPaths', 'Job', 'TriggeredBy', 'Restart']
        command = ['systemctl', '--user', 'show', '--all', 'weavelo.service']
        for key in keys:
            command += ['-p', key]
        raw = subprocess.check_output(command, text=True, timeout=15)
        result = dict(line.split('=', 1) for line in raw.splitlines() if '=' in line)
        require(set(keys) <= result.keys(), 'Incomplete service policy observation')
        # systemctl omits empty Exec* arrays even with --all on the native host.
        # Query their real D-Bus values; omission is never treated as absence.
        for key in ('ExecStop', 'ExecStopPost'):
            value = subprocess.check_output([
                'busctl', '--user', 'get-property', 'org.freedesktop.systemd1',
                '/org/freedesktop/systemd1/unit/weavelo_2eservice',
                'org.freedesktop.systemd1.Service', key], text=True, timeout=15)
            require(value.strip() == 'a(sasbttttuii) 0', 'Service stop hook is present or unknown')
            result[key] = ''
        return result

    def configuration(self):
        s = self.service()
        require(s['UnitFileState'] in ('enabled', 'disabled') and not s['Job']
                and not s['TriggeredBy'] and not s['ExecStop'] and not s['ExecStopPost']
                and s['Restart'] in ('no', 'on-failure'), 'Unsupported service lifecycle policy')
        require(Path(s['FragmentPath']).resolve(strict=True) == self.unit, 'Service fragment changed')
        text = self.unit.read_text()
        require(text.count('ExecStart=') == 1 and f'ExecStart={self.exe}\n' in text,
                'Only the existing standalone canonical entry can be updated')
        require(not (self.profile/'background-service.enabled').exists(),
                'Existing separated lifecycle requires its own update plan')
        dropins = {}
        for name in s['DropInPaths'].split():
            path = Path(name).resolve(strict=True)
            # These existing drop-ins only control resource/stop policy.
            lines = [line.strip() for line in path.read_text().splitlines()
                     if line.strip() and not line.lstrip().startswith(('#', ';'))]
            require(all(line == '[Service]' or line.split('=', 1)[0]
                        in ('CPUWeight', 'TimeoutStopFailureMode') for line in lines),
                    'Unreviewed service drop-in')
            dropins[str(path)] = gate.hash_file(path)
        return {'unit': state(self.unit), 'dropins': dropins, 'enabled': s['UnitFileState']}

    def profile_state(self):
        require(state(self.profile)['mode'] == 0o700, 'Private profile mode changed')
        files = {name: state(self.profile/name) for name in ('mesh.conf', 'mesh.secret')}
        require(all(value['mode'] == 0o600 for value in files.values()), 'Private profile file mode changed')
        return {'directory': state(self.profile), 'files': files}

    def status(self):
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(('REMOTE_PLAY_', 'WEAVELO_', 'RP_MEDIA_ACCEPTANCE'))}
        env['WEAVELO_DEVICE_GROUP_DIR'] = str(self.profile)
        result = subprocess.run([str(self.exe), '--gui-status-json'], env=env,
                                capture_output=True, text=True, timeout=15)
        require(result.returncode == 0, 'Existing owner status unavailable')
        return json.loads(result.stdout)

    def owner(self, expected_hash):
        s = self.service()
        require(s['ActiveState'] == 'active' and s['SubState'] == 'running', 'Service not running')
        pid = int(s['MainPID'])
        proof = gate.verify_running_linux(pid, self.exe, expected_hash)
        return {'pid': pid, 'generation': require_idle(self.status()), 'native': proof}

    def pins(self):
        a = self.args
        require(all((a.build_receipt, a.build_receipt_sha256, a.expected_commit,
                     a.identity_sha256, a.compiler_binary, a.old_binary_sha256)),
                'Independent compiler receipt and old/new identity pins required')
        receipt = gate.read_json(a.build_receipt, a.build_receipt_sha256)
        require(receipt.get('kind') == 'observed-build', 'Native observed compiler receipt required')
        expected = gate.validate_identity(receipt['build_identity'], a.expected_commit, a.identity_sha256)
        require(expected['configuration']['platform'] == 'linux', 'Linux compiler output required')
        gate.linux_elf_identity(a.compiler_binary, expected, receipt['compiler_binary_sha256'])
        gate.product_info(a.compiler_binary, expected, receipt['compiler_binary_sha256'])
        require(receipt['compiler_binary_sha256'] != a.old_binary_sha256, 'Candidate is already installed')
        return receipt

    def prepare(self):
        self.guard_other_transactions()
        require('linux_update' not in self.root, 'Observe the original update; never replace its journal')
        require(not any(p.exists() or p.is_symlink() for p in (self.payload, self.previous)),
                'Existing update payload/rollback preserved')
        r = self.pins()
        old_manifest, manifest_sha, old_commit, old_identity, old_binary = self.old_release()
        require(old_binary == self.args.old_binary_sha256,
                'Installed journal differs from independently reviewed old image')
        gate.verify_install(self.live, old_manifest, manifest_sha,
                            old_commit, old_identity, self.exe, self.exe)
        plan = {'schema': 1, 'kind': 'weavelo-linux-same-profile-update',
                'old_app': tree(self.live), 'old_manifest': state(old_manifest),
                'old_manifest_path': str(old_manifest),
                **({'update_id': self.update_id, 'predecessor': self.root['predecessor']}
                   if self.update_id else {}),
                'profile': self.profile_state(), 'configuration': self.configuration(),
                'old_owner': self.owner(self.args.old_binary_sha256),
                'old_binary_sha256': self.args.old_binary_sha256,
                'source_commit': self.args.expected_commit,
                'identity_sha256': self.args.identity_sha256,
                'binary_sha256': r['compiler_binary_sha256'],
                'receipt': str(self.args.build_receipt),
                'receipt_sha256': self.args.build_receipt_sha256,
                'installer_sha256': gate.hash_file(Path(__file__)),
                'entry_sha256': gate.hash_file(self.repo/'scripts/install_native_first_identity.py')}
        update = {'phase': 'preparing', 'plan': plan}
        if self.update_id:
            namespace = self.transaction.parent
            if not namespace.exists():
                namespace.mkdir(mode=0o700)
                sync(namespace.parent)
            require(state(namespace)['mode'] == 0o700, 'Private update namespace required')
            self.transaction.mkdir(mode=0o700)
            sync(namespace)
        self.save(update)
        self.payload.mkdir(mode=0o700)
        self.stage.mkdir(mode=0o700)
        shutil.copy2(self.args.compiler_binary, self.stage/'weavelo')
        gate.write_new(self.stage/'build-info.json', {
            'source_commit': plan['source_commit'], 'build_identity_sha256': plan['identity_sha256'],
            'binary_sha256': plan['binary_sha256']})
        sealed = gate.seal_tree(self.payload, r, plan['source_commit'], plan['identity_sha256'],
                                compiler_binary=self.args.compiler_binary)
        plan['manifest_sha256'] = sealed['manifest_sha256']
        plan['candidate_app'] = tree(self.stage)
        update.update(phase='prepared', plan_sha256=digest(plan))
        self.save(update)
        return self.report(update)

    def check(self, update, expected):
        plan = update['plan']
        require(expected == update.get('plan_sha256') == digest(plan), 'Reviewed update plan differs')
        implementation = update.get('reconciliation', plan)
        require(implementation['installer_sha256'] == gate.hash_file(Path(__file__))
                and implementation['entry_sha256'] == gate.hash_file(self.repo/'scripts/install_native_first_identity.py'),
                'Installer changed since plan review')
        configuration = update.get('reconciliation', {}).get('configuration', plan['configuration'])
        require(self.configuration() == configuration, 'Service configuration changed')
        require(self.profile_state() == plan['profile'], 'Profile changed; preserve credentials')
        require(state(Path(plan.get('old_manifest_path', self.work/'installed-manifest.json'))) == plan['old_manifest'],
                'Original release evidence changed')
        predecessor = plan.get('predecessor')
        if predecessor:
            require(state(Path(predecessor['journal'])) == predecessor['state'], 'Predecessor journal changed')
        return plan

    def ctl(self, action):
        subprocess.run(['systemctl', '--user', action, 'weavelo.service'],
                       check=True, capture_output=True, timeout=35)

    def exchange(self, first, second):
        require(first.parent.stat().st_dev == second.parent.stat().st_dev, 'Same filesystem required')
        libc = ctypes.CDLL(None, use_errno=True)
        require(hasattr(libc, 'renameat2'), 'Atomic directory exchange unavailable')
        fn = libc.renameat2
        fn.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        if fn(-100, os.fsencode(first), -100, os.fsencode(second), 2):
            raise OSError(ctypes.get_errno(), 'Atomic application exchange failed')
        sync(first.parent)
        sync(second.parent)

    def wait_owner(self, expected):
        deadline = time.monotonic() + 30
        previous = None
        samples = 0
        while True:
            try:
                observation = self.owner(expected)
                samples = samples + 1 if previous == observation else 1
                previous = observation
                if samples >= 3:
                    return observation
            except (ValueError, RuntimeError, gate.IdentityRejected, subprocess.SubprocessError):
                samples = 0
            require(time.monotonic() < deadline, 'Owner/GUI startup did not stabilize')
            time.sleep(1)

    def apply(self, expected):
        update = self.root.get('linux_update', {})
        plan = self.check(update, expected)
        require(update['phase'] == 'prepared', 'Observe/recover original update; do not replay')
        require(tree(self.live) == plan['old_app'] and tree(self.stage) == plan['candidate_app'],
                'Application changed since plan review')
        require(self.owner(plan['old_binary_sha256']) == plan['old_owner'], 'Old owner changed since review')
        gate.read_json(Path(plan['receipt']), plan['receipt_sha256'])
        gate.verify_install(self.stage, self.manifest, plan['manifest_sha256'], plan['source_commit'],
                            plan['identity_sha256'], self.stage/'weavelo', self.stage/'weavelo')
        try:
            update['phase'] = 'stop_requested'; self.save(update)
            self.ctl('stop')
            require(int(self.service()['MainPID']) == 0
                    and not Path('/proc', str(plan['old_owner']['pid'])).exists(), 'Old owner still present')
            self.check(update, expected)
            require(tree(self.live) == plan['old_app'] and tree(self.stage) == plan['candidate_app'],
                    'Application drift before exchange')
            update['phase'] = 'exchange_requested'; self.save(update)
            self.exchange(self.live, self.stage)
            require(tree(self.live) == plan['candidate_app'] and tree(self.stage) == plan['old_app'],
                    'Exchanged inodes differ')
            exclusive_rename(self.stage, self.previous)
            update['phase'] = 'start_requested'; self.save(update)
            self.ctl('start')
            update['owner'] = self.wait_owner(plan['binary_sha256'])
            self.check(update, expected)
            gate.verify_install(self.live, self.manifest, plan['manifest_sha256'], plan['source_commit'],
                                plan['identity_sha256'], self.exe, self.exe)
            require(tree(self.live) == plan['candidate_app'] and tree(self.previous) == plan['old_app'],
                    'Installed/rollback bytes changed')
            update['phase'] = 'committed'; self.save(update)
        except Exception:
            update['failure'] = 'Installation verification failed; product output withheld'
            self.save(update)
            try:
                self.rollback(update, expected)
            except Exception:
                update['phase'] = 'recovery_required'; self.save(update)
                raise RuntimeError('Recovery requires original journal inspection; do not replay') from None
            raise RuntimeError('Update failed; original image restored and verified') from None
        return self.report(update)

    def rollback(self, update, expected):
        plan = self.check(update, expected)
        live = tree(self.live)
        require(live in (plan['old_app'], plan['candidate_app']), 'Unknown installed inode; preserve it')
        s = self.service()
        if int(s['MainPID']):
            expected_hash = plan['binary_sha256'] if live == plan['candidate_app'] else plan['old_binary_sha256']
            gate.verify_running_linux(int(s['MainPID']), self.exe, expected_hash)
            if live == plan['candidate_app']:
                update['phase'] = 'rollback_stop_requested'; self.save(update)
                self.ctl('stop')
                require(int(self.service()['MainPID']) == 0, 'Candidate still active')
        if live == plan['candidate_app']:
            locations = [p for p in (self.previous, self.stage) if p.exists()]
            require(len(locations) == 1 and tree(locations[0]) == plan['old_app'],
                    'Original rollback inode unavailable')
            update['phase'] = 'rollback_exchange_requested'; self.save(update)
            self.exchange(self.live, locations[0])
        require(tree(self.live) == plan['old_app'], 'Old application was not restored')
        if int(self.service()['MainPID']) == 0:
            update['phase'] = 'rollback_start_requested'; self.save(update)
            self.ctl('start')
        update['rollback_owner'] = self.wait_owner(plan['old_binary_sha256'])
        self.check(update, expected)
        update['phase'] = 'rolled_back'; self.save(update)
        return self.report(update)

    def reconcile(self, expected, configuration_sha256):
        """Finish an already verified start after a specifically reviewed resource change.

        This never starts/stops/exchanges an app or rewrites unit/profile files.
        The original failed intent and immutable plan remain in the same journal.
        """
        update = self.root.get('linux_update', {})
        plan = update.get('plan', {})
        require(expected == update.get('plan_sha256') == digest(plan), 'Original update plan differs')
        require(update.get('phase') == 'recovery_required' and update.get('owner'),
                'Only an originally verified new owner can be reconciled')
        require(self.profile_state() == plan['profile'], 'Profile changed; preserve credentials')
        require(state(Path(plan.get('old_manifest_path', self.work/'installed-manifest.json'))) == plan['old_manifest'],
                'Original release evidence changed')
        require(tree(self.live) == plan['candidate_app'] and tree(self.previous) == plan['old_app'],
                'Installed/rollback inode or bytes differ')
        current = self.configuration()
        path = Path(f'/run/user/{os.getuid()}/systemd/user.control/weavelo.service.d/50-CPUWeight.conf')
        review = resource_drift(plan['configuration'], current, configuration_sha256, path, path.read_text())
        owner = self.wait_owner(plan['binary_sha256'])
        require(owner == update['owner'], 'Originally verified new owner changed')
        gate.read_json(Path(plan['receipt']), plan['receipt_sha256'])
        gate.verify_install(self.live, self.manifest, plan['manifest_sha256'], plan['source_commit'],
                            plan['identity_sha256'], self.exe, self.exe)
        require(self.configuration() == current and self.profile_state() == plan['profile'],
                'Configuration/profile changed during reconciliation')
        update['reconciliation'] = {
            'previous_phase': update['phase'], 'configuration': current,
            'configuration_sha256': configuration_sha256, 'resource_review': review,
            'installer_sha256': gate.hash_file(Path(__file__)),
            'entry_sha256': gate.hash_file(self.repo/'scripts/install_native_first_identity.py'),
            'service_actions_replayed': False}
        update['phase'] = 'committed'
        self.save(update)
        return self.report(update)

    def report(self, update):
        plan = update['plan']
        return {'phase': update['phase'], 'plan_sha256': update.get('plan_sha256'),
                'update_id': getattr(self, 'update_id', None),
                'source_commit': plan['source_commit'], 'binary_sha256': plan['binary_sha256'],
                'manifest_sha256': plan.get('manifest_sha256'), 'manifest': str(self.manifest),
                'journal': str(self.journal), 'previous_app': str(self.previous),
                'owner': update.get('owner'), 'rollback_owner': update.get('rollback_owner'),
                'credentials_copied': False, 'service_configuration_changed': False,
                'external_resource_change_reviewed': 'reconciliation' in update,
                'physical_acceptance': 'not_evaluated'}


def run(args):
    require(sum((args.prepare_update, args.apply, args.recover_update, args.reconcile_update)) == 1,
            'Choose prepare, apply, recover, or reconcile for the original Linux update')
    update = LinuxUpdate(args)
    with update.lock():
        if args.prepare_update:
            result = update.prepare()
        elif args.recover_update:
            result = update.rollback(update.root['linux_update'], args.plan_sha256)
        elif args.reconcile_update:
            result = update.reconcile(args.plan_sha256, args.configuration_sha256)
        else:
            result = update.apply(args.plan_sha256)
    print(json.dumps(result, indent=2))
