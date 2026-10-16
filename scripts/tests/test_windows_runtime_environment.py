"""A receive test must not leave the installed product unable to accept sessions."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from windows_runtime_environment import production_environment, receive_test_environment


class WindowsRuntimeEnvironmentTests(unittest.TestCase):
    def test_restoration_overrides_inherited_receive_only_role(self):
        parent = {"REMOTE_PLAY_PASSIVE_HOST": "0", "PATH": "existing"}
        restored = production_environment(parent, profile="existing-paired-profile")
        self.assertEqual(restored["REMOTE_PLAY_PASSIVE_HOST"], "1")
        self.assertEqual(parent["REMOTE_PLAY_PASSIVE_HOST"], "0")
        self.assertEqual(restored["PATH"], "existing")

    def test_receive_child_cannot_contaminate_success_or_failure_restoration(self):
        formal = production_environment({}, profile="existing-paired-profile")
        for failed in (False, True):
            child = receive_test_environment(formal, REMOTE_PLAY_RESTORED_TEST_SECONDS="160")
            try:
                child["REMOTE_PLAY_DEVICE_GROUP_DIR"] = "test-local-change"
                if failed:
                    raise RuntimeError("viewer exited before producing a result")
            except RuntimeError:
                pass
            finally:
                self.assertEqual(formal["REMOTE_PLAY_PASSIVE_HOST"], "1")
                self.assertEqual(formal["REMOTE_PLAY_DEVICE_GROUP_DIR"], "existing-paired-profile")
                self.assertNotIn("REMOTE_PLAY_RESTORED_TEST_SECONDS", formal)

    def test_restored_product_does_not_reenter_inherited_acceptance_mode(self):
        parent = {"REMOTE_PLAY_RESTORED_TEST_DEVICE": "old-peer",
                  "REMOTE_PLAY_RESTORED_TEST_OUTPUT": "old-result",
                  "RP_MEDIA_ACCEPTANCE": "1", "RP_MEDIA_ACCEPTANCE_EXTRA": "stale"}
        restored = production_environment(parent, profile="existing-paired-profile")
        self.assertEqual(set(restored), {"REMOTE_PLAY_HEADLESS", "REMOTE_PLAY_PASSIVE_HOST",
                                       "REMOTE_PLAY_DEVICE_GROUP_DIR"})
        self.assertEqual(len(parent), 4)

    def test_roles_preserve_existing_identity_paths_and_security_settings(self):
        parent = {"PSExecutionPolicyPreference": "Restricted",
                  "REMOTE_PLAY_SESSION_PSK": "test-sentinel-only",
                  "REMOTE_PLAY_HOST_BIND_ADDR": "127.0.0.1:38117",
                  "REMOTE_PLAY_FILE_RECEIVE_DIR": "existing-downloads"}
        formal = production_environment(parent, profile="existing-paired-profile")
        child = receive_test_environment(formal, REMOTE_PLAY_RESTORED_TEST_OUTPUT="new-result")
        for name, value in parent.items():
            self.assertEqual(formal[name], value)
            self.assertEqual(child[name], value)
        chosen = production_environment(parent, profile="existing-paired-profile",
                                        receive_directory="chosen-downloads")
        self.assertEqual(chosen["REMOTE_PLAY_FILE_RECEIVE_DIR"], "chosen-downloads")
        self.assertEqual(parent["REMOTE_PLAY_FILE_RECEIVE_DIR"], "existing-downloads")

    def test_receive_role_cannot_accidentally_enable_the_child_host(self):
        child = receive_test_environment({}, REMOTE_PLAY_PASSIVE_HOST="1")
        self.assertEqual(child["REMOTE_PLAY_PASSIVE_HOST"], "0")


if __name__ == "__main__":
    unittest.main()
