import copy
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import execute_linux_lifecycle as e
import test_linux_background_lifecycle_plan as fixtures


class Backend:
    """Only synthetic state; no process, profile, unit, credentials or OS calls."""
    def __init__(self, proposal, raw):
        self.p = proposal; self.raw = raw
        self.root = {'phase': 'installed_and_running_verified', 'unrelated': {'keep': 'exact'}, 'pid': 41}
        self.events = []; self.old = True; self.unit = 'before'; self.marker = False
        self.gui = False; self.background = False; self.stage_present = False
        self.foreign = False; self.fail = None; self.crash = None

    def hit(self, name):
        self.events.append(name)
        if self.fail == name:
            self.fail = None
            raise RuntimeError('isolated injected failure')

    def load(self): return copy.deepcopy(self.root)
    def save(self, root):
        self.root = copy.deepcopy(root)
        phase = root[e.KEY]['phase']; self.events.append('save:'+phase)
        if self.crash == phase:
            self.crash = None
            raise KeyboardInterrupt('isolated process interruption')

    def preflight(self, p): self.hit('preflight')
    def unit_bytes(self): return self.raw
    def stage(self, p):
        self.hit('stage'); self.stage_present = True
        return {'inode': 77, 'sha256': p['unit_after_sha256']}
    def stop_old(self, p):
        self.old = False; self.hit('stop_old')
    def publish(self, p, state):
        self.unit = 'after'; self.hit('publish')
        return copy.deepcopy(state['stage'])
    def start_background(self, p, state):
        self.hit('start_before_effect')
        self.background = self.marker = True
        self.hit('start_after_effect')
        return {'pid': 42, 'generation': 'background-fixture', 'marker': {'inode': 88}}
    def gui_test(self, p, state, persist):
        self.gui = True; state['gui_pid'] = 43; state['gui_start_ticks'] = 'fixture'
        persist(); self.hit('gui_close')
        self.gui = False
        return {'pid': 43, 'normal_exit': True, 'native_image_verified': True, 'render_calls': 2}
    def verify_terminal(self, p, state):
        self.hit('verify:'+state['phase'])
        e.policy.require(not self.foreign, 'Unknown edit retained')
        if state['phase'] == 'committed':
            e.policy.require(self.unit == 'after' and self.background and self.marker and not self.gui, 'Bad commit')
        else:
            e.policy.require(self.unit == 'before' and self.old and not self.marker, 'Bad rollback')
    def rollback(self, p, state):
        self.hit('rollback')
        e.policy.require(not self.foreign, 'Unknown file or generation retained')
        if self.marker:
            e.policy.require('background' in state, 'Unreceipted marker preserved')
        self.gui = self.background = self.marker = False
        self.unit = 'before'; self.old = True


