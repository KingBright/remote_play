#!/usr/bin/env python3
"""Report RemotePlay installation/launcher/process mismatches without starting it.

Only ps and launchctl print are executed. Legacy binaries may not recognize
--product-info-json, so this observer never executes any RemotePlay binary.
Membership and credential files are inspected with lstat only, never opened.
The report is evidence for diagnosis, not signing, visual or streaming acceptance.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import plistlib
import re
import stat
import subprocess
import sys

LABEL = 'com.remoteplay.host'
ORIGINAL = 'restored-original-gpui'
LIMIT = 65536


def read_document(path: Path, kind: str) -> dict:
    """Bound regular-file reads; report errors without echoing file contents."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, 'rb') as handle:
            if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                return {'status': 'not_regular_file'}
            data = handle.read(LIMIT + 1)
        if len(data) > LIMIT:
            return {'status': 'oversized'}
        value = plistlib.loads(data) if kind == 'plist' else json.loads(data)
        if not isinstance(value, dict):
            return {'status': 'invalid_document'}
        return {'status': 'observed', 'value': value}
    except FileNotFoundError:
        return {'status': 'missing'}
    except PermissionError:
        return {'status': 'permission_denied'}
    except (OSError, ValueError, TypeError, RecursionError, plistlib.InvalidFileException):
        return {'status': 'unreadable_or_invalid'}


def text_field(value: object) -> str | None:
    return value if isinstance(value, str) and len(value) <= 1024 else None


def bundle_info(app: Path) -> dict:
    result = read_document(app / 'Contents/Info.plist', 'plist')
    data = result.pop('value', {})
    result.update(path=str(app), version=text_field(data.get('RemotePlayReleaseVersion')),
                  build=text_field(data.get('CFBundleVersion')),
                  bundle_id=text_field(data.get('CFBundleIdentifier')),
                  executable=text_field(data.get('CFBundleExecutable')))
    return result


def file_metadata(path: Path) -> dict:
    try:
        info = path.lstat()
        return {'status': 'observed', 'owner_uid': info.st_uid,
                'mode': format(stat.S_IMODE(info.st_mode), '04o'),
                'regular_file': stat.S_ISREG(info.st_mode),
                'symlink': stat.S_ISLNK(info.st_mode)}
    except FileNotFoundError:
        return {'status': 'missing'}
    except OSError:
        return {'status': 'unavailable'}


def observe_command(argv: list[str], runner) -> dict:
    try:
        run = runner(argv, capture_output=True, text=True, timeout=10)
        if len(run.stdout) > 1048576 or len(run.stderr) > 65536:
            return {'status': 'oversized'}
        if run.returncode:
            if argv[0] == '/bin/launchctl' and 'Could not find service' in run.stderr:
                return {'status': 'not_loaded'}
            return {'status': 'command_failed', 'exit_code': run.returncode}
        return {'status': 'observed', 'output': run.stdout}
    except (OSError, subprocess.SubprocessError):
        return {'status': 'command_unavailable'}


