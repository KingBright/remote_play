#!/usr/bin/env python3
"""Check a returned native binary before normal GUI and signing gates execute."""
import argparse
import hashlib
from pathlib import Path
import re

def verified_path(binary: Path, expected: str) -> Path:
    if not binary.is_absolute() or binary.is_symlink() or not binary.is_file():
        raise ValueError('Prebuilt binary must be an existing absolute regular file, not a symlink')
    if not re.fullmatch(r'[0-9a-f]{64}', expected):
        raise ValueError('The exact lowercase SHA-256 from the verified build is required')
    with binary.open('rb') as source:
        digest = hashlib.file_digest(source, 'sha256').hexdigest()
    if digest != expected:
        raise ValueError('Prebuilt artifact differs from the verified build; signing refused')
    return binary.resolve(strict=True)

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--sha256', required=True)
    args = parser.parse_args()
    print(verified_path(args.binary, args.sha256))
