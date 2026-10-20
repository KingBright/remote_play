"""Owned filesystem and failure-boundary tests; no product/service is started."""
import copy
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import linux_current_update as update


class Fixture(update.LinuxUpdate):
    def __init__(self, root):
        self.work = root
        self.live = root/'live'; self.live.mkdir()
        self.exe = self.live/'weavelo'; self.exe.write_bytes(b'old')
        self.payload = root/'payload'; self.payload.mkdir()
        self.stage = self.payload/'Weavelo'; self.stage.mkdir()
        (self.stage/'weavelo').write_bytes(b'new')
        self.previous = root/'previous'
        self.manifest = self.payload/'manifest.json'; self.manifest.write_text('{}')
        self.journal = root/'journal.json'
        receipt = root/'receipt.json'; receipt.write_text('{}')
        self.config = {'unchanged': True}
        self.profile = {'unchanged': True}
        self.pid = 99999999
        self.events = []
        self.fail_new = False
        self.tamper_rollback = False
        self.plan = {'old_app': update.tree(self.live), 'candidate_app': update.tree(self.stage),
                     'old_binary_sha256': update.gate.hash_file(self.exe),
                     'binary_sha256': update.gate.hash_file(self.stage/'weavelo'),
                     'old_owner': self.owner(None), 'receipt': str(receipt),
                     'receipt_sha256': update.gate.hash_file(receipt), 'manifest_sha256': 'a'*64,
                     'source_commit': 'b'*40, 'identity_sha256': 'c'*64,
                     'profile': copy.deepcopy(self.profile), 'configuration': copy.deepcopy(self.config)}
        self.expected = update.digest(self.plan)
        self.root = {'untouched_original': {'value': 1},
                     'linux_update': {'phase': 'prepared', 'plan': self.plan, 'plan_sha256': self.expected}}

    def owner(self, unused):
        return {'pid': self.pid, 'generation': 'generation'}

    def check(self, document, expected):
        update.require(expected == document['plan_sha256'] == update.digest(document['plan']), 'Plan drift')
        update.require(self.profile == document['plan']['profile'], 'Profile drift')
        update.require(self.config == document['plan']['configuration'], 'Service drift')
        return document['plan']

    def service(self):
        return {'MainPID': str(self.pid)}

    def save(self, document):
        self.root['linux_update'] = document

    def ctl(self, action):
        self.events.append(action)
        self.pid = 0 if action == 'stop' else 99999998
        if action == 'start' and self.exe.read_bytes() == b'new' and self.fail_new:
            if self.tamper_rollback:
                (self.previous/'weavelo').write_bytes(b'external')
            raise RuntimeError('Synthetic startup failure')

    def exchange(self, first, second):
        self.events.append('exchange')
        temporary = first.with_name('owned-exchange')
        first.rename(temporary); second.rename(first); temporary.rename(second)

    def wait_owner(self, expected):
        update.require(update.gate.hash_file(self.exe) == expected, 'Wrong loaded fixture')
        return {'pid': self.pid, 'verified_fixture': True}


