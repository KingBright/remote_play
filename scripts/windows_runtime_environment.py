"""Process-local Windows launch roles; never change persistent OS settings.

Receive acceptance temporarily disables the passive host. Its environment must
never be reused to restore the installed product's normal duplex role.
"""
from collections.abc import Mapping


def production_environment(inherited: Mapping[str, str], *, profile: str,
                           receive_directory: str | None = None) -> dict[str, str]:
    """Restore the product's default host/viewer role, including from a test parent."""
    result = dict(inherited)
    for name in tuple(result):
        if name.startswith(("REMOTE_PLAY_RESTORED_TEST_", "RP_MEDIA_ACCEPTANCE")):
            del result[name]
    result.update(REMOTE_PLAY_HEADLESS="0", REMOTE_PLAY_PASSIVE_HOST="1",
                  REMOTE_PLAY_DEVICE_GROUP_DIR=profile)
    if receive_directory is not None:
        result["REMOTE_PLAY_FILE_RECEIVE_DIR"] = receive_directory
    return result


def receive_test_environment(production: Mapping[str, str],
                             **test_settings: str) -> dict[str, str]:
    """Isolate the receive-only child; leave the restoration environment intact."""
    result = dict(production)
    result.update(test_settings)
    result["REMOTE_PLAY_PASSIVE_HOST"] = "0"
    return result
