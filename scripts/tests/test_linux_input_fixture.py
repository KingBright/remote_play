import copy
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('linux_effects',
    Path(__file__).resolve().parents[1]/'acceptance/linux_input_fixture.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class ModelTests(unittest.TestCase):
    def model(self, mode='native-window'):
        return m.Effects('a'*32, '73122905', {'pid': 41, 'start_ticks': 59}, mode)

    def complete(self, state):
        state.focus(True)
        state.button(True, True, True)
        state.button(False, True, True)
        for digit in state.challenge:
            state.key(True, ord(digit))
            state.key(False, ord(digit))

    def pair(self):
        state = self.model()
        before = state.snapshot('GdkWaylandDisplay')
        self.complete(state)
        return before, state.snapshot('GdkWaylandDisplay')

    def test_click_focus_and_complete_synthetic_digits(self):
        before, after = self.pair()
        result = m.verify(before, after)
        self.assertTrue(result['native_window_effects_verified'])
        self.assertFalse(result['product_sender_attribution_verified'])
        self.assertFalse(result['end_to_end_acceptance'])

    def test_arbitrary_typing_never_retained(self):
        state = self.model()
        state.focus(True)
        for char in 'private-user-data':
            self.assertFalse(state.key(True, ord(char)))
            self.assertFalse(state.key(False, ord(char)))
        data = state.snapshot('fixture')
        self.assertEqual(data['matched_digits'], 0)
        self.assertNotIn('private-user-data', json.dumps(data))
        self.assertNotIn('keyval', data)
        self.assertNotIn('input_events', data)

    def test_input_without_focus_never_counts(self):
        state = self.model()
        state.button(True, True, True)
        state.button(False, True, True)
        state.key(True, ord('7'))
        state.key(False, ord('7'))
        self.assertEqual((state.clicks, state.matched), (0, 0))

    def test_wrong_modifiers_and_unmatched_release_never_advance(self):
        state = self.model()
        state.focus(True)
        state.key(False, ord('7'))
        state.key(True, ord('7'), True)
        state.key(False, ord('7'), True)
        self.assertEqual(state.matched, 0)

    def test_autorepeat_does_not_count_as_multiple_keys(self):
        state = self.model()
        state.focus(True)
        for _ in range(30):
            state.key(True, ord('7'))
        state.key(False, ord('7'))
        self.assertEqual(state.matched, 1)

    def test_nonprimary_or_outside_click_not_counted(self):
        for primary, within in [(False, True), (True, False)]:
            state = self.model()
            state.focus(True)
            state.button(True, primary, within)
            state.button(False, primary, within)
            self.assertEqual(state.clicks, 0)

    def test_focus_loss_does_not_invent_releases(self):
        state = self.model()
        state.focus(True)
        state.key(True, ord('7'))
        state.button(True, True, True)
        state.focus(False)
        self.assertTrue(state.key_pending)
        self.assertTrue(state.button_pending)
        self.assertTrue(state.coverage_lost)

    def test_invalid_public_parameters_refused(self):
        for nonce, challenge in [('bad', '73122905'), ('a'*32, 'password'), ('a'*32, '１２３４５６７８')]:
            with self.subTest(nonce=nonce, challenge=challenge), self.assertRaises(ValueError):
                m.Effects(nonce, challenge, {})

    def test_selftest_receipts_cannot_become_live_acceptance(self):
        before, after = self.pair()
        for state in (before, after):
            state['mode'] = 'gtk-offscreen-selftest'
        with self.assertRaises(ValueError):
            m.verify(before, after)

    def test_changed_identity_rejected(self):
        for name in ('pid', 'start_ticks', 'nonce', 'title', 'challenge', 'backend', 'started_ns'):
            before, after = self.pair()
            after[name] = 100 if isinstance(after[name], int) else 'changed'
            with self.subTest(field=name), self.assertRaises(ValueError):
                m.verify(before, after)

    def test_missing_native_effect_or_held_input_rejected(self):
        for name, value in [('clicks', 0), ('clicks', 2), ('matched_digits', 7),
                ('expected_key_pending', True), ('primary_button_pending', True),
                ('focused', False), ('focus_gains', 0), ('coverage_lost', True)]:
            before, after = self.pair()
            after[name] = value
            with self.subTest(field=name), self.assertRaises(ValueError):
                m.verify(before, after)

    def test_replay_dirty_baseline_and_stale_results_refused(self):
        for change in ('same-time', 'stale', 'dirty'):
            before, after = self.pair()
            if change == 'same-time': after['observed_ns'] = before['observed_ns']
            if change == 'stale': after['observed_ns'] = before['observed_ns'] + 181_000_000_000
            if change == 'dirty': before['clicks'] = 1
            with self.subTest(change=change), self.assertRaises(ValueError):
                m.verify(before, after)


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()/'only-this-run'

    def tearDown(self):
        self.temp.cleanup()

    def test_private_bounded_atomic_state_and_no_pending_left(self):
        writer = m.Evidence(self.root)
        try:
            writer.write({'step': 1})
            writer.write({'step': 2})
            self.assertEqual(json.loads((self.root/'state.json').read_text()), {'step': 2})
            self.assertEqual(self.root.stat().st_mode & 0o777, 0o700)
            self.assertEqual((self.root/'state.json').stat().st_mode & 0o777, 0o600)
            self.assertFalse((self.root/'.state.pending').exists())
        finally:
            writer.close()

    def test_existing_output_never_overwritten(self):
        self.root.mkdir()
        with self.assertRaises(FileExistsError):
            m.Evidence(self.root)

    def test_unknown_pending_preserved(self):
        writer = m.Evidence(self.root)
        try:
            (self.root/'.state.pending').write_text('unknown')
            with self.assertRaises(FileExistsError):
                writer.write({'step': 1})
            self.assertEqual((self.root/'.state.pending').read_text(), 'unknown')
        finally:
            writer.close()

    def test_external_state_change_preserved(self):
        writer = m.Evidence(self.root)
        try:
            writer.write({'step': 1})
            (self.root/'state.json').write_text('unknown')
            with self.assertRaises(ValueError):
                writer.write({'step': 2})
            self.assertEqual((self.root/'state.json').read_text(), 'unknown')
        finally:
            writer.close()

    def test_symlink_and_hardlink_state_refused(self):
        for linked in ('symlink', 'hardlink'):
            root = self.root/linked
            self.root.mkdir(exist_ok=True)
            writer = m.Evidence(root)
            try:
                writer.write({'step': 1})
                path = root/'state.json'
                if linked == 'symlink':
                    path.rename(root/'original')
                    path.symlink_to(root/'original')
                else:
                    os.link(path, root/'other')
                with self.assertRaises((ValueError, OSError)):
                    writer.write({'step': 2})
            finally:
                writer.close()


if __name__ == '__main__':
    unittest.main()
