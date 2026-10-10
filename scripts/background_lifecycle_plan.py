#!/usr/bin/env python3
"""Pure launch-agent migration/rollback plan. This module never runs launchctl.

A real migration must first install the signed separated-lifecycle candidate
through the existing guarded installer, back up the original plist, stop its
old runtime, verify that the profile lock/ports are free, then start ONLY the
background service. No fixture receipt authorizes a real system migration.
"""
from __future__ import annotations
import copy
import hashlib
import plistlib


class LifecyclePlanRejected(ValueError):
    pass


def plan(document: dict, executable: str, product_info: dict) -> dict:
    if (document.get('Label') != 'com.remoteplay.host'
            or document.get('ProgramArguments') not in
            ([executable], [executable, '--background-service'])
            or document.get('Program', executable) != executable):
        raise LifecyclePlanRejected('Unexpected canonical launch-agent executable/arguments')
    if (product_info.get('separated_lifecycle') is not True
            or product_info.get('background_entry') != '--background-service'
            or product_info.get('frontend_entry') != '--gui'
            or product_info.get('default_gui') != 'restored-original-gpui'
            or product_info.get('original_gui_compiled') is not True):
        raise LifecyclePlanRejected('Candidate has no explicit background service and attached GPUI entry')
    environment = document.get('EnvironmentVariables', {})
    if not isinstance(environment, dict) or not environment.get('REMOTE_PLAY_DEVICE_GROUP_DIR'):
        raise LifecyclePlanRejected('Preserve the explicitly bound existing device profile')
    after = copy.deepcopy(document)
    after['ProgramArguments'] = [executable, '--background-service']
    after['KeepAlive'] = True
    after['RunAtLoad'] = True
    after['EnvironmentVariables'] = dict(environment, REMOTE_PLAY_HEADLESS='1')
    before_bytes = plistlib.dumps(document, sort_keys=False)
    after_bytes = plistlib.dumps(after, sort_keys=False)
    return {'before': copy.deepcopy(document), 'after': after,
            'before_normalized_sha256': hashlib.sha256(before_bytes).hexdigest(),
            'after_normalized_sha256': hashlib.sha256(after_bytes).hexdigest(),
            'identity_or_permission_change': False, 'applied': False,
            'manual_gui_arguments': [executable, '--gui'],
            'rollback_requires_app_and_original_plist': True,
            'real_launchd_login_reboot_acceptance': 'pending'}


def rollback_document(migration: dict, current: dict) -> dict:
    # Refuse overwriting a launch-agent subsequently edited by another task.
    if current != migration['after']:
        raise LifecyclePlanRejected('Launch agent changed after the recorded migration')
    return copy.deepcopy(migration['before'])
