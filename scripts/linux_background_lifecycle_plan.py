#!/usr/bin/env python3
"""Pure, same-profile systemd user-service migration and rollback proposals.

No subprocesses, service writes, profile reads, credential generation or apply
entry exist here. Product capability fields are not native identity proof. An
authorized executor must reverify original product identity, plan CAS, quiet
ownership and exact marker ownership before every actual cutover or rollback.
"""
from __future__ import annotations
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re

from background_lifecycle_plan import LifecyclePlanRejected, require_separated_entries

UNIT_NAME = 'weavelo.service'
MARKER = 'background-service.enabled'
ENVIRONMENT = {'DISPLAY', 'WAYLAND_DISPLAY', 'XDG_RUNTIME_DIR', 'XDG_SESSION_TYPE',
               'WEAVELO_DEVICE_GROUP_DIR', 'WEAVELO_HEADLESS', 'WEAVELO_PASSIVE_HOST'}


def require(ok, message):
    if not ok:
        raise LifecyclePlanRejected(message)


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def digest(document):
    return sha(json.dumps(document, sort_keys=True, separators=(',', ':')).encode())


def hash_pin(value):
    require(isinstance(value, str) and re.fullmatch('[0-9a-f]{64}', value), 'Exact SHA256 pin required')


def canonical_path(value):
    require(isinstance(value, str) and value.startswith('/') and
            str(PurePosixPath(value)) == value and '..' not in PurePosixPath(value).parts and
            re.fullmatch(r'/[A-Za-z0-9_./-]+', value), 'Unsupported or noncanonical path')


def validate_profile(state):
    require(isinstance(state, dict) and set(state) == {'directory', 'files'}, 'Exact private profile state required')
    directory = state['directory']
    require(isinstance(directory, dict) and set(directory) == {'device', 'inode', 'uid', 'mode'}, 'Unknown directory state')
    require(directory['mode'] == 0o700, 'Existing private directory mode required; never repair it')
    for field in ('device', 'inode', 'uid', 'mode'):
        require(type(directory[field]) is int and directory[field] >= 0, 'Invalid directory identity')
    require(directory['inode'] > 0, 'Directory inode required')
    files = state['files']
    require(isinstance(files, dict) and set(files) == {'mesh.conf', 'mesh.secret'}, 'Existing authentication pair required')
    for item in files.values():
        require(isinstance(item, dict) and set(item) == {'device', 'inode', 'uid', 'mode', 'links', 'sha256'}, 'Metadata and hash pins only; no credential contents')
        for field in ('device', 'inode', 'uid', 'mode', 'links'):
            require(type(item[field]) is int and item[field] >= 0, 'Invalid file identity')
        require(item['inode'] > 0 and item['uid'] == directory['uid'] and item['links'] == 1 and item['mode'] == 0o600,
                'Existing same-user private ordinary files required')
        hash_pin(item['sha256'])


def rewrite_unit(raw, executable, profile):
    require(type(raw) is bytes and 0 < len(raw) <= 65536, 'Bounded original unit bytes required')
    text = raw.decode('utf-8')
    require('\r' not in text and '\x00' not in text and text.endswith('\n'), 'Unsupported unit encoding')
    allowed = {'Unit': {'Description', 'After'},
               'Service': {'Type', 'ExecStart', 'Restart', 'RestartSec', 'Environment'},
               'Install': {'WantedBy'}}
    seen = {}; environment = {}; section = None; sections = set(); out = []
    for line in text.splitlines(keepends=True):
        stripped = line.strip()
        if not stripped or stripped.startswith(('#', ';')):
            out.append(line)
            continue
        require('\\' not in stripped, 'Continuation/escape semantics require separate review')
        if stripped.startswith('['):
            section = stripped[1:-1]
            require(stripped == '[' + section + ']' and section in allowed and section not in sections,
                    'Unknown or repeated unit section')
            sections.add(section); out.append(line)
            continue
        require(section in allowed and '=' in stripped, 'Unknown unit syntax')
        key, value = stripped.split('=', 1)
        require(key in allowed[section], 'Unsupported unit directive; no generic systemd rewriting')
        if key == 'Environment':
            match = re.fullmatch(r'"([A-Z_]+)=([^"\\$%\x00-\x1f]*)"', value)
            require(match is not None, 'Exact original quoted environment entry required')
            name, entry = match.groups()
            require(name in ENVIRONMENT and name not in environment, 'Unknown or duplicate environment assignment')
            environment[name] = entry
            if name == 'WEAVELO_HEADLESS':
                require(entry == '0', 'Already migrated or unknown GUI role')
                line = 'Environment="WEAVELO_HEADLESS=1"\n'
        else:
            token = (section, key)
            require(token not in seen, 'Repeated directive could override reviewed semantics')
            seen[token] = value
            if key == 'ExecStart':
                require(value == executable, 'Unexpected original executable/arguments')
                line = 'ExecStart=' + executable + ' --background-service\n'
        out.append(line)
    require(sections == set(allowed), 'Original Unit/Service/Install sections required')
    require(seen.get(('Service', 'Type')) == 'simple' and
            seen.get(('Service', 'ExecStart')) == executable and
            seen.get(('Service', 'Restart')) == 'on-failure' and
            seen.get(('Service', 'RestartSec')) == '3' and
            seen.get(('Unit', 'After')) == 'graphical-session.target' and
            seen.get(('Install', 'WantedBy')) == 'default.target', 'Original desktop service policy differs')
    require(environment.get('WEAVELO_DEVICE_GROUP_DIR') == profile and
            environment.get('WEAVELO_HEADLESS') == '0' and
            environment.get('WEAVELO_PASSIVE_HOST') == '0', 'Exact existing profile and GUI policy required')
    return ''.join(out).encode()


