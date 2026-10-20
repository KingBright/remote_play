#!/usr/bin/env python3
"""Narrow native Windows/Linux first install, consuming original verified outputs.

No build, key import, secret export, OS permissions repair or package rewriting.
Both platforms require explicit reviewed original producer inputs; there is no
historical artifact fallback. Installer source and runtime source are distinct:
changing this installer does not rebuild or relabel a reviewed runtime. Run from
this primary checkout only. Existing/partially migrated installations are refused.
Old app and credentials stay in their original locations as rollback. Unknown
journals are never replayed. Linux uses the project's original sealer/desktop tool.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import time
import zipfile



def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def real_path(path):
    require(path.is_absolute() and '..' not in path.parts, 'Noncanonical path')
    for part in (path, *path.parents):
        require(not part.is_symlink(), 'Linked transaction path')
    return path


def atomic_state(path, document, initial=False):
    real_path(path)
    raw = json.dumps(document, indent=2).encode() + b'\n'
    if initial:
        with path.open('xb') as output:
            output.write(raw)
            output.flush()
            os.fsync(output.fileno())
        if sys.platform != 'win32':
            os.chmod(path, 0o600)
        return
    pending = path.with_suffix('.pending')
    with pending.open('xb') as output:
        output.write(raw)
        output.flush()
        os.fsync(output.fileno())
    if sys.platform != 'win32':
        os.chmod(pending, 0o600)
    os.replace(pending, path)


def inventory(root, gate):
    real_path(root)
    require(root.is_dir(), 'Missing payload')
    result = {}
    for path in sorted(root.rglob('*')):
        real_path(path)
        if path.is_dir():
            continue
        require(path.is_file() and path.stat().st_nlink == 1, 'Unsafe payload member')
        result[path.relative_to(root).as_posix()] = gate.hash_file(path)
    require(0 < len(result) <= 32, 'Unexpected payload size')
    return result


def reviewed_linux_binding(gate, args):
    """Consume independent receipt/manifest pins; pending/static evidence cannot install."""
    names = ('build_receipt','build_receipt_sha256','release_manifest','manifest_sha256',
             'expected_commit','identity_sha256','compiler_binary','old_binary_sha256')
    require(args is not None and all(getattr(args,name,None) for name in names),
            'Linux installation requires reviewed receipt, manifest, source, identity and old/new binary pins')
    receipt = gate.read_json(args.build_receipt,args.build_receipt_sha256)
    require(type(receipt.get('schema')) is int and receipt['schema'] in (1,2) and
            receipt.get('kind') == 'observed-build' and receipt.get('native_observation') in (None,'verified'),
            'Real observed producer receipt required; pending compiler output cannot install')
    expected = gate.validate_identity(receipt.get('build_identity'),args.expected_commit,args.identity_sha256)
    require(expected['configuration']['platform'] == 'linux', 'Linux producer identity required')
    gate.validate_product_info(receipt['product_info'],expected)
    gate.regular(args.release_manifest)
    manifest = gate.manifest_document(args.release_manifest.read_bytes(),args.manifest_sha256,
                                      args.expected_commit,args.identity_sha256)
    require(manifest.get('build_receipt_canonical_sha256') == gate.digest(gate.canonical(receipt)),
            'Release manifest is not bound to the reviewed observed receipt')
    require(manifest['build_identity'] == expected and
            manifest['product_info'] == receipt['product_info'] and
            manifest['binary_sha256'] == receipt['compiler_binary_sha256'], 'Producer/manifest provenance differs')
    names = {entry['path'] for entry in manifest['files']}
    require(names in ({'weavelo'},{'weavelo','build-info.json'}), 'Unsupported Linux installer payload members')
    if 'build-info.json' in names:
        metadata = {'source_commit':args.expected_commit,'build_identity_sha256':args.identity_sha256,
                    'binary_sha256':receipt['compiler_binary_sha256']}
        raw = json.dumps(metadata,sort_keys=True,indent=2,allow_nan=False).encode()+b'\n'
        entry = next(item for item in manifest['files'] if item['path'] == 'build-info.json')
        require(entry['sha256'] == gate.digest(raw) and entry['bytes'] == len(raw), 'Reviewed build-info differs')
    gate.linux_elf_identity(args.compiler_binary,expected,receipt['compiler_binary_sha256'])
    return {'commit':args.expected_commit,
            'pin':{'binary':receipt['compiler_binary_sha256'],'old':gate.sha(args.old_binary_sha256),
                   'identity':args.identity_sha256,'receipt':args.build_receipt_sha256,'manifest':args.manifest_sha256},
            'receipt':args.build_receipt,'compiler':args.compiler_binary,'manifest':args.release_manifest,
            'manifest_document':manifest}


def reviewed_windows_binding(gate, args, repo):
    """Bind the original six-member Windows package, receipt and rollback image."""
    names = ('build_receipt', 'build_receipt_sha256', 'release_archive', 'archive_sha256',
             'expected_commit', 'identity_sha256', 'compiler_binary', 'old_binary_sha256')
    require(args is not None and all(getattr(args, name, None) for name in names),
            'Windows installation requires reviewed receipt, archive, source, identity and old/new binary pins')
    paths = {'build_receipt': repo/'target/package/windows/windows-current-build-receipt.json',
             'release_archive': repo/'target/package/windows/Weavelo-Windows-x64.zip',
             'compiler_binary': repo/'target/release/weavelo.exe'}
    for name, path in paths.items():
        require(real_path(getattr(args, name)) == path, 'Use original Windows producer output paths')
    receipt = gate.read_json(args.build_receipt, args.build_receipt_sha256)
    require(type(receipt.get('schema')) is int and receipt['schema'] == 2 and
            receipt.get('kind') == 'observed-build', 'Native Windows observed producer receipt required')
    expected = gate.validate_identity(receipt.get('build_identity'), args.expected_commit, args.identity_sha256)
    config = expected['configuration']
    require(config['platform'] == 'windows' and config['architecture'] == 'x86_64' and
            config['target'] == 'x86_64-pc-windows-msvc' and config['profile'] == 'release',
            'Original native Windows release identity required')
    binary = gate.sha(receipt['compiler_binary_sha256'])
    require(type(receipt.get('compiler_binary_bytes')) is int and
            args.compiler_binary.stat().st_size == receipt['compiler_binary_bytes'], 'Compiler size differs')
    require(gate.product_info(args.compiler_binary, expected, binary) == receipt['product_info'],
            'Actual compiler ProductInfo differs from observed receipt')
    pin = {'binary': binary, 'old': gate.sha(args.old_binary_sha256),
           'identity': gate.sha(args.identity_sha256), 'receipt': gate.sha(args.build_receipt_sha256),
           'archive': gate.sha(args.archive_sha256)}
    candidate = args.release_archive.parent/'Weavelo'
    files = reviewed_windows_payload(gate, candidate, args.release_archive, receipt, pin)
    return {'commit': args.expected_commit, 'pin': pin, 'receipt': args.build_receipt,
            'compiler': args.compiler_binary, 'archive': args.release_archive,
            'candidate': candidate, 'payload': files}


def reviewed_windows_payload(gate, candidate, archive, receipt, pin):
    require(gate.hash_file(archive) == pin['archive'], 'Formal archive drift')
    files = inventory(candidate, gate)
    expected = {'application-identity.json', 'build-info.json', 'ffmpeg-license.txt',
                'weavelo.exe', 'Weavelo.ico', 'bin/ffmpeg.exe'}
    require(set(files) == expected and files['weavelo.exe'] == pin['binary'],
            'Original Windows payload differs')
    with zipfile.ZipFile(archive) as z:
        entries = [item for item in z.infolist() if not item.is_dir()]
        names = [item.filename.replace('\\', '/') for item in entries]
        require(len(names) == len(set(name.lower() for name in names)) and
                set(names) == {'Weavelo/'+name for name in expected}, 'Unexpected or duplicate archive member')
        require(sum(item.file_size for item in entries) <= 512*1024**2, 'Windows archive too large')
        for item, name in zip(entries, names):
            with z.open(item) as incoming:
                value = hashlib.file_digest(incoming, 'sha256').hexdigest()
            require(value == files[name.removeprefix('Weavelo/')], 'Archive/payload mismatch')
    info = gate.read_json(candidate/'build-info.json')
    require(info.get('source_commit') == receipt['build_identity']['source']['commit'] and
            info.get('build_identity') == receipt['build_identity'] and
            info.get('build_receipt_sha256') == pin['receipt'] and
            info.get('weavelo_sha256') == pin['binary'] and
            info.get('package_version') == receipt['build_identity']['configuration']['version'] and
            info.get('ffmpeg_sha256') == files['bin/ffmpeg.exe'] and
            info.get('system_ffmpeg_fallback') is False, 'Windows package/producer provenance differs')
    require(gate.hash_file(archive) == pin['archive'] and inventory(candidate, gate) == files,
            'Windows package changed during verification')
    return files


class NativeInstall:
    def __init__(self, reviewed=None):
        self.windows = sys.platform == 'win32'
        require(self.windows or sys.platform.startswith('linux'), 'Native Windows/Linux required')
        self.platform = 'windows' if self.windows else 'linux'
        self.home = Path.home().resolve(strict=True)
        self.repo = Path(__file__).resolve().parents[1]
        sys.path.insert(0, str(self.repo/'scripts'))
        import build_identity_gate
        self.gate = build_identity_gate
        if self.windows:
            binding = reviewed_windows_binding(self.gate, reviewed, self.repo)
            self.pin = binding['pin']; self.commit = binding['commit']
            self.old_app = self.home/'AppData/Local/RemotePlay'
            self.live = self.home/'AppData/Local/Weavelo'
            self.old_exe = self.old_app/'remote_play.exe'
            self.profile = self.home/'AppData/Roaming/Weavelo/Mesh'
            self.old_profiles = [self.home/'AppData/Roaming/RemotePlay/Mesh']
            self.receipt = binding['receipt']
            self.candidate = binding['candidate']
            self.compiler = binding['compiler']
            self.archive = binding['archive']
            self.work = self.home/'AppData/Local/.weavelo-identity-migration'
            self.shortcut = self.home/'AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Weavelo.lnk'
        else:
            binding = reviewed_linux_binding(self.gate, reviewed)
            self.pin = binding['pin']; self.commit = binding['commit']
            self.old_app = (self.home/'.local/opt/remoteplay/2.0.0-alpha.8').resolve(strict=True)
            self.old_exe = self.old_app/'remote_play'
            self.live = self.home/'.local/opt/weavelo'
            self.profile = self.home/'.config/weavelo/mesh'
            self.old_profiles = [self.home/'.config/remote-play/mesh', self.home/'.config/remote-play-current/mesh']
            self.receipt = binding['receipt']
            self.compiler = binding['compiler']
            self.input_manifest = binding['manifest']
            self.input_manifest_document = binding['manifest_document']
            self.work = self.home/'.local/state/weavelo-identity-migration'
            self.old_unit = self.home/'.config/systemd/user/remote-play-current.service'
            self.new_unit = self.home/'.config/systemd/user/weavelo.service'
        self.exe = self.live/('weavelo.exe' if self.windows else 'weavelo')
        self.stage = self.live.parent/'.weavelo-install.pending'
        self.journal = self.work/'journal.json'
        for p in [self.live, self.work, self.profile, self.stage, self.old_exe, self.receipt]:
            real_path(p)

    def ctl(self, action, unit):
        subprocess.run(['systemctl', '--user', action, unit], check=True, timeout=30,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

    def old_pid(self):
        if not self.windows:
            return int(subprocess.check_output(['systemctl', '--user', 'show', 'remote-play-current.service', '-p', 'MainPID', '--value'], text=True).strip())
        raw = subprocess.check_output(['powershell.exe', '-NoProfile', '-Command',
            "Get-CimInstance Win32_Process | Where-Object {$_.Name -eq 'remote_play.exe'} | Select-Object ProcessId,ExecutablePath | ConvertTo-Json -Compress"], text=True)
        entries = json.loads(raw or '[]')
        if isinstance(entries, dict):
            entries = [entries]
        own = [x for x in entries if os.path.normcase(x['ExecutablePath']) == os.path.normcase(str(self.old_exe))]
        require(len(own) == 1, 'Unexpected old owners')
        return int(own[0]['ProcessId'])

    def native(self, pid, exe, binary, expected=None):
        if self.windows:
            if expected is None:
                info = json.loads(subprocess.check_output([str(exe), '--product-info-json'], text=True, timeout=15))
                expected = info['build_identity']
            return self.gate.verify_running_windows(pid, exe, binary, expected)
        return self.gate.verify_running_linux(pid, exe, binary)

    def protected(self):
        return {str(p/name): self.gate.hash_file(p/name) for p in self.old_profiles for name in ['mesh.conf', 'mesh.secret']}

    def runtime_env(self, old=False):
        env = os.environ.copy()
        for k in list(env):
            if k.startswith(('REMOTE_PLAY_', 'WEAVELO_', 'RP_MEDIA_ACCEPTANCE')):
                env.pop(k)
        prefix = 'REMOTE_PLAY_' if old else 'WEAVELO_'
        # Windows production and rollback are duplex roles. A receive-only test
        # must never leave either normal owner without its passive listener.
        env.update({prefix+'HEADLESS': '0', prefix+'PASSIVE_HOST': '1' if self.windows else '0',
                    prefix+'DEVICE_GROUP_DIR': str(self.old_profiles[-1] if old else self.profile)})
        return env

    def require_idle(self):
        result = subprocess.run([str(self.old_exe), '--gui-status-json'], env=self.runtime_env(old=True),
                                text=True, capture_output=True, timeout=15)
        if self.windows and result.returncode != 0:
            # This old owner's private IPC reports UnexpectedEof. Its exact
            # compiled image is verified independently before the explicitly
            # authorized stop. Do not misrepresent IPC failure as an idle proof.
            return {'available': False, 'idle_verified': False,
                    'cutover_basis': 'explicit three-device stop-and-new-identity approval'}
        require(result.returncode == 0, 'Old GUI status unavailable; preserve owner')
        d = json.loads(result.stdout)
        require(d.get('ok') is True and not d.get('lease'), 'Old runtime has active ownership/lease')
        sessions = d.get('sessions') or {}
        require(not sessions.get('sessions') and not sessions.get('pending'), 'Old runtime has active sessions')
        return {'available': True, 'idle_verified': True}

    def plan(self):
        g = self.gate
        require(not any(p.exists() for p in [self.live, self.stage, self.work, self.profile]), 'Existing new installation/transaction requires observation')
        if self.windows:
            real_path(self.shortcut)
            require(not self.shortcut.exists() and self.shortcut.parent.is_dir(), 'Existing/missing shortcut destination requires observation')
        if not self.windows:
            require(not self.new_unit.exists(), 'Existing new unit preserved')
        require(g.hash_file(self.receipt) == self.pin['receipt'], 'Observed receipt drift')
        if not self.windows:
            require(g.hash_file(self.input_manifest) == self.pin['manifest'], 'Reviewed Linux manifest drift')
        r = g.read_json(self.receipt)
        expected = r['build_identity']
        g.validate_identity(expected, self.commit, self.pin['identity'])
        require(r['kind'] == 'observed-build' and r['compiler_binary_sha256'] == self.pin['binary'], 'Wrong observed producer output')
        g.product_info(self.compiler, expected, self.pin['binary'])
        pid = self.old_pid()
        self.native(pid, self.old_exe, self.pin['old'])
        session_observation = self.require_idle()
        result = dict(schema=1, platform=self.platform, kind='weavelo-first-identity-install',
            source_commit=self.commit, build_identity_sha256=self.pin['identity'], receipt_sha256=self.pin['receipt'],
            observed_build_receipt_path=str(self.receipt),
            old_pid=pid, old_executable=str(self.old_exe), old_binary_sha256=self.pin['old'],
            old_session_observation=session_observation,
            new_executable=str(self.exe), new_binary_sha256=self.pin['binary'],
            new_profile=str(self.profile), protected_profile_hashes=self.protected(),
            old_app_retained=str(self.old_app), fresh_identity=True, secret_import=False,
            security_policy_modified=False, installer_sha256=g.hash_file(Path(__file__).absolute()))
        if self.windows:
            files = reviewed_windows_payload(g, self.candidate, self.archive, r, self.pin)
            result.update(archive_sha256=self.pin['archive'], archive_path=str(self.archive),
                          compiler_binary_path=str(self.compiler), payload=files, persistent_startup_added=False,
                          shortcut=str(self.shortcut),
                          new_profile_security='protected owner/SYSTEM/Administrators DACL at creation; never repair existing profile implicitly')
        else:
            require(subprocess.run(['systemctl', '--user', 'is-enabled', 'remote-play-current.service'], capture_output=True).returncode == 0, 'Old startup policy changed')
            result.update(old_unit_sha256=g.hash_file(self.old_unit), old_unit_enabled=True,
                          new_unit='weavelo.service', desktop_transaction='original install_linux_desktop.py')
        return result

    def save(self, state, initial=False):
        atomic_state(self.journal, state, initial)

    def launch_windows(self, old=False):
        exe = self.old_exe if old else self.exe
        return subprocess.Popen([str(exe)], env=self.runtime_env(old), stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)

    def unit_bytes(self):
        text = '[Unit]\nDescription=Weavelo user desktop\nAfter=graphical-session.target\n\n[Service]\nType=simple\n'
        text += f'ExecStart={self.exe}\nRestart=on-failure\nRestartSec=3\n'
        allowed = ['DISPLAY', 'WAYLAND_DISPLAY', 'XDG_RUNTIME_DIR', 'XDG_SESSION_TYPE']
        values = {k: os.environ[k] for k in allowed if k in os.environ}
        values.update(WEAVELO_DEVICE_GROUP_DIR=str(self.profile), WEAVELO_HEADLESS='0', WEAVELO_PASSIVE_HOST='0')
        for k, v in values.items():
            require('\n' not in v and '\r' not in v and '"' not in v and '%' not in v, 'Unsafe unit environment value')
            text += f'Environment="{k}={v}"\n'
        return (text+'\n[Install]\nWantedBy=default.target\n').encode()

    def apply(self, expected_plan):
        plan = self.plan()
        require(digest(plan) == expected_plan, 'Reviewed plan drift; no installation')
        self.work.mkdir(mode=0o700, parents=True, exist_ok=False)
        state = {'schema': 1, 'phase': 'prepared', 'plan_sha256': expected_plan, 'plan': plan}
        self.save(state, initial=True)
        child = None
        stopped = False
        desktop_plan = None
        try:
            if self.windows:
                shutil.copytree(self.candidate, self.stage)
                require(inventory(self.stage, self.gate) == plan['payload'], 'Staged payload drift')
                from windows_private_profile import NativeSecurity
                self.profile.parent.mkdir(parents=True, exist_ok=True)
                created = NativeSecurity().create_private_directory(self.profile)
                state['new_profile_creation'] = {
                    'file_identity': created['file_identity'], 'owner': created['owner'],
                    'protected': created['dacl_protected'],
                    'dacl_sha256': hashlib.sha256(created['sddl'].encode()).hexdigest(),
                    'existing_acl_repaired': False}
                self.save(state)
            else:
                self.stage.mkdir(mode=0o700)
                app = self.stage/'Weavelo'
                app.mkdir(mode=0o700)
                shutil.copy2(self.compiler, app/'weavelo')
                if any(entry['path'] == 'build-info.json' for entry in self.input_manifest_document['files']):
                    self.gate.write_new(app/'build-info.json', {'source_commit': self.commit,
                        'build_identity_sha256': self.pin['identity'], 'binary_sha256': self.pin['binary']})
                sealed = self.gate.seal_tree(self.stage, self.gate.read_json(self.receipt,self.pin['receipt']), self.commit,
                                            self.pin['identity'], compiler_binary=self.compiler)
                require(sealed['manifest_sha256'] == self.pin['manifest'], 'Reconstructed release manifest differs from reviewed release')
                state['manifest_sha256'] = sealed['manifest_sha256']
                unit = self.unit_bytes()
                state['new_unit_sha256'] = hashlib.sha256(unit).hexdigest()
                self.save(state)
                shutil.copy2(self.old_unit, self.work/'remote-play-current.service.before')
            self.native(plan['old_pid'], self.old_exe, self.pin['old'])
            self.require_idle()
            require(self.protected() == plan['protected_profile_hashes'], 'Old profile drift')
            if self.windows:
                from windows_process_lifecycle import stop_verified
                stop_verified(self, plan['old_pid'], self.old_exe, self.pin['old'])
                stopped = True
            else:
                require(self.gate.hash_file(self.old_unit) == plan['old_unit_sha256'], 'Old unit drift')
                self.ctl('stop', 'remote-play-current.service')
                stopped = True
                require(self.old_pid() == 0, 'Old owner still active')
                self.ctl('disable', 'remote-play-current.service')
            stopped = True
            state['phase'] = 'old_stopped'
            self.save(state)
            require(not self.live.exists(), 'New destination appeared')
            if self.windows:
                os.rename(self.stage, self.live)  # Windows refuses an existing destination.
            else:
                from install_linux_desktop import exclusive_rename
                exclusive_rename(self.stage/'Weavelo', self.live)
                exclusive_rename(self.stage/self.gate.MANIFEST, self.work/'installed-manifest.json')
                self.stage.rmdir()
                self.gate.verify_install(self.live, self.work/'installed-manifest.json', state['manifest_sha256'],
                    self.commit, self.pin['identity'], self.exe, self.exe)
                with self.new_unit.open('xb') as out:
                    out.write(unit)
                    out.flush()
                    os.fsync(out.fileno())
                subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True, timeout=30)
            state['phase'] = 'installed'
            self.save(state)
            if self.windows:
                child = self.launch_windows()
                pid = child.pid
            else:
                self.ctl('enable', 'weavelo.service')
                self.ctl('start', 'weavelo.service')
            deadline = time.monotonic()+35
            while True:
                if not self.windows:
                    pid = int(subprocess.check_output(['systemctl', '--user', 'show', 'weavelo.service', '-p', 'MainPID', '--value'], text=True).strip())
                try:
                    proof = self.native(pid, self.exe, self.pin['binary'], self.gate.read_json(self.receipt,self.pin['receipt'])['build_identity'])
                    require((self.profile/'mesh.conf').is_file() and (self.profile/'mesh.secret').is_file(), 'New profile not ready')
                    status = subprocess.run([str(self.exe), '--gui-status-json'], env=self.runtime_env(), text=True, capture_output=True, timeout=10)
                    gui = json.loads(status.stdout)
                    require(status.returncode == 0 and gui.get('ok') is True, 'New daily GUI not ready')
                    break
                except (RuntimeError, self.gate.IdentityRejected, ValueError):
                    require(time.monotonic() < deadline, 'New owner/profile/GUI could not be verified')
                    if child is not None:
                        require(child.poll() is None, 'New GUI exited')
                    time.sleep(.3)
            require(self.protected() == plan['protected_profile_hashes'], 'Old credentials changed')
            if self.windows:
                require(inventory(self.live, self.gate) == plan['payload'], 'Installed payload drift')
                require(not self.shortcut.exists(), 'Shortcut destination appeared')
                command = ("$ErrorActionPreference='Stop';$w=New-Object -ComObject WScript.Shell;"
                    f"$s=$w.CreateShortcut('{self.shortcut}');$s.TargetPath='{self.exe}';"
                    f"$s.WorkingDirectory='{self.live}';$s.IconLocation='{self.live/'Weavelo.ico'}';"
                    "$s.Description='Weavelo';$s.Save()")
                subprocess.run(['powershell.exe', '-NoProfile', '-Command', command], capture_output=True, check=True, timeout=20)
                state['shortcut_sha256'] = self.gate.hash_file(self.shortcut)
            else:
                require(self.gate.hash_file(self.new_unit) == state['new_unit_sha256'], 'New unit drift')
                require(self.old_pid() == 0, 'Old service restarted')
                desktop = self.repo/'scripts/install_linux_desktop.py'
                args = [sys.executable, '-B', str(desktop), '--assets', str(self.repo/'assets/brand/weavelo'),
                    '--assets-manifest-sha256', self.gate.hash_file(self.repo/'assets/brand/weavelo/SOURCE.json'),
                    '--app', str(self.live), '--release-manifest', str(self.work/'installed-manifest.json'),
                    '--manifest-sha256', state['manifest_sha256'], '--commit', self.commit, '--identity-sha256', self.pin['identity']]
                result = subprocess.run(args, capture_output=True, text=True, check=True, timeout=30)
                document = json.loads(result.stdout)
                desktop_plan = document['plan_sha256']
                self.gate.write_new(self.work/'desktop-plan.json', document)
                subprocess.run(args+['--apply', '--plan-sha256', desktop_plan], capture_output=True, text=True, check=True, timeout=30)
            state.update(phase='installed_and_running_verified', pid=pid, native=proof,
                gui={'ok': True, 'renderer': gui.get('renderer'), 'generation': gui.get('generation')},
                old_credentials_unchanged=True, fresh_profile_created=True,
                pairing='pending_normal_product_pairing', physical_acceptance='not_evaluated')
            self.save(state)
            print(json.dumps({k:v for k,v in state.items() if k != 'plan'}, indent=2))
        except Exception as error:
            state['error'] = type(error).__name__  # Never print runtime output/credentials.
            state['phase'] = 'failed_recovery_required'
            self.save(state)
            if stopped:
                if self.windows:
                    if child is not None and child.poll() is None:
                        self.native(child.pid, self.exe, self.pin['binary'], self.gate.read_json(self.receipt,self.pin['receipt'])['build_identity'])
                        child.terminate()
                        child.wait(timeout=15)
                    if state.get('shortcut_sha256'):
                        require(self.gate.hash_file(self.shortcut) == state['shortcut_sha256'], 'Shortcut changed outside transaction')
                        self.shortcut.unlink()
                    old = self.launch_windows(old=True)
                    time.sleep(2)
                    self.native(old.pid, self.old_exe, self.pin['old'])
                else:
                    if self.new_unit.exists():
                        require(self.gate.hash_file(self.new_unit) == state['new_unit_sha256'], 'New unit changed; preserve for recovery')
                        self.ctl('stop', 'weavelo.service')
                        self.ctl('disable', 'weavelo.service')
                    if desktop_plan:
                        subprocess.run([sys.executable, '-B', str(self.repo/'scripts/install_linux_desktop.py'), '--rollback', '--plan-sha256', desktop_plan], capture_output=True, check=True, timeout=30)
                    self.ctl('enable', 'remote-play-current.service')
                    self.ctl('start', 'remote-play-current.service')
                    self.native(self.old_pid(), self.old_exe, self.pin['old'])
                state['phase'] = 'rolled_back_old_owner_verified_new_files_retained'
                self.save(state)
            raise RuntimeError('Installation did not complete; observe original private journal; do not replay') from None


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--apply', action='store_true')
    p.add_argument('--plan-sha256')
    p.add_argument('--linux-update', action='store_true',
                   help='Update the existing same-profile Linux installation with retained rollback')
    p.add_argument('--prepare-update', action='store_true',
                   help='Seal an already observed Linux compiler output and record its installation plan')
    p.add_argument('--recover-update', action='store_true',
                   help='Recover only the original journal-bound Linux update by restoring its old image')
    p.add_argument('--reconcile-update', action='store_true',
                   help='Verify an already running Linux update after an exact reviewed CPUWeight change')
    p.add_argument('--configuration-sha256')
    for name in ('build-receipt','release-manifest','release-archive','compiler-binary'):
        p.add_argument('--'+name,type=Path)
    for name in ('build-receipt-sha256','manifest-sha256','archive-sha256','expected-commit','identity-sha256','old-binary-sha256'):
        p.add_argument('--'+name)
    args = p.parse_args()
    if args.linux_update:
        from linux_current_update import run
        run(args)
        return
    require(not args.prepare_update and not args.recover_update and not args.reconcile_update,
            'Linux update mode required')
    install = NativeInstall(args)
    if args.apply:
        require(args.plan_sha256 is not None, 'Reviewed plan required')
        install.apply(args.plan_sha256)
    else:
        plan = install.plan()
        print(json.dumps({'plan': plan, 'plan_sha256': digest(plan)}, indent=2))


if __name__ == '__main__':
    main()
