#!/usr/bin/env python3
"""Fixed Linux compiler entry for CMake/cc-rs; no shell or host-library fallback."""
import os
from pathlib import Path
import sys


def command(tool, mode, arguments):
    if mode not in ('cc', 'c++', 'ar') or not Path(tool).is_absolute():
        raise ValueError('Verified absolute Zig path and fixed compiler mode required')
    normalized=[]; index=0
    while index < len(arguments):
        arg=arguments[index]
        if arg in ('-target','--target'):
            index+=1
            if index >= len(arguments):raise ValueError('Missing fixed target')
            arg='--target='+arguments[index]
        if arg.startswith('--target='):
            if mode=='ar' or arg.split('=',1)[1] not in ('x86_64-unknown-linux-gnu','x86_64-linux-gnu'):
                raise ValueError('Foreign target refused')
            arg='--target=x86_64-linux-gnu'
        normalized.append(arg); index+=1
    return [tool,mode,*([] if mode=='ar' else ['-target','x86_64-linux-gnu']),*normalized]


if __name__=='__main__':
    argv=command(os.environ['WEAVELO_VALIDATION_ZIG'],sys.argv[1],sys.argv[2:])
    os.execv(argv[0],argv)