def plan(unit_before, *, home, product_info, binary_sha256, source_commit, profile_state, unit_state, observation, unit_security_sha256=None):
    canonical_path(home)
    executable = home + '/.local/opt/weavelo/weavelo'
    profile = home + '/.config/weavelo/mesh'
    require_separated_entries(product_info)
    hash_pin(binary_sha256)
    if unit_security_sha256 is not None: hash_pin(unit_security_sha256)
    require(isinstance(source_commit, str) and re.fullmatch('[0-9a-f]{40}', source_commit), 'Exact existing source commit required')
    validate_profile(profile_state)
    require(isinstance(unit_state, dict) and set(unit_state) == {'device', 'inode', 'uid', 'mode', 'links', 'sha256'}, 'Original unit inode and byte pins required')
    for field in ('device', 'inode', 'uid', 'mode', 'links'):
        require(type(unit_state[field]) is int and unit_state[field] >= 0, 'Invalid original unit identity')
    require(unit_state['inode'] > 0 and unit_state['uid'] == profile_state['directory']['uid'] and
            unit_state['mode'] in (0o600, 0o644) and unit_state['links'] == 1 and
            unit_state['sha256'] == sha(unit_before), 'Original ordinary same-user unit state differs')
    require(isinstance(observation, dict) and observation.get('owner_mode') == 'Standalone' and
            type(observation.get('pid')) is int and observation['pid'] > 0 and
            isinstance(observation.get('generation'), str) and observation['generation'], 'Known live standalone owner required')
    require(observation.get('binary_sha256') == binary_sha256, 'Observed installed-image pin differs')
    require(observation.get('active_sessions') == 0 and type(observation.get('active_sessions')) is int and
            observation.get('pending_sessions') == 0 and type(observation.get('pending_sessions')) is int and
            observation.get('active_lease') is False, 'Busy or unknown sessions/lease; preserve current owner')
    require(observation.get('drop_ins') == [] and type(observation.get('drop_ins')) is list and
            observation.get('unit_active') is True and type(observation.get('unit_enabled')) is bool and
            observation.get('marker_present') is False, 'Unknown drop-in, service or role-marker state')
    unit_after = rewrite_unit(unit_before, executable, profile)
    result = {'schema': 1, 'kind': 'linux-same-profile-background-migration-plan',
        'home': home, 'unit_name': UNIT_NAME, 'executable': executable, 'profile': profile,
        'binary_sha256': binary_sha256, 'source_commit': source_commit,
        'unit_before_sha256': sha(unit_before), 'unit_after_sha256': sha(unit_after),
        'unit_security_sha256': unit_security_sha256,
        'unit_after_utf8': unit_after.decode(), 'unit_state': copy.deepcopy(unit_state),
        'profile_state': copy.deepcopy(profile_state),
        'observation': copy.deepcopy({k: observation[k] for k in ('owner_mode', 'pid', 'generation',
            'binary_sha256', 'active_sessions', 'pending_sessions', 'active_lease', 'drop_ins',
            'unit_active', 'unit_enabled', 'marker_present')}),
        'unit_enabled_after': observation['unit_enabled'], 'manual_gui_arguments': [executable, '--gui'],
        'desktop_entry_modified': False, 'credentials_created_or_imported': False,
        'profile_permissions_modified': False, 'applied': False,
        'marker': {'path': profile + '/' + MARKER, 'absent_before': True,
                   'created_by': 'original --background-service product entry',
                   'rollback': 'stop exact owned background and attached GUIs; remove only receipt-bound zero-length private marker'},
        'native_product_identity_verification': 'required_at_apply; supplied capabilities are not proof',
        'real_gui_close_survival_acceptance': 'not_evaluated'}
    result['plan_sha256'] = digest(result)
    return result


