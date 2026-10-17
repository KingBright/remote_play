import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import linux_background_lifecycle_plan as m


class LinuxSeparationPlanTests(unittest.TestCase):
    def setUp(self):
        self.home = '/home/example'
        self.exe = self.home + '/.local/opt/weavelo/weavelo'
        self.profile = self.home + '/.config/weavelo/mesh'
        self.unit = ('# preserved comment\n[Unit]\nDescription=Weavelo user desktop\n'
            'After=graphical-session.target\n\n[Service]\nType=simple\nExecStart=' + self.exe +
            '\nRestart=on-failure\nRestartSec=3\nEnvironment="DISPLAY=:0"\n'
            'Environment="WAYLAND_DISPLAY=wayland-0"\nEnvironment="WEAVELO_DEVICE_GROUP_DIR=' +
            self.profile + '"\nEnvironment="WEAVELO_HEADLESS=0"\nEnvironment="WEAVELO_PASSIVE_HOST=0"\n'
            '\n[Install]\nWantedBy=default.target\n').encode()
        self.facts = {'home': self.home, 'binary_sha256': 'b'*64, 'source_commit': 'a'*40,
            'product_info': {'separated_lifecycle': True, 'background_entry': '--background-service',
                'frontend_entry': '--gui', 'default_gui': 'restored-original-gpui', 'original_gui_compiled': True},
            'profile_state': {'directory': {'device': 2, 'inode': 3, 'uid': 1000, 'mode': 0o700},
                'files': {name: {'device': 2, 'inode': inode, 'uid': 1000, 'mode': 0o600,
                    'links': 1, 'sha256': letter*64} for name, inode, letter in
                    [('mesh.conf', 4, 'c'), ('mesh.secret', 5, 'd')]}},
            'unit_state': {'device': 2, 'inode': 77, 'uid': 1000, 'mode': 0o644,
                           'links': 1, 'sha256': m.sha(self.unit)},
            'observation': {'owner_mode': 'Standalone', 'pid': 41, 'generation': '41-fixture',
                'binary_sha256': 'b'*64, 'active_sessions': 0, 'pending_sessions': 0,
                'active_lease': False, 'drop_ins': [], 'unit_active': True,
                'unit_enabled': True, 'marker_present': False}}

    def proposal(self):
        return m.plan(self.unit, **self.facts)

    def test_exact_two_unit_changes_preserve_same_profile_and_original_bytes(self):
        before = copy.deepcopy(self.facts)
        result = self.proposal()
        expected = self.unit.replace(('ExecStart=' + self.exe + '\n').encode(),
            ('ExecStart=' + self.exe + ' --background-service\n').encode()).replace(
                b'WEAVELO_HEADLESS=0', b'WEAVELO_HEADLESS=1')
        self.assertEqual(result['unit_after_utf8'].encode(), expected)
        self.assertEqual(result['unit_before_sha256'], m.sha(self.unit))
        self.assertEqual(result['unit_after_sha256'], m.sha(expected))
        self.assertEqual(self.facts, before)
        self.assertEqual(result['profile_state'], before['profile_state'])
        self.assertFalse(result['credentials_created_or_imported'])
        self.assertFalse(result['profile_permissions_modified'])
        self.assertFalse(result['applied'])
        self.assertFalse(result['desktop_entry_modified'])
        self.assertEqual(result['manual_gui_arguments'], [self.exe, '--gui'])

    def test_disabled_startup_intent_never_silently_enabled(self):
        self.facts['observation']['unit_enabled'] = False
        self.assertFalse(self.proposal()['unit_enabled_after'])

    def test_unknown_observation_fields_never_exported(self):
        self.facts['observation']['unrelated_private_data'] = 'never-export-this-fixture-value'
        self.assertNotIn('never-export', json.dumps(self.proposal()))

    def test_capabilities_are_required_without_becoming_identity_proof(self):
        self.assertIn('required_at_apply', self.proposal()['native_product_identity_verification'])
        for field, value in [('separated_lifecycle', False), ('background_entry', None),
                             ('frontend_entry', '--unknown'), ('original_gui_compiled', False)]:
            facts = copy.deepcopy(self.facts); facts['product_info'][field] = value
            with self.subTest(field=field), self.assertRaises(m.LifecyclePlanRejected):
                m.plan(self.unit, **facts)

    def test_busy_pending_unknown_or_noncanonical_owner_never_planned(self):
        for field, value in [('active_sessions', 1), ('pending_sessions', 1), ('active_sessions', False),
                ('active_lease', True), ('pid', 0), ('owner_mode', 'Background'), ('generation', ''),
                ('marker_present', True), ('drop_ins', ['/etc/foreign.conf']), ('drop_ins', None),
                ('unit_active', False), ('unit_enabled', None), ('binary_sha256', 'e'*64)]:
            facts = copy.deepcopy(self.facts); facts['observation'][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(m.LifecyclePlanRejected):
                m.plan(self.unit, **facts)

    def test_drift_and_unsafe_unit_semantics_preserved_by_refusal(self):
        for replacement in [b'ExecStart=/other/app', b'ExecStart=-'+self.exe.encode(),
                b'ExecStart='+self.exe.encode()+b' --gui', b'ExecStart=',
                b'ExecStart='+self.exe.encode()+b'\nExecStart=/other/app']:
            raw = self.unit.replace(('ExecStart='+self.exe).encode(), replacement)
            with self.subTest(replacement=replacement), self.assertRaises(m.LifecyclePlanRejected):
                m.plan(raw, **dict(self.facts, unit_state=dict(self.facts['unit_state'], sha256=m.sha(raw))))
        for extra in [b'User=root\n', b'EnvironmentFile=/some/file\n', b'ExecStartPre=/bin/true\n',
                      b'Environment=\n', b'Environment="WEAVELO_HEADLESS=0"\n',
                      b'Environment="UNREVIEWED_TOKEN=placeholder"\n', b'[Service]\n']:
            raw = self.unit.replace(b'\n[Install]', b'\n'+extra+b'[Install]')
            with self.subTest(extra=extra), self.assertRaises(m.LifecyclePlanRejected):
                m.plan(raw, **dict(self.facts, unit_state=dict(self.facts['unit_state'], sha256=m.sha(raw))))

    def test_profile_rebinding_escape_and_privacy_drift_rejected(self):
        raw = self.unit.replace(self.profile.encode(), b'/other/profile')
        with self.assertRaises(m.LifecyclePlanRejected):
            m.plan(raw, **dict(self.facts, unit_state=dict(self.facts['unit_state'], sha256=m.sha(raw))))
        for home in ['/home/../other', '/home/a b', '/home/$user', '/home/%h', '/home/example/']:
            facts = copy.deepcopy(self.facts); facts['home'] = home
            with self.subTest(home=home), self.assertRaises(m.LifecyclePlanRejected):
                m.plan(self.unit, **facts)
        for field, value in [('uid', 0), ('mode', 0o644), ('links', 2), ('inode', 0), ('sha256', 'bad')]:
            facts = copy.deepcopy(self.facts); facts['profile_state']['files']['mesh.secret'][field] = value
            with self.subTest(field=field), self.assertRaises(m.LifecyclePlanRejected):
                m.plan(self.unit, **facts)

    def rollback_args(self):
        proposal = self.proposal()
        marker = {'path': self.profile+'/'+m.MARKER, 'device': 2, 'inode': 88,
                  'uid': 1000, 'mode': 0o600, 'size': 0, 'links': 1, 'sha256': m.sha(b'')}
        return proposal, {'expected_plan_sha256': proposal['plan_sha256'], 'original_unit': self.unit,
            'current_unit': proposal['unit_after_utf8'].encode(),
            'current_profile_state': copy.deepcopy(self.facts['profile_state']),
            'owned_unit': dict(self.facts['unit_state'], inode=78, sha256=proposal['unit_after_sha256']),
            'current_unit_state': dict(self.facts['unit_state'], inode=78, sha256=proposal['unit_after_sha256']),
            'owned_marker': marker, 'current_marker': copy.deepcopy(marker),
            'background_stopped': True, 'attached_gui_pids': []}

    def test_rollback_restores_original_role_only_after_owned_marker_proof(self):
        proposal, args = self.rollback_args(); before = copy.deepcopy(args)
        result = m.rollback_preview(proposal, **args)
        self.assertEqual(result['unit_restore_utf8'].encode(), self.unit)
        self.assertEqual(result['restart_arguments'], [self.exe])
        self.assertEqual(result['marker_remove_only_if_unchanged']['inode'], 88)
        self.assertEqual(args, before)
        self.assertFalse(result['applied'])

    def test_rollback_refuses_unit_profile_or_plan_drift(self):
        for change in ('unit', 'original', 'profile', 'digest', 'plan'):
            proposal, args = self.rollback_args()
            if change == 'unit': args['current_unit'] += b'# foreign edit\n'
            if change == 'original': args['original_unit'] += b'# foreign edit\n'
            if change == 'profile': args['current_profile_state']['files']['mesh.conf']['sha256'] = 'e'*64
            if change == 'digest': args['expected_plan_sha256'] = 'e'*64
            if change == 'plan': proposal['profile'] = '/wrong/profile'
            with self.subTest(change=change), self.assertRaises(m.LifecyclePlanRejected):
                m.rollback_preview(proposal, **args)

    def test_same_content_foreign_marker_or_unknown_marker_never_removed(self):
        for field, value in [('inode', 99), ('device', 7), ('uid', 0), ('size', 1),
                             ('links', 2), ('mode', 0o644), ('sha256', 'e'*64), ('path', '/other/file')]:
            proposal, args = self.rollback_args(); args['current_marker'][field] = value
            with self.subTest(field=field), self.assertRaises(m.LifecyclePlanRejected):
                m.rollback_preview(proposal, **args)
        proposal, args = self.rollback_args(); args['current_marker'] = args['owned_marker'] = None
        with self.assertRaises(m.LifecyclePlanRejected): m.rollback_preview(proposal, **args)

    def test_failed_start_without_created_marker_can_restore_unit_when_absence_proven(self):
        proposal, args = self.rollback_args()
        args.update(owned_marker=None, current_marker=None, marker_absence_verified=True)
        result = m.rollback_preview(proposal, **args)
        self.assertIsNone(result['marker_remove_only_if_unchanged'])
        self.assertEqual(result['unit_restore_utf8'].encode(), self.unit)
        self.assertFalse(result['applied'])

    def test_live_background_or_frontend_blocks_rollback(self):
        for field, value in [('background_stopped', False), ('attached_gui_pids', [42]),
                             ('attached_gui_pids', None)]:
            proposal, args = self.rollback_args(); args[field] = value
            with self.subTest(field=field), self.assertRaises(m.LifecyclePlanRejected):
                m.rollback_preview(proposal, **args)

    def test_original_unit_identity_and_permissions_must_be_bound(self):
        for field, value in [('uid', 0), ('mode', 0o666), ('links', 2), ('inode', 0), ('sha256', 'e'*64)]:
            facts = copy.deepcopy(self.facts); facts['unit_state'][field] = value
            with self.subTest(field=field), self.assertRaises(m.LifecyclePlanRejected):
                m.plan(self.unit, **facts)

    def test_same_bytes_foreign_unit_inode_is_not_rollback_ownership(self):
        proposal, args = self.rollback_args()
        args['current_unit_state']['inode'] = 99
        with self.assertRaises(m.LifecyclePlanRejected):
            m.rollback_preview(proposal, **args)

    def test_isolated_cli_only_writes_new_private_proposal(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve(); root.chmod(0o700)
            unit = root/'fixture.service'; unit.write_bytes(self.unit)
            facts = root/'facts.json'; facts.write_text(json.dumps(self.facts))
            before = {p.name: p.read_bytes() for p in root.iterdir()}
            output = root/'proposal.json'
            script = Path(m.__file__).resolve()
            args = [sys.executable, '-B', str(script), '--unit', str(unit), '--facts', str(facts), '--output', str(output)]
            first = subprocess.run(args, capture_output=True, text=True)
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertFalse(json.loads(first.stdout)['applied'])
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            self.assertEqual({p.name: p.read_bytes() for p in root.iterdir() if p != output}, before)
            saved = output.read_bytes()
            second = subprocess.run(args, capture_output=True, text=True)
            self.assertNotEqual(second.returncode, 0)
            self.assertEqual(output.read_bytes(), saved)
            self.assertEqual(set(p.name for p in root.iterdir()), {'fixture.service', 'facts.json', 'proposal.json'})


if __name__ == '__main__':
    unittest.main()
