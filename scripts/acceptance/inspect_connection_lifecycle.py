#!/usr/bin/env python3
"""Read scalar lifecycle diagnostics for one known connection. No mutation."""
import argparse
import json
import os
from pathlib import Path
import re
import socket
import stat
import struct
import time

LIMIT = 262144
COUNTERS = ('active_capture_workers', 'native_streams', 'native_stops_pending', 'native_stops_completed',
            'native_stop_errors', 'captured_frames', 'encoded_frames')


def require(value, reason):
    if not value:
        raise ValueError(reason)


def read_exact(stream, size):
    result = b''
    while len(result) < size:
        part = stream.recv(size - len(result))
        require(part, 'private IPC ended before response')
        result += part
    return result


def request(profile, command):
    # Existing same-user endpoint only. Never create an endpoint/profile.
    endpoint = profile / 'background.sock'
    for path, kind in ((profile, stat.S_ISDIR), (endpoint, stat.S_ISSOCK)):
        metadata = path.lstat()
        require(kind(metadata.st_mode) and metadata.st_uid == os.getuid()
                and stat.S_IMODE(metadata.st_mode) & 0o077 == 0,
                'existing private same-user IPC endpoint required')
    payload = json.dumps(command).encode()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(2)
        stream.connect(str(endpoint))
        stream.sendall(struct.pack('!I', len(payload)) + payload)
        size = struct.unpack('!I', read_exact(stream, 4))[0]
        require(0 < size <= LIMIT, 'invalid bounded IPC response')
        return json.loads(read_exact(stream, size))


def validate(records, connection_id):
    require(isinstance(records, list) and len(records) == 1,
            'known connection absent or ID collision; no inferred teardown')
    record = records[0]
    require(record.get('connection_id') == connection_id, 'connection ID mismatch')
    require(record.get('state') in ('open', 'closing', 'closed', 'drop_requested', 'teardown_failed', 'teardown_unknown'),
            'unknown lifecycle state')
    require(all(type(record.get(key)) is int and record[key] >= 0 for key in COUNTERS),
            'missing or invalid native counters')
    require(type(record.get('observed_at_ms')) is int, 'missing observation time')
    return {key: record[key] for key in ('connection_id', 'state', 'observed_at_ms') + COUNTERS}


def confirmed_closed(first, second):
    # Require a native start/stop and encoder evidence for this video fixture.
    for record in (first, second):
        require(record['state'] == 'closed' and record['active_capture_workers'] == 0
                and record['native_streams'] == 0 and record['native_stops_pending'] == 0 and record['native_stop_errors'] == 0
                and record['native_stops_completed'] > 0 and record['encoded_frames'] > 0,
                'native video teardown was not confirmed')
    require(all(first[key] == second[key] for key in COUNTERS),
            'capture/encode counters still changing after Closed')


def inspect(profile, connection_id, expected_backend_sha256, expect_closed=False):
    require(re.fullmatch('[0-9a-f]{64}', expected_backend_sha256), 'exact backend hash required')
    require(type(connection_id) is int and 0 < connection_id <= 0xffffffff,
            'known nonzero connection ID required')
    # Snapshot includes existing pairing material; retain in memory, never export.
    snapshot = request(profile, 'Snapshot').get('Snapshot', {})
    require(snapshot.get('executable_sha256') == expected_backend_sha256,
            'installed backend differs from pinned candidate')
    command = {'InspectConnection': {'connection_id': connection_id}}
    first = validate(request(profile, command).get('ConnectionDiagnostics'), connection_id)
    samples = [first]
    if expect_closed:
        time.sleep(0.2)
        second = validate(request(profile, command).get('ConnectionDiagnostics'), connection_id)
        confirmed_closed(first, second)
        samples.append(second)
    final_snapshot = request(profile, 'Snapshot').get('Snapshot', {})
    require(all(snapshot.get(key) == final_snapshot.get(key)
                for key in ('pid', 'generation', 'executable_sha256')),
            'backend changed during inspection')
    return {'schema': 1, 'kind': 'known-connection-native-lifecycle-inspection',
            'backend_sha256': expected_backend_sha256, 'backend_pid': snapshot['pid'],
            'samples': samples, 'closed_video_resources_confirmed': expect_closed,
            'mutation_or_input_sent': False, 'credentials_or_frame_contents_exported': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--connection-id', type=int, required=True)
    parser.add_argument('--backend-sha256', required=True)
    parser.add_argument('--expect-closed', action='store_true')
    args = parser.parse_args()
    try:
        result = inspect(args.profile, args.connection_id, args.backend_sha256, args.expect_closed)
    except (ValueError, OSError, KeyError, TypeError):
        raise SystemExit('Lifecycle inspection failed closed; no cleanup was inferred or performed')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