def rollback_preview(migration, *, expected_plan_sha256, original_unit, current_unit,
                     current_profile_state, owned_unit, current_unit_state, owned_marker, current_marker,
                     background_stopped, attached_gui_pids, marker_absence_verified=False):
    expected = dict(migration); actual_digest = expected.pop('plan_sha256', None)
    require(actual_digest == expected_plan_sha256 == digest(expected), 'Recorded plan CAS mismatch')
    require(sha(original_unit) == migration['unit_before_sha256'] and
            sha(current_unit) == migration['unit_after_sha256'], 'Unit bytes changed; preserve unknown edits')
    require(isinstance(owned_unit, dict) and owned_unit == current_unit_state and
            set(owned_unit) == {'device', 'inode', 'uid', 'mode', 'links', 'sha256'},
            'Unknown or replaced after-unit inode; preserve it')
    for field in ('device', 'inode', 'uid', 'mode', 'links'):
        require(type(owned_unit[field]) is int and owned_unit[field] >= 0, 'Invalid owned unit identity')
    require(owned_unit['inode'] > 0 and owned_unit['uid'] == migration['unit_state']['uid'] and
            owned_unit['mode'] == migration['unit_state']['mode'] and owned_unit['links'] == 1 and
            owned_unit['sha256'] == migration['unit_after_sha256'], 'After-unit differs from producer receipt')
    validate_profile(current_profile_state)
    require(current_profile_state == migration['profile_state'], 'Same-identity profile changed; no rollback overwrite')
    require(background_stopped is True and attached_gui_pids == [] and type(attached_gui_pids) is list,
            'Stop and verify only recorded owners before rollback')
    if owned_marker is None and current_marker is None:
        require(marker_absence_verified is True, 'Marker absence must be independently verified, not unknown')
    else:
        require(isinstance(owned_marker, dict) and owned_marker == current_marker and
                set(owned_marker) == {'path', 'device', 'inode', 'uid', 'mode', 'size', 'links', 'sha256'},
                'Unknown or replaced role marker; preserve it')
        for field in ('device', 'inode', 'uid', 'mode', 'size', 'links'):
            require(type(owned_marker[field]) is int and owned_marker[field] >= 0, 'Invalid owned marker identity')
        require(owned_marker['path'] == migration['marker']['path'] and owned_marker['size'] == 0 and
                owned_marker['mode'] == 0o600 and owned_marker['links'] == 1 and
                owned_marker['uid'] == migration['profile_state']['directory']['uid'] and
                type(owned_marker['inode']) is int and owned_marker['inode'] > 0 and
                owned_marker['sha256'] == sha(b''), 'Marker must be exact owned empty private ordinary inode')
    return {'unit_restore_utf8': original_unit.decode(), 'unit_restore_sha256': sha(original_unit),
            'marker_remove_only_if_unchanged': copy.deepcopy(owned_marker),
            'unit_replace_only_if_unchanged': copy.deepcopy(owned_unit),
            'unit_restore_mode': migration['unit_state']['mode'],
            'restart_arguments': [migration['executable']], 'unit_enabled': migration['unit_enabled_after'],
            'credentials_or_permissions_modified': False, 'applied': False}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--unit', type=Path, required=True)
    p.add_argument('--facts', type=Path, required=True, help='Reviewed existing identity/metadata/quiet-owner facts JSON')
    p.add_argument('--output', type=Path, required=True, help='New private proposal; existing files refused')
    a = p.parse_args()
    require(a.unit.stat().st_size <= 65536 and a.facts.stat().st_size <= 65536, 'Bounded proposal inputs required')
    facts = json.loads(a.facts.read_text())
    proposal = plan(a.unit.read_bytes(), **facts)
    parent = a.output.parent.resolve(strict=True)
    require(not a.output.is_symlink() and parent.stat().st_uid == os.getuid() and
            parent.stat().st_mode & 0o077 == 0, 'Existing private same-user output directory required')
    fd = os.open(parent/a.output.name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as out:
        json.dump(proposal, out, sort_keys=True, indent=2); out.write('\n'); out.flush(); os.fsync(out.fileno())
    print(json.dumps({'plan_sha256': proposal['plan_sha256'], 'applied': False,
                      'credentials_created_or_imported': False, 'proposal': str(parent/a.output.name)}))


if __name__ == '__main__':
    main()
