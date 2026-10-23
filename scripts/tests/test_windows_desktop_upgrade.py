import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import upgrade_windows_desktop as upgrade


class UpgradeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.p = upgrade.transaction_paths(self.home)
        for name in ('app', 'profile', 'state'):
            self.p[name].mkdir(parents=True)
        (self.p['app']/'weavelo.exe').write_bytes(b'old-image')
        (self.p['profile']/'mesh.conf').write_bytes(b'fixture-membership')
        (self.p['profile']/'mesh.secret').write_bytes(b'fixture-only-secret')
        self.old = upgrade.gate.hash_file(self.p['app']/'weavelo.exe')
        self.candidate = self.home/'candidate'
        self.candidate.mkdir()
        (self.candidate/'weavelo.exe').write_bytes(b'new-image')
        self.receipt = self.home/'receipt.json'
        self.receipt.write_text(json.dumps({'build_identity': {'fixture': True}}))
        self.bound = {'candidate': self.candidate, 'receipt': self.receipt,
            'receipt_sha256': upgrade.gate.hash_file(self.receipt),
            'commit': 'b'*40, 'binary_sha256': upgrade.gate.hash_file(self.candidate/'weavelo.exe'),
            'identity_sha256': 'd'*64, 'archive_sha256': 'e'*64,
            'files': upgrade.original.inventory(self.candidate, upgrade.gate)}
        self.registry = {'schema': 1, 'kind': 'independent-weavelo-windows-desktop-install',
            'paths': {k: str(v) for k, v in upgrade.install.paths(self.home).items()},
            'runtime_commit': 'a'*40, 'identity_sha256': 'c'*64, 'binary_sha256': self.old,
            'archive_sha256': 'f'*64, 'payload': upgrade.original.inventory(self.p['app'], upgrade.gate),
            'applied': True, 'phase': 'committed_not_started', 'user_notes': {'keep': 'user text'}}
        (self.p['state']/'install.json').write_text(json.dumps(self.registry))
        self.process = {'pid': 9, 'path': os.path.normcase(str(self.p['app']/'weavelo.exe')), 'created': 7}
        self.calls = []
        patches = [
            patch.object(upgrade.install, 'status', side_effect=lambda home: json.loads((self.p['state']/'install.json').read_text())),
            patch.object(upgrade.install, 'revalidate', side_effect=lambda bound: bound['files']),
            patch.object(upgrade, 'process_metadata', side_effect=lambda pid: dict(self.process)),
            patch.object(upgrade.subprocess, 'check_output', return_value=json.dumps({'build_identity': {'fixture': True}})),
            patch.object(upgrade.gate, 'validate_identity', return_value={'fixture': True}),
            patch.object(upgrade.gate, 'product_info', return_value={'build_identity': {'fixture': True}}),
            patch.object(upgrade.gate, 'verify_running_windows', return_value={'verified': True}),
            patch.object(upgrade.install, 'move_no_replace', side_effect=self.move),
        ]
        for item in patches:
            item.start(); self.addCleanup(item.stop)

    @staticmethod
    def move(source, target):
        if target.exists():
            raise FileExistsError(target)
        source.rename(target)

    def reviewed(self):
        return upgrade.plan(self.home, self.bound, self.old, 'c'*64, 9, 7)

    def stop(self, *args):
        self.calls.append('stop')

    def launch(self, *args):
        self.calls.append('start')
        return SimpleNamespace(pid=11)

    def accept(self, *args):
        return {'pid': 11, 'status': {'executable_sha256': self.bound['binary_sha256']},
                'pairing': {'accepted': True, 'network_name_sha256': '1'*64}}

    def apply(self, digest, stopper=None):
        security = SimpleNamespace(create_private_directory=lambda p: p.mkdir())
        with patch.object(upgrade.sys, 'platform', 'win32'):
            return upgrade.apply(self.home, self.bound, self.old, 'c'*64, 9, 7, digest,
                security=security, stopper=stopper or self.stop, starter=self.launch, reader=self.accept)

    def test_upgrade_retains_old_image_membership_and_user_fields(self):
        before = upgrade.membership_metadata(self.p['profile'])
        _, digest = self.reviewed()
        result = self.apply(digest)
        self.assertEqual(result['phase'], 'completed')
        self.assertEqual(self.calls, ['stop', 'start'])
        self.assertEqual((self.p['app']/'weavelo.exe').read_bytes(), b'new-image')
        self.assertEqual((self.p['rollback']/'weavelo.exe').read_bytes(), b'old-image')
        self.assertEqual(upgrade.membership_metadata(self.p['profile']), before)
        registry = json.loads((self.p['state']/'install.json').read_text())
        self.assertEqual(registry['user_notes'], {'keep': 'user text'})
        self.assertEqual(registry['runtime_commit'], 'b'*40)
        self.assertEqual(registry['upgrade_history'][0]['runtime_commit'], 'a'*40)
        self.assertFalse(result['membership_contents_read'])

    def test_wrong_plan_never_stops_or_creates_transaction(self):
        with self.assertRaises(Exception):
            self.apply('0'*64)
        self.assertFalse(self.calls)
        self.assertFalse(self.p['journal'].exists())

    def test_candidate_tampering_never_stops(self):
        _, digest = self.reviewed()
        (self.candidate/'weavelo.exe').write_bytes(b'altered')
        with self.assertRaises(Exception):
            self.apply(digest)
        self.assertFalse(self.calls)
        self.assertEqual((self.p['app']/'weavelo.exe').read_bytes(), b'old-image')

    def test_change_during_stop_keeps_original_app_and_recovery_state(self):
        _, digest = self.reviewed()
        def stop(*args):
            self.calls.append('stop')
            (self.p['pending']/'weavelo.exe').write_bytes(b'changed-during-stop')
        with self.assertRaises(Exception):
            self.apply(digest, stopper=stop)
        self.assertEqual(self.calls, ['stop'])
        self.assertEqual((self.p['app']/'weavelo.exe').read_bytes(), b'old-image')
        self.assertEqual(json.loads(self.p['journal'].read_text())['phase'], 'stopped')

    def test_existing_transaction_and_replaced_pid_fail_closed(self):
        self.p['journal'].write_text('existing unknown transaction')
        with self.assertRaises(Exception):
            self.reviewed()
        self.p['journal'].unlink()
        self.process['created'] = 8
        with self.assertRaises(Exception):
            self.reviewed()
        self.assertFalse(self.calls)

    def test_registry_cas_preserves_concurrent_user_edit(self):
        path = self.p['state']/'install.json'
        digest = upgrade.gate.hash_file(path)
        changed = dict(self.registry, user_notes={'new': 'concurrent user edit'})
        path.write_text(json.dumps(changed))
        candidate = {'commit': 'b'*40, 'identity_sha256': 'd'*64,
            'binary_sha256': 'e'*64, 'archive_sha256': 'f'*64,
            'receipt_sha256': '1'*64, 'payload': {}}
        with self.assertRaises(Exception):
            upgrade.commit_registry(path, digest, self.registry, candidate, {'pid': 11})
        self.assertEqual(json.loads(path.read_text()), changed)

    def test_membership_observation_never_reads_contents(self):
        with patch.object(Path, 'open', side_effect=AssertionError('credential contents read')):
            metadata = upgrade.membership_metadata(self.p['profile'])
        self.assertEqual(set(metadata), {'mesh.conf', 'mesh.secret'})


if __name__ == '__main__':
    unittest.main()
