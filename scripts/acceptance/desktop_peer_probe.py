#!/usr/bin/env python3
"""Prepare duplex payloads and inspect production endpoints without changing pairing.

A content match or a healthy endpoint does not prove a native file transfer.
Use normal invitations and the application's file workflow, retaining both
native transfer receipts before calling the two-device acceptance complete.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess

SIZE = 262181
GROUP_KEYS = ('control_sha256', 'discovery_sha256')


def require(ok, reason):
    if not ok:
        raise ValueError(reason)


def payload(side):
    require(side in ('book', 'studio'), 'Unknown payload origin')
    seed = ('weavelo-desktop-duplex-v1:' + side).encode()
    return b''.join(hashlib.sha256(seed + i.to_bytes(4, 'big')).digest()
                    for i in range((SIZE + 31) // 32))[:SIZE]


def regular(path):
    require(path.is_absolute(), 'Absolute file path required')
    require(not any(p.is_symlink() for p in (path, *path.parents)), 'Linked input rejected')
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid(), 'Owned regular file required')


def digest(path):
    regular(path)
    with path.open('rb') as incoming:
        return hashlib.file_digest(incoming, 'sha256').hexdigest()


def prepare(directory, side):
    require(directory.is_absolute(), 'Absolute fixture directory required')
    require(not any(p.is_symlink() for p in (directory, *directory.parents)), 'Linked fixture directory rejected')
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    require(directory.stat().st_uid == os.getuid()
            and stat.S_IMODE(directory.stat().st_mode) == 0o700, 'Private owned fixture directory required')
    path = directory / ('from-' + side + '.bin')
    data = payload(side)
    if not path.exists():
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'wb') as out:
            out.write(data)
            out.flush()
            os.fsync(out.fileno())
    proof = verify_content(path, side)
    return dict(proof, path=str(path), prepared=True)


def verify_content(path, side):
    regular(path)
    require(path.stat().st_size == SIZE, 'Unexpected payload size')
    expected = hashlib.sha256(payload(side)).hexdigest()
    require(digest(path) == expected, 'Received bytes differ from the selected origin')
    return {'payload_origin': side, 'bytes': SIZE, 'sha256': expected,
            'content_verified': True, 'native_transfer_verified': False}


def readiness(background, frontend, expected_binary, peer_groups=None):
    reasons = []
    if background.get('role') != 'background-service' or frontend.get('ok') is not True:
        reasons.append('production_owners_unavailable')
    if not background.get('pid') or not frontend.get('pid') or background.get('pid') == frontend.get('pid'):
        reasons.append('separate_frontend_and_background_required')
    if any(x.get('executable_sha256') != expected_binary for x in (background, frontend)):
        reasons.append('executable_binding_mismatch')
    b = (background.get('network_diagnostics') or {}).get('network') or {}
    g = frontend.get('network') or {}
    groups = [{k: (n.get('relay_groups') or {}).get(k) for k in GROUP_KEYS} for n in (b, g)]
    if (not all(isinstance(v, str) and len(v) == 64 for v in groups[0].values())
            or groups[0] != groups[1]
            or not b.get('runtime_generation')
            or b.get('runtime_generation') != g.get('runtime_generation')):
        reasons.append('frontend_background_group_or_generation_mismatch')
    for n in (b, g):
        for channel in ('relay_control', 'relay_discovery'):
            t = n.get(channel) or {}
            if not (t.get('state') == 'connected' and t.get('stage') == 'active'
                    and t.get('http_status') == 101 and t.get('registrations_sent', 0) > 0
                    and t.get('pongs_received', 0) > 0):
                reasons.append('relay_registration_not_observed')
    counts = [background.get('discovered_routes', 0),
              (b.get('owner_projection') or {}).get('online_devices') or 0,
              (g.get('gui_projection') or {}).get('online_devices') or 0]
    if min(counts) < 1:
        reasons.append('no_discovered_peer')
    if peer_groups is None:
        reasons.append('other_endpoint_group_digests_required')
    elif groups[0] != peer_groups:
        reasons.append('other_endpoint_group_mismatch')
    return {'background_pid': background.get('pid'), 'frontend_pid': frontend.get('pid'),
            'relay_group_digests': groups[0], 'discovery_counts': counts,
            'ready_for_peer_verification': not reasons,
            'reasons': sorted(set(reasons)), 'native_transfer_verified': False}


def network(binary, profile, expected_binary, peer_groups):
    require(len(expected_binary) == 64 and digest(binary) == expected_binary, 'Independent executable digest differs')
    require(profile.is_absolute() and profile.is_dir() and not profile.is_symlink()
            and profile.stat().st_uid == os.getuid()
            and stat.S_IMODE(profile.stat().st_mode) == 0o700, 'Private production profile required')
    env = {k: v for k, v in os.environ.items() if not k.startswith('WEAVELO_') and k != 'RUST_LOG'}
    env.update(WEAVELO_DEVICE_GROUP_DIR=str(profile), WEAVELO_HEADLESS='0')
    values = []
    for arg in ('--background-status-json', '--gui-network-diagnostics-json'):
        result = subprocess.run([str(binary), arg], env=env, capture_output=True, text=True, timeout=20)
        require(result.returncode == 0, 'Native safe diagnostics failed; no restart attempted')
        values.append(json.loads(result.stdout))
    return readiness(*values, expected_binary, peer_groups)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    p = commands.add_parser('prepare')
    p.add_argument('--directory', required=True, type=Path)
    p.add_argument('--side', required=True, choices=('book', 'studio'))
    p = commands.add_parser('verify-content')
    p.add_argument('--file', required=True, type=Path)
    p.add_argument('--origin', required=True, choices=('book', 'studio'))
    p = commands.add_parser('network')
    p.add_argument('--binary', required=True, type=Path)
    p.add_argument('--profile', required=True, type=Path)
    p.add_argument('--expected-binary-sha256', required=True)
    p.add_argument('--peer-control-sha256')
    p.add_argument('--peer-discovery-sha256')
    args = parser.parse_args()
    if args.command == 'prepare':
        result = prepare(args.directory, args.side)
    elif args.command == 'verify-content':
        result = verify_content(args.file, args.origin)
    else:
        values = (args.peer_control_sha256, args.peer_discovery_sha256)
        require(not any(values) or all(values), 'Both other-endpoint group digests are required together')
        peer = dict(zip(GROUP_KEYS, values)) if all(values) else None
        result = network(args.binary, args.profile, args.expected_binary_sha256, peer)
    print(json.dumps(result))
    return 3 if result.get('ready_for_peer_verification') is False else 0


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (ValueError, OSError, subprocess.TimeoutExpired) as error:
        print(json.dumps({'verified': False, 'error': type(error).__name__, 'reason': str(error)}))
        raise SystemExit(2)
