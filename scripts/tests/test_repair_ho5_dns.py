import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "repair_ho5_dns", Path(__file__).resolve().parents[1] / "repair_ho5_portal_transfer_dns.py")
repair = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(repair)


def fixture():
    return {"dns": {"servers": [{"tag": "dns-remote", "type": "tls", "detour": "proxy",
                                  "server": "fixture.invalid", "private": "never_print_this"}],
                     "rules": [{"domain_suffix": ["oaiusercontent.com"], "server": "dns-fakeip"}],
                     "final": "dns-local"},
            "outbounds": [{"type": "fixture", "password": "never_print_this_either"}],
            "route": {"rules": [{"action": "sniff"}]}}


@unittest.skipUnless(os.name == "posix", "HO5 repair uses Unix file and administrator interfaces")
class ExactDnsPlanTests(unittest.TestCase):
    def test_duplicate_json_fields_cannot_be_silently_discarded(self):
        with self.assertRaises(ValueError):
            json.loads('{"dns":{},"dns":{"rules":[]}}', object_pairs_hook=repair.unique_pairs)

    def test_only_one_semantic_insertion_and_input_not_mutated(self):
        original = fixture()
        snapshot = copy.deepcopy(original)
        updated = repair.patch_document(original)
        self.assertEqual(updated["dns"]["rules"].pop(0), repair.RULE)
        self.assertEqual(updated, snapshot)
        self.assertEqual(original, snapshot)

    def test_refuses_missing_changed_or_ambiguous_server(self):
        for servers in ([], [{"tag": "dns-remote", "type": "https", "detour": "proxy"}],
                        [{"tag": "dns-remote", "type": "tls", "detour": "direct"}],
                        fixture()["dns"]["servers"] * 2):
            document = fixture()
            document["dns"]["servers"] = servers
            with self.subTest(servers=servers), self.assertRaises(ValueError):
                repair.patch_document(document)

    def test_existing_exact_rule_and_unsupported_shape_fail_closed(self):
        for extra in (repair.RULE, {"domain": repair.DOMAIN}, "fixture"):
            document = fixture()
            document["dns"]["rules"].append(extra)
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                repair.patch_document(document)

    def test_preflight_hash_refusal_and_nonroot_apply_never_write(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "config.json"
            original = json.dumps(fixture()).encode()
            config.write_bytes(original)
            with patch.object(repair, "CONFIG", config), patch.object(repair, "EXPECTED_SHA256", repair.sha256(original)), \
                 patch.object(repair.os, "geteuid", return_value=501), patch.object(repair, "write_candidate") as writer:
                receipt = repair.execute()
                self.assertEqual(receipt["state"], "preflight_only")
                self.assertNotIn("never_print", json.dumps(receipt))
                self.assertEqual(config.read_bytes(), original)
                with self.assertRaises(PermissionError):
                    repair.execute(True)
                writer.assert_not_called()
            with patch.object(repair, "CONFIG", config):
                with self.assertRaises(ValueError):
                    repair.execute()

    def test_rejects_symlink_without_reading_target(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "target"
            target.write_bytes(b"fixture")
            link = Path(directory) / "link"
            link.symlink_to(target)
            with self.assertRaises(OSError):
                repair.read_regular(link)

    def test_failed_restart_restores_exact_original_and_keeps_private_backup(self):
        self.run_apply(fail_restart=True)

    def test_success_checks_before_replace_and_restarts_once(self):
        self.run_apply(fail_restart=False)

    def run_apply(self, fail_restart):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "config.json"
            backup = Path(directory) / "backup.json"
            original = json.dumps(fixture()).encode()
            config.write_bytes(original)
            real_read = repair.read_regular
            calls = []

            def read(path):
                data, info = real_read(path)
                fields = {field: getattr(info, field) for field in ("st_uid", "st_gid", "st_mode", "st_dev", "st_ino")}
                fields["st_uid"] = 0
                return data, SimpleNamespace(**fields)

            def candidate(data, info):
                p = Path(directory) / "candidate.json"
                p.write_bytes(data)
                return p

            def command(args, timeout=30):
                calls.append(args)
                if "check" in args:
                    self.assertEqual(config.read_bytes(), original)
                if "restart" in args and fail_restart and sum("restart" in c for c in calls) == 1:
                    raise RuntimeError("fixture restart failure")

            with patch.object(repair, "CONFIG", config), patch.object(repair, "BACKUP", backup), \
                 patch.object(repair, "EXPECTED_SHA256", hashlib.sha256(original).hexdigest()), \
                 patch.object(repair.os, "geteuid", return_value=0), patch.object(repair, "check_secure_parent"), \
                 patch.object(repair, "service_executable", return_value="/usr/bin/sing-box"), \
                 patch.object(repair, "read_regular", side_effect=read), \
                 patch.object(repair, "write_candidate", side_effect=candidate), \
                 patch.object(repair, "command_ok", side_effect=command), \
                 patch.object(repair, "verify_resolver", return_value={"all_public": True}):
                if fail_restart:
                    with self.assertRaises(repair.RepairFailure) as caught:
                        repair.execute(True)
                    self.assertTrue(caught.exception.rolled_back)
                    self.assertEqual(config.read_bytes(), original)
                    self.assertEqual(sum("restart" in c for c in calls), 2)
                else:
                    receipt = repair.execute(True)
                    self.assertEqual(receipt["state"], "applied")
                    self.assertEqual(sum("restart" in c for c in calls), 1)
                    self.assertEqual(json.loads(config.read_bytes())["dns"]["rules"][0], repair.RULE)
                self.assertEqual(backup.read_bytes(), original)
                self.assertEqual(backup.stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