def collect(home: Path, uid: int, expected_version: str | None = None,
            runner=subprocess.run) -> dict:
    app = home / 'Applications/RemotePlay.app'
    executable = app / 'Contents/MacOS/remote_play'
    profile = home / 'Library/Application Support/RemotePlay/NativeMesh'
    installed = bundle_info(app)
    launcher = read_document(home / 'Library/LaunchAgents' / (LABEL + '.plist'), 'plist')
    data = launcher.pop('value', {})
    args = data.get('ProgramArguments')
    # Do not echo arbitrary launcher arguments or environment variables.
    arguments_match = args == [str(executable)]
    program = text_field(data.get('Program'))
    launcher.update(label_matches=data.get('Label') == LABEL,
                    program=program,
                    argument_executable=text_field(args[0]) if isinstance(args, list) and args else None,
                    arguments_match=arguments_match,
                    canonical=launcher['status'] == 'observed' and data.get('Label') == LABEL
                    and arguments_match and program in (None, str(executable)))

    service = observe_command(['/bin/launchctl', 'print', f'gui/{uid}/{LABEL}'], runner)
    output = service.pop('output', '')
    for key in ('program', 'state'):
        match = re.search(r'^\s*' + key + r' = (.+)$', output, re.M)
        service[key] = text_field(match[1].strip()) if match else None
    pid = re.search(r'^\s*pid = (\d+)\s*$', output, re.M)
    service['pid'] = int(pid[1]) if pid else None

    process_probe = observe_command(['/bin/ps', '-axo', 'pid=,comm='], runner)
    processes = []
    for line in process_probe.pop('output', '').splitlines():
        parts = line.strip().split(None, 1)
        if len(parts) != 2 or not parts[0].isdigit() or Path(parts[1]).name != 'remote_play':
            continue
        path = Path(parts[1])
        item = {'pid': int(parts[0]), 'executable': str(path),
                'canonical_path': path == executable,
                'running_version': 'not_verified_from_process'}
        if path.is_absolute() and path.parent.name == 'MacOS' and path.parent.parent.name == 'Contents' and path.parents[2].suffix == '.app':
            item['on_disk_bundle'] = bundle_info(path.parents[2])
        processes.append(item)

    runtime = read_document(profile / 'desktop-instance.renderer.json', 'json')
    data = runtime.pop('value', {})
    renderer = data.get('renderer')
    runtime['renderer'] = renderer if type(data.get('schema')) is int and data['schema'] == 1 and renderer in (ORIGINAL, 'egui-diagnostic', 'headless') else None
    runtime['process_binding'] = 'unavailable'
    runtime['live_gui_verified'] = False
    protected = {'membership': file_metadata(profile / 'mesh.conf'),
                 'identity': file_metadata(profile / 'mesh.secret')}
    findings = []
    if expected_version and installed['version'] and installed['version'] != expected_version:
        findings.append('installed_version_differs_from_expected')
    if launcher['status'] == 'observed' and not launcher['canonical']:
        findings.append('launcher_does_not_match_canonical_app')
    if service['status'] == 'observed' and service['program'] != str(executable):
        findings.append('loaded_service_does_not_match_canonical_app')
    if len(processes) > 1:
        findings.append('multiple_remoteplay_processes')
    if any(not p['canonical_path'] for p in processes):
        findings.append('noncanonical_remoteplay_process')
    if any(m.get('regular_file') and m.get('owner_uid') != uid for m in protected.values()):
        findings.append('protected_configuration_owned_by_another_user')
    if runtime['renderer'] in ('egui-diagnostic', 'headless'):
        findings.append('profile_renderer_metadata_is_not_original_gui')
    if runtime['renderer'] is None:
        findings.append('profile_renderer_metadata_unavailable')
    incomplete = (installed['status'] != 'observed' or process_probe['status'] != 'observed'
                  or service['status'] not in ('observed', 'not_loaded')
                  or launcher['status'] not in ('observed', 'missing'))
    return {'schema': 1, 'observed_at_utc': datetime.now(timezone.utc).isoformat(),
            'platform': 'macos', 'expected_version': expected_version,
            'diagnostic_status': 'incomplete' if incomplete else 'observed',
            'installed': installed, 'launcher': launcher, 'loaded_service': service,
            'process_probe': process_probe, 'processes': processes,
            'profile_renderer_metadata': runtime, 'protected_file_metadata': protected,
            'findings': findings,
            'acceptance': {'signing': 'not_evaluated', 'visual': 'not_evaluated',
                           'streaming': 'not_evaluated', 'input': 'not_evaluated',
                           'long_duration': 'not_evaluated'},
            'application_started': False, 'configuration_contents_read': False,
            'system_mutated': False}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--expected-version', help='Version expected at the canonical installation')
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('Run this macOS observer on the selected Mac')
    report = collect(Path.home(), os.getuid(), args.expected_version)
    print(json.dumps(report, indent=2))
    return 2 if report['diagnostic_status'] == 'incomplete' else 0


if __name__ == '__main__':
    sys.exit(main())
