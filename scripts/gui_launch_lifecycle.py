#!/usr/bin/env python3
"""Read-only launchd policy check; never loads, stops or edits a service.

This validates normal-exit behavior, not compiled product identity or independent
GUI/host architecture. Install/launch integration remains a separate change.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import plistlib


class LifecycleRejected(ValueError):
    pass


def validate(document: dict, executable: str, *, role: str = 'gui') -> dict:
    if role not in ('gui', 'host'):
        raise LifecycleRejected('Expected role must be gui or host')
    if not isinstance(document, dict):
        raise LifecycleRejected('Launch configuration must be a dictionary')
    program = document.get('Program')
    args = document.get('ProgramArguments')
    if args is not None and (not isinstance(args, list) or not args
                             or not all(isinstance(x, str) for x in args)):
        raise LifecycleRejected('Invalid executable arguments')
    if program is not None and program != executable:
        raise LifecycleRejected('Program differs from the expected executable')
    if args is not None and args[0] != executable:
        raise LifecycleRejected('ProgramArguments differs from the expected executable')
    if program is None and args is None:
        raise LifecycleRejected('Missing executable')
    environment = document.get('EnvironmentVariables', {})
    if not isinstance(environment, dict):
        raise LifecycleRejected('Invalid environment dictionary')
    value = environment.get('REMOTE_PLAY_HEADLESS', '0')
    if not isinstance(value, str):
        raise LifecycleRejected('Headless setting must be a string')
    headless = value in ('1', 'true', 'TRUE', 'yes', 'YES')
    if headless != (role == 'host'):
        raise LifecycleRejected('Launch role differs from REMOTE_PLAY_HEADLESS')
    keep_alive = document.get('KeepAlive', False)
    if type(keep_alive) is bool:
        restarts_normal_exit = keep_alive
    elif isinstance(keep_alive, dict) and keep_alive:
        # launchd ORs predicates. Restrict GUI policies to crash/nonzero-exit
        # predicates; path/network/other-job predicates can restart a closed GUI.
        allowed = {'SuccessfulExit': False, 'Crashed': True}
        if any(k not in allowed or type(v) is not bool or v != allowed[k]
               for k, v in keep_alive.items()):
            raise LifecycleRejected('KeepAlive predicates may restart a normally closed GUI')
        restarts_normal_exit = False
    else:
        raise LifecycleRejected('Invalid or unsupported KeepAlive value')
    if role == 'gui' and restarts_normal_exit:
        raise LifecycleRejected('Unconditional KeepAlive would reopen the GUI after normal exit')
    return {'role': role, 'executable': executable,
            'normal_gui_exit_restarts': role == 'gui' and restarts_normal_exit,
            'launch_policy_verified': True, 'service_modified': False,
            'independent_background_host': 'not_evaluated',
            'compiled_identity': 'not_evaluated'}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--plist', type=Path, required=True)
    parser.add_argument('--expected-executable', required=True)
    parser.add_argument('--role', choices=('gui', 'host'), default='gui')
    args = parser.parse_args()
    with args.plist.open('rb') as source:
        document = plistlib.load(source)
    print(json.dumps(validate(document, args.expected_executable, role=args.role), indent=2))


if __name__ == '__main__':
    main()
