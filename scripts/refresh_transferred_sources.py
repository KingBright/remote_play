#!/usr/bin/env python3
"""Verify a completed source delta and invalidate timestamp-based compiler caches.

Run after transfer and before compiling, never concurrently with a build. This
changes only modification times of explicitly listed, hash-verified source files.
It does not delete build caches, update dependencies or certify a compiled binary.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import time
from typing import Any


def refresh(root: Path, delta: dict[str, dict[str, Any]], *, apply: bool = False) -> dict[str, Any]:
    root = root.resolve(strict=True)
    if not delta or len(delta) > 10000:
        raise ValueError('delta must list between 1 and 10000 source files')
    checked: list[tuple[str, Path, str, int]] = []
    for name, entry in delta.items():
        relative = PurePosixPath(name)
        if not name or relative.is_absolute() or any(p in ('..', '.') for p in relative.parts) or '\\' in name or ':' in name:
            raise ValueError(f'unsafe source path: {name!r}')
        path = root.joinpath(*relative.parts)
        if not path.is_file() or path.is_symlink() or not path.resolve().is_relative_to(root):
            raise ValueError(f'source is not a contained regular file: {name}')
        expected = entry.get('after')
        if not isinstance(expected, str) or len(expected) != 64 or any(c not in '0123456789abcdef' for c in expected):
            raise ValueError(f'valid lowercase after SHA-256 required: {name}')
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if digest != expected:
            raise ValueError(f'transferred source differs from delta: {name}')
        checked.append((name, path, digest, path.stat().st_mtime_ns))
    # No timestamp is touched until the entire source delta has passed verification.
    changed = []
    for name, path, digest, previous in checked:
        if apply:
            stat = path.stat()
            os.utime(path, ns=(stat.st_atime_ns, time.time_ns()))
            if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
                raise ValueError(f'source changed concurrently: {name}')
        changed.append({'path': name, 'sha256': digest, 'old_mtime_ns': previous,
                        'new_mtime_ns': path.stat().st_mtime_ns})
    return {'applied': apply, 'files': changed, 'compiled_binary_verified': False}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--delta', type=Path, required=True)
    parser.add_argument('--apply', action='store_true')
    parser.add_argument('--receipt', type=Path)
    args = parser.parse_args()
    if args.receipt and args.receipt.exists():
        parser.error('receipt already exists; preserve previous evidence')
    delta = json.loads(args.delta.read_text(encoding='utf-8'))
    result = refresh(args.root, delta, apply=args.apply)
    output = json.dumps(result, indent=2) + '\n'
    if args.receipt:
        with args.receipt.open('x', encoding='utf-8') as handle:
            handle.write(output)
    print(output, end='')

if __name__ == '__main__':
    main()