class UpdateTests(unittest.TestCase):
    def setUp(self):
        if not hasattr(os, 'listxattr'):
            metadata = patch.object(os, 'listxattr', return_value=[], create=True)
            metadata.start(); self.addCleanup(metadata.stop)
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.adapter = Fixture(self.root)
        self.verify = patch.object(update.gate, 'verify_install', return_value={'verified': True})
        self.verify.start(); self.addCleanup(self.verify.stop)
        self.native = patch.object(update.gate, 'verify_running_linux', return_value={'verified': True})
        self.native.start(); self.addCleanup(self.native.stop)

    def test_success_retains_exact_previous_inode_and_original_journal(self):
        a = self.adapter
        before = update.tree(a.live)
        result = a.apply(a.expected)
        self.assertEqual(result['phase'], 'committed')
        self.assertEqual(a.exe.read_bytes(), b'new')
        self.assertEqual(update.tree(a.previous), before)
        self.assertEqual(a.root['untouched_original'], {'value': 1})
        self.assertEqual(a.events, ['stop', 'exchange', 'start'])

    def test_candidate_mutation_is_rejected_before_stop(self):
        a = self.adapter
        (a.stage/'weavelo').write_bytes(b'foreign')
        with self.assertRaises(RuntimeError):
            a.apply(a.expected)
        self.assertEqual(a.events, [])
        self.assertEqual(a.exe.read_bytes(), b'old')

    def test_same_bytes_on_new_inode_are_not_owned_staging(self):
        a = self.adapter
        path = a.stage/'weavelo'
        original = path.with_name('other')
        path.rename(original); path.write_bytes(b'new'); original.unlink()
        with self.assertRaises(RuntimeError):
            a.apply(a.expected)
        self.assertEqual(a.events, [])

    def test_profile_or_unit_drift_blocks_before_stop(self):
        for field in ('profile', 'config'):
            with self.subTest(field=field):
                a = self.adapter
                old = getattr(a, field)
                setattr(a, field, {'changed': True})
                with self.assertRaises(RuntimeError):
                    a.apply(a.expected)
                setattr(a, field, old)
                self.assertEqual(a.events, [])

    def test_wrong_plan_and_unknown_phase_never_replay(self):
        a = self.adapter
        with self.assertRaises(RuntimeError):
            a.apply('f'*64)
        a.root['linux_update']['phase'] = 'start_requested'
        with self.assertRaises(RuntimeError):
            a.apply(a.expected)
        self.assertEqual(a.events, [])

    def test_failed_new_start_restores_previous_inode_and_owner(self):
        a = self.adapter; a.fail_new = True
        before = update.tree(a.live)
        with self.assertRaisesRegex(RuntimeError, 'original image restored'):
            a.apply(a.expected)
        self.assertEqual(update.tree(a.live), before)
        self.assertEqual(a.root['linux_update']['phase'], 'rolled_back')
        self.assertIn('rollback_owner', a.root['linux_update'])
        self.assertEqual(a.events, ['stop', 'exchange', 'start', 'stop', 'exchange', 'start'])

    def test_foreign_rollback_bytes_are_preserved_and_report_recovery_required(self):
        a = self.adapter; a.fail_new = True; a.tamper_rollback = True
        with self.assertRaisesRegex(RuntimeError, 'original journal inspection'):
            a.apply(a.expected)
        self.assertEqual((a.previous/'weavelo').read_bytes(), b'external')
        self.assertEqual(a.root['linux_update']['phase'], 'recovery_required')

    def test_explicit_recovery_of_pre_exchange_stop_does_not_exchange(self):
        a = self.adapter; a.pid = 0
        document = a.root['linux_update']; document['phase'] = 'stop_requested'
        result = a.rollback(document, a.expected)
        self.assertEqual(result['phase'], 'rolled_back')
        self.assertEqual(a.events, ['start'])
        self.assertEqual(a.exe.read_bytes(), b'old')

    def test_recovery_recognizes_original_stage_after_exchange_before_rename(self):
        a = self.adapter; a.pid = 0
        a.exchange(a.live, a.stage); a.events.clear()
        document = a.root['linux_update']; document['phase'] = 'exchange_requested'
        result = a.rollback(document, a.expected)
        self.assertEqual(result['phase'], 'rolled_back')
        self.assertEqual(a.exe.read_bytes(), b'old')
        self.assertEqual(a.events, ['exchange', 'start'])

    def test_completed_update_is_not_reapplied(self):
        a = self.adapter
        a.apply(a.expected); a.events.clear()
        with self.assertRaises(RuntimeError):
            a.apply(a.expected)
        self.assertEqual(a.events, [])

    def test_active_lease_pending_and_missing_sessions_fail_closed(self):
        good = {'ok': True, 'renderer': update.gate.RENDERER, 'generation': 'g',
                'lease': None, 'sessions': {'sessions': [], 'pending': 0}}
        self.assertEqual(update.require_idle(good), 'g')
        for change in ({'lease': {'active': True}}, {'sessions': {'sessions': [], 'pending': 1}},
                       {'sessions': {'sessions': [{}], 'pending': 0}}, {'sessions': {}},
                       {'renderer': 'diagnostic'}, {'generation': None}):
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                update.require_idle(dict(good, **change))

    def test_external_journal_edit_is_not_overwritten(self):
        a = self.adapter
        a.journal.write_text('{}'); a.journal_state = update.state(a.journal)
        a.journal.write_text('{"external":true}')
        with self.assertRaisesRegex(RuntimeError, 'changed externally'):
            update.LinuxUpdate.save(a, {'phase': 'prepared'})
        self.assertEqual(json.loads(a.journal.read_text()), {'external': True})

    def test_service_requests_empty_policy_fields_and_refuses_missing_observations(self):
        keys = ['MainPID', 'ActiveState', 'SubState', 'UnitFileState', 'FragmentPath',
                'DropInPaths', 'Job', 'TriggeredBy', 'Restart']
        with patch.object(update.subprocess, 'check_output',
                          side_effect=[''.join(f'{key}=\n' for key in keys),
                                       'a(sasbttttuii) 0\n', 'a(sasbttttuii) 0\n']) as read:
            result = update.LinuxUpdate.service(self.adapter)
            self.assertIn('--all', read.call_args_list[0].args[0])
            self.assertEqual(result['ExecStop'], '')
            self.assertEqual(read.call_args_list[1].args[0][0], 'busctl')
        with patch.object(update.subprocess, 'check_output', return_value='MainPID=1\n'):
            with self.assertRaisesRegex(RuntimeError, 'Incomplete'):
                update.LinuxUpdate.service(self.adapter)
        for value in ('a(sasbttttuii) 1', '', 'unknown 0'):
            with patch.object(update.subprocess, 'check_output',
                              side_effect=[''.join(f'{key}=\n' for key in keys), value]):
                with self.assertRaisesRegex(RuntimeError, 'stop hook'):
                    update.LinuxUpdate.service(self.adapter)

    def test_symlink_and_hardlink_are_not_update_payloads(self):
        source = self.root/'source'; source.write_bytes(b'x')
        linked = self.root/'linked'; linked.symlink_to(source)
        with self.assertRaises(RuntimeError):
            update.state(linked)
        hard = self.root/'hard'; os.link(source, hard)
        with self.assertRaises(RuntimeError):
            update.state(hard)

    def test_reviewed_resource_change_cannot_accept_service_or_profile_policy(self):
        path = Path('/run/user/1000/systemd/user.control/weavelo.service.d/50-CPUWeight.conf')
        raw = '# runtime resource manager\n[Service]\nCPUWeight=10000\n'
        before = {'unit': {'inode': 1}, 'enabled': 'enabled', 'dropins': {str(path): 'a'*64}}
        after = copy.deepcopy(before); after['dropins'][str(path)] = update.gate.digest(raw.encode())
        result = update.resource_drift(before, after, update.digest(after), path, raw)
        self.assertFalse(result['resource_setting_modified_by_installer'])
        for altered in (dict(after, unit={'inode': 2}), dict(after, enabled='disabled'),
                        dict(after, dropins={**after['dropins'], '/other': 'b'*64})):
            with self.subTest(altered=altered), self.assertRaises(RuntimeError):
                update.resource_drift(before, altered, update.digest(altered), path, raw)
        with self.assertRaises(RuntimeError):
            update.resource_drift(before, after, 'f'*64, path, raw)
        for body in ('[Service]\nCPUWeight=10000\nExecStart=/other\n',
                     '[Service]\nCPUWeight=0\n', '[Service]\nCPUWeight=invalid\n'):
            changed = copy.deepcopy(after)
            changed['dropins'][str(path)] = update.gate.digest(body.encode())
            with self.subTest(body=body), self.assertRaises(RuntimeError):
                update.resource_drift(before, changed, update.digest(changed), path, body)

    def test_reconcile_already_verified_owner_never_replays_service_or_exchange(self):
        a = self.adapter
        a.apply(a.expected); a.events.clear()
        original = a.work/'installed-manifest.json'; original.write_text('{}')
        a.repo = Path(update.__file__).resolve().parents[1]
        path = Path(f'/run/user/{os.getuid()}/systemd/user.control/weavelo.service.d/50-CPUWeight.conf')
        raw = '[Service]\nCPUWeight=10000\n'
        before = {'unit': {'inode': 1}, 'enabled': 'enabled', 'dropins': {str(path): 'a'*64}}
        current = copy.deepcopy(before)
        current['dropins'][str(path)] = update.gate.digest(raw.encode())
        a.plan['configuration'] = before
        a.plan['old_manifest'] = update.state(original)
        a.profile_state = lambda: a.profile
        a.configuration = lambda: current
        document = a.root['linux_update']
        document['phase'] = 'recovery_required'
        a.expected = document['plan_sha256'] = update.digest(a.plan)
        with patch.object(Path, 'read_text', return_value=raw):
            result = a.reconcile(a.expected, update.digest(current))
        self.assertEqual(result['phase'], 'committed')
        self.assertTrue(result['external_resource_change_reviewed'])
        self.assertEqual(a.events, [])
        self.assertEqual(a.root['untouched_original'], {'value': 1})
        with self.assertRaises(RuntimeError):
            a.reconcile(a.expected, update.digest(current))

    @unittest.skipUnless(sys.platform.startswith('linux'), 'Native Linux renameat2')
    def test_native_exchange_preserves_both_recorded_directory_inodes(self):
        a = self.adapter
        old, new = update.tree(a.live), update.tree(a.stage)
        update.LinuxUpdate.exchange(a, a.live, a.stage)
        self.assertEqual(update.tree(a.live), new)
        self.assertEqual(update.tree(a.stage), old)


if __name__ == '__main__':
    unittest.main()
