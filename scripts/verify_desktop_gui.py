#!/usr/bin/env python3
"""Verify a built RemotePlay GUI before packaging; does not start a GUI or mesh.

The binary's --product-info-json path returns before profile creation. This gate
checks compiled presentation identity, not visual, functional or performance QA.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

class GuiReleaseRejected(RuntimeError):
    pass

def validate(info: dict, platform: str, version: str) -> None:
    if 'diagnostic_gui_requires_opt_in' in info:
        raise GuiReleaseRejected('Build metadata still advertises the legacy diagnostic GUI')
    required = {'schema': 1, 'product': 'RemotePlay', 'platform': platform,
                'version': version, 'default_gui': 'restored-original-gpui',
                'original_gui_compiled': True, 'native_video_compiled': True}
    for key, value in required.items():
        if key not in info or type(info[key]) is not type(value) or info[key] != value:
            raise GuiReleaseRejected(f'Product GUI contract mismatch for {key}: {info.get(key)!r}; expected {value!r}')
    if info.get('architecture') not in ('aarch64', 'x86_64'):
        raise GuiReleaseRejected('Unsupported or missing compiled architecture')

def verify(binary: Path, platform: str, version: str) -> dict:
    binary = binary.resolve(strict=True)
    if not binary.is_file():
        raise GuiReleaseRejected('Product binary is not a regular file')
    before = hashlib.sha256(binary.read_bytes()).hexdigest()
    env = os.environ.copy()
    for key in ('REMOTE_PLAY_LEGACY_MAC_GUI',):
        env.pop(key, None)
    run = subprocess.run([str(binary), '--product-info-json'], env=env,
                         capture_output=True, text=True, timeout=15)
    if run.returncode != 0 or len(run.stdout) > 16384:
        raise GuiReleaseRejected('Binary did not return bounded product metadata; do not package it as the restored GUI')
    try:
        info = json.loads(run.stdout)
    except (ValueError, TypeError) as error:
        raise GuiReleaseRejected('Binary did not emit product-info JSON') from error
    if not isinstance(info, dict):
        raise GuiReleaseRejected('Product metadata must be an object')
    validate(info, platform, version)
    if hashlib.sha256(binary.read_bytes()).hexdigest() != before:
        raise GuiReleaseRejected('Binary changed during verification')
    return {'binary_sha256': before, 'product_info': info,
            'gui_identity_verified': True, 'visual_acceptance': 'not_evaluated',
            'functional_acceptance': 'not_evaluated', 'network_started': False}

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--platform', choices=('macos', 'linux', 'windows'), required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--receipt', type=Path)
    args = parser.parse_args()
    if args.receipt and args.receipt.exists():
        parser.error('Do not overwrite a GUI verification receipt')
    result = verify(args.binary, args.platform, args.version)
    data = json.dumps(result, indent=2) + '\n'
    if args.receipt:
        with args.receipt.open('x', encoding='utf-8') as handle:
            handle.write(data)
    print(data, end='')

if __name__ == '__main__':
    main()