class ExecutorTests(unittest.TestCase):
    def setUp(self):
        case = fixtures.LinuxSeparationPlanTests(); case.setUp()
        self.p = case.proposal(); self.b = Backend(self.p, case.unit)

    def run_request(self, recover=False):
        return e.execute(self.b, self.p, self.p['plan_sha256'], recover)

    def test_success_intents_precede_effects_and_preserve_original_journal_fields(self):
        result = self.run_request()
        self.assertEqual(result['phase'], 'committed')
        self.assertTrue(result['native_gui_close_verified'])
        for intent, effect in [('stop_requested', 'stop_old'), ('exchange_requested', 'publish'),
                              ('background_start_requested', 'start_before_effect'), ('gui_started', 'gui_close')]:
            self.assertLess(self.b.events.index('save:'+intent), self.b.events.index(effect))
        self.assertEqual(self.b.root['phase'], 'installed_and_running_verified')
        self.assertEqual(self.b.root['pid'], 41)
        self.assertEqual(self.b.root['unrelated'], {'keep': 'exact'})

    def test_duplicate_committed_request_checks_state_without_replaying_effects(self):
        self.run_request(); before = list(self.b.events)
        result = self.run_request()
        self.assertEqual(result['phase'], 'committed')
        self.assertEqual(self.b.events[len(before):], ['verify:committed'])

    def test_different_plan_or_bad_digest_never_changes_original(self):
        original = copy.deepcopy(self.b.root)
        with self.assertRaises(e.policy.LifecyclePlanRejected):
            e.execute(self.b, self.p, 'e'*64)
        self.assertEqual(self.b.root, original)
        self.run_request(); self.b.root[e.KEY]['plan_sha256'] = 'e'*64
        with self.assertRaises(e.policy.LifecyclePlanRejected): self.run_request()

    def test_known_failure_boundaries_restore_original_owner_once(self):
        for step in ('stage', 'stop_old', 'publish', 'start_before_effect', 'gui_close', 'verify:committed'):
            self.setUp(); self.b.fail = step
            with self.subTest(step=step), self.assertRaises(RuntimeError): self.run_request()
            self.assertEqual(self.b.root[e.KEY]['phase'], 'rolled_back')
            self.assertTrue(self.b.old); self.assertFalse(self.b.background); self.assertFalse(self.b.gui)
            before = len(self.b.events); result = self.run_request()
            self.assertEqual(result['phase'], 'rolled_back')
            self.assertEqual(self.b.events[before:], ['verify:rolled_back'])

    def test_interrupted_request_does_not_replay_and_recovers_explicitly(self):
        for phase in ('prepared', 'stop_requested', 'old_stopped', 'exchange_requested', 'unit_published'):
            self.setUp(); self.b.crash = phase
            with self.subTest(phase=phase), self.assertRaises(KeyboardInterrupt): self.run_request()
            before = len(self.b.events)
            with self.assertRaises(e.policy.LifecyclePlanRejected): self.run_request()
            self.assertEqual(len(self.b.events), before)
            self.assertEqual(self.run_request(recover=True)['phase'], 'rolled_back')

    def test_unknown_edits_block_recovery_and_are_never_overwritten(self):
        self.b.crash = 'unit_published'
        with self.assertRaises(KeyboardInterrupt): self.run_request()
        self.b.foreign = True
        with self.assertRaises(e.policy.LifecyclePlanRejected): self.run_request(recover=True)
        self.assertTrue(self.b.foreign); self.assertEqual(self.b.unit, 'after')
        self.assertEqual(self.b.root[e.KEY]['phase'], 'unit_published')

    def test_unreceipted_marker_after_unknown_start_is_preserved(self):
        self.b.fail = 'start_after_effect'
        with self.assertRaisesRegex(RuntimeError, 'Recovery incomplete'): self.run_request()
        self.assertTrue(self.b.marker); self.assertTrue(self.b.background)
        self.assertEqual(self.b.root[e.KEY]['phase'], 'recovery_required')
        before = len(self.b.events)
        with self.assertRaises(e.policy.LifecyclePlanRejected): self.run_request()
        self.assertEqual(len(self.b.events), before)

    def test_preflight_refusal_has_no_transaction_or_effect(self):
        self.b.fail = 'preflight'
        with self.assertRaises(RuntimeError): self.run_request()
        self.assertNotIn(e.KEY, self.b.root)
        self.assertTrue(self.b.old); self.assertFalse(self.b.stage_present)

    def test_recovery_without_existing_subtransaction_is_refused(self):
        with self.assertRaises(e.policy.LifecyclePlanRejected): self.run_request(recover=True)
        self.assertTrue(self.b.old); self.assertNotIn(e.KEY, self.b.root)

    def test_actual_installed_capability_cannot_be_overridden_by_reviewed_proposal(self):
        from unittest.mock import Mock
        gate = Mock()
        gate.product_info.return_value = {'separated_lifecycle':False,'background_entry':None,
            'frontend_entry':None,'default_gui':'restored-original-gpui','original_gui_compiled':True}
        self.assertTrue(self.p['applied'] is False)
        with self.assertRaises(e.policy.LifecyclePlanRejected):
            e.checked_installed_product(gate,Path('/fixture/executable'),{},'b'*64)
        self.assertNotIn(e.KEY,self.b.root)
        self.assertTrue(self.b.old)

    def test_changed_committed_state_is_not_reported_as_duplicate_success(self):
        self.run_request(); self.b.foreign = True
        with self.assertRaises(e.policy.LifecyclePlanRejected): self.run_request()


