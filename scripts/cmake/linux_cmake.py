#!/usr/bin/env python3
"""Apply target archive settings after project(), including an existing cache."""
import os
from pathlib import Path
import sys


def command(cmake, tools, arguments):
    if not Path(cmake).is_absolute() or not Path(tools).is_absolute():
        raise ValueError('Installed absolute CMake and fixed tool directory required')
    controls = ('--build', '--install', '--version', '--help', '-E', '-P', '--find-package')
    if any(arg in controls or arg.startswith('--help-') for arg in arguments):
        return [cmake, *arguments]
    # CMAKE_TOOLCHAIN_FILE alone is not re-read when CMakeSystem.cmake already
    # exists. project() also reloads old compiler archive tools. Override them
    # through the documented post-project hook before generation, without
    # deleting any cache or rewriting CMake's generated compiler records.
    return [cmake, *arguments, '-DCMAKE_PROJECT_INCLUDE:FILEPATH='+str(Path(tools)/'linux-zig-toolchain.cmake')]


if __name__ == '__main__':
    cmake = os.environ['WEAVELO_VALIDATION_CMAKE']
    if not Path(cmake).is_file():
        raise ValueError('Installed CMake required')
    args = command(cmake, str(Path(__file__).resolve().parent), sys.argv[1:])
    os.execv(args[0], args)
