#!/usr/bin/env python3
"""Link the fixed Linux target with the supplied Linux sysroot, never host libc."""
import os
from pathlib import Path
import sys


def command(compiler, sysroot, arguments):
    if not Path(compiler).is_absolute() or not Path(sysroot).is_absolute():
        raise ValueError('Absolute installed Clang and Linux sysroot required')
    if any(arg.startswith(('--target=', '-target=', '--sysroot=', '-isysroot')) or
           arg in ('--target', '-target', '--sysroot') for arg in arguments):
        raise ValueError('Caller target/sysroot override refused')
    # Fedora ships the runtime SONAME in libgcc. Do not fall back to a host
    # unversioned development link or synthesize a library/pkg-config file.
    arguments = ['-l:libgcc_s.so.1' if arg == '-lgcc_s' else arg for arg in arguments]
    return [compiler, '--target=x86_64-unknown-linux-gnu', '--sysroot='+sysroot,
            '-fuse-ld=lld', '-L'+str(Path(sysroot)/'usr/lib64'),
            '-Wl,-rpath-link,'+str(Path(sysroot)/'usr/lib64'),
            '-Wl,--no-allow-shlib-undefined', *arguments]


if __name__ == '__main__':
    compiler = os.environ['WEAVELO_VALIDATION_LINUX_CLANG']
    root = os.environ['WEAVELO_VALIDATION_SYSROOT']
    if not Path(compiler).is_file() or not Path(root).is_dir() or Path(root).is_symlink():
        raise ValueError('Installed Clang and real sysroot required')
    args = command(compiler, root, sys.argv[1:])
    os.execv(args[0], args)