class NativeStorageSafetyTests(unittest.TestCase):
    def setUp(self):
        import tempfile
        import build_identity_gate
        from unittest.mock import Mock, patch
        # macOS Python lacks Linux xattr APIs. Mock just this syscall boundary;
        # inode/mode/journal tests still operate on real isolated files.
        self.attrs = {}
        def set_attr(path, name, value): self.attrs.setdefault(str(path), {})[name] = value
        for name, effect in [('listxattr', lambda path: list(self.attrs.get(str(path), {}))),
                             ('getxattr', lambda path, key: self.attrs[str(path)][key]),
                             ('setxattr', set_attr)]:
            mock = patch.object(e.os, name, side_effect=effect, create=True)
            mock.start(); self.addCleanup(mock.stop)
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.b = e.Native.__new__(e.Native)
        self.b.gate = build_identity_gate
        self.b.work = self.root
        self.b.unit = self.root/'fixture.service'
        self.b.pending = self.root/'.fixture.pending'
        self.b.journal = self.root/'journal.json'
        self.b.profile = self.root/'profile'; self.b.profile.mkdir(mode=0o700)
        self.b.unit.write_bytes(b'original inert unit\n'); self.b.unit.chmod(0o644)
        self.attrs[str(self.b.unit)] = {'fixture.attribute': b'inert metadata only'}
        self.b.journal.write_text('{"phase":"installed_and_running_verified","keep":7}')
        self.b.journal.chmod(0o600)
        self.b.preflight = Mock(); self.b.guard = Mock()
        self.plan = {'unit_state':self.b.file_state(self.b.unit),
            'unit_after_utf8':'changed inert unit\n','unit_security_sha256':self.b.security(self.b.unit)}

    def tearDown(self): self.temp.cleanup()

    def test_real_journal_cas_preserves_unknown_edit_and_no_pending_created(self):
        root = self.b.load(); self.b.journal.write_text('concurrent unrelated edit')
        with self.assertRaises(e.policy.LifecyclePlanRejected): self.b.save(root)
        self.assertEqual(self.b.journal.read_text(), 'concurrent unrelated edit')
        self.assertFalse(self.b.journal.with_suffix('.pending').exists())

    def test_native_stage_preserves_mode_security_and_refuses_existing_pending(self):
        state = self.b.stage(self.plan)
        self.assertEqual(state['mode'], self.plan['unit_state']['mode'])
        self.assertEqual(self.b.security(self.b.pending), self.plan['unit_security_sha256'])
        before = self.b.pending.read_bytes()
        with self.assertRaises(FileExistsError): self.b.stage(self.plan)
        self.assertEqual(self.b.pending.read_bytes(), before)

    def test_native_publish_refuses_same_bytes_foreign_inode_before_exchange(self):
        from unittest.mock import Mock
        state = {'stage':self.b.stage(self.plan)}
        self.b.unit.rename(self.root/'retained-original')
        self.b.unit.write_bytes(b'original inert unit\n')
        self.b.exchange = Mock()
        with self.assertRaises(e.policy.LifecyclePlanRejected): self.b.publish(self.plan, state)
        self.b.exchange.assert_not_called()
        self.assertEqual(self.b.unit.read_bytes(), b'original inert unit\n')

    def test_native_publish_refuses_security_metadata_drift_before_exchange(self):
        from unittest.mock import Mock
        state = {'stage': self.b.stage(self.plan)}
        self.attrs[str(self.b.unit)]['fixture.attribute'] = b'foreign change'
        self.b.exchange = Mock()
        with self.assertRaises(e.policy.LifecyclePlanRejected): self.b.publish(self.plan, state)
        self.b.exchange.assert_not_called()

    def test_native_publish_detects_post_exchange_replacement_and_preserves_original(self):
        state = {'stage':self.b.stage(self.plan)}
        def race():
            self.b.unit.rename(self.root/'old')
            self.b.pending.rename(self.b.unit)
            (self.root/'old').rename(self.b.pending)
            self.b.unit.rename(self.root/'retained-ours')
            self.b.unit.write_bytes(b'foreign concurrent unit\n')
        self.b.exchange = race
        with self.assertRaises(e.policy.LifecyclePlanRejected): self.b.publish(self.plan, state)
        self.assertEqual(self.b.pending.read_bytes(), b'original inert unit\n')
        self.assertEqual(self.b.unit.read_bytes(), b'foreign concurrent unit\n')

    def test_gui_pid_reuse_never_signals_even_same_application(self):
        from unittest.mock import Mock, patch
        self.b.start_ticks = Mock(return_value='new-start')
        self.b.native = Mock()
        with patch.object(e.os,'pidfd_open',return_value=77,create=True), \
             patch.object(e.signal,'pidfd_send_signal',create=True) as send, \
             patch.object(e.os,'close'):
            with self.assertRaises(e.policy.LifecyclePlanRejected):
                self.b.stop_gui({'gui_pid':41,'gui_start_ticks':'original-start'})
            send.assert_not_called(); self.b.native.assert_not_called()

    def test_effective_weight_mismatch_rejected_even_when_drop_in_bytes_match(self):
        from unittest.mock import Mock, patch
        case=fixtures.LinuxSeparationPlanTests();case.setUp()
        case.facts['observation']['drop_ins']=case.overlays()
        self.b.plan=case.proposal();self.b.check_drop_ins=Mock()
        output='MainPID=41\nDropInPaths=reviewed\nActiveState=active\nSubState=running\nJob=\nCPUWeight=100\nTimeoutStopFailureMode=abort\n'
        with patch.object(e.subprocess,'check_output',return_value=output):
            with self.assertRaises(e.policy.LifecyclePlanRejected):self.b.service()
        self.b.check_drop_ins.assert_called_once_with(['reviewed'])

    def test_pending_stop_abort_job_never_counts_as_stable_old_owner(self):
        from unittest.mock import Mock
        self.b.service=Mock(return_value=41)
        self.b.service_state={'ActiveState':'deactivating','SubState':'stop-sigterm','Job':'123'}
        with self.assertRaises(e.policy.LifecyclePlanRejected):self.b.settled(True)
        self.b.service=Mock(return_value=0)
        with self.assertRaises(e.policy.LifecyclePlanRejected):self.b.settled(False)
        self.b.service_state={'ActiveState':'inactive','SubState':'dead','Job':''}
        self.assertEqual(self.b.settled(False),0)

    def test_foreign_locked_frontend_is_preserved(self):
        import fcntl,os
        path = self.b.profile/'gui-state/desktop-instance.lock'; path.parent.mkdir(mode=0o700)
        path.write_bytes(b''); path.chmod(0o600)
        fd = os.open(path,os.O_RDWR)
        try:
            fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
            with self.assertRaises(BlockingIOError): self.b.frontend_clear()
            self.assertTrue(path.exists())
        finally: os.close(fd)


if __name__ == '__main__': unittest.main()
