from copy import deepcopy
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

PATH = Path(__file__).parents[1] / 'acceptance/inspect_connection_lifecycle.py'
SPEC = importlib.util.spec_from_file_location('lifecycle_inspection', PATH)
inspection = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(inspection)


class LifecycleInspectionTests(unittest.TestCase):
    def setUp(self):
        self.record = dict(connection_id=123, state='closed', observed_at_ms=42,
                           active_capture_workers=0, native_streams=0, native_stops_pending=0,
                           native_stops_completed=1, native_stop_errors=0,
                           captured_frames=53, encoded_frames=53)

    def test_unknown_or_colliding_id_cannot_report_release(self):
        for records in ([], [self.record, self.record], [dict(self.record, connection_id=124)]):
            with self.assertRaises(ValueError):
                inspection.validate(records, 123)

    def test_stop_failure_active_resource_or_changing_counter_fails_closed(self):
        for change in ({'state': 'closing'}, {'state': 'teardown_unknown'}, {'native_stops_pending': 1}, {'active_capture_workers': 1},
                       {'native_streams': 1}, {'native_stop_errors': 1},
                       {'native_stops_completed': 0}, {'encoded_frames': 0},
                       {'captured_frames': 54}):
            with self.assertRaises(ValueError):
                inspection.confirmed_closed(self.record, dict(self.record, **change))
        inspection.confirmed_closed(self.record, deepcopy(self.record))

    def test_wrong_backend_stops_before_diagnostic_query(self):
        with patch.object(inspection, 'request', return_value={'Snapshot': {'executable_sha256': 'b' * 64}}) as call:
            with self.assertRaises(ValueError):
                inspection.inspect(Path('/fixture'), 123, 'a' * 64)
            self.assertEqual(call.call_count, 1)

    def test_pairing_and_other_sessions_are_not_exported(self):
        snapshot = {'pid': 1234, 'generation': 'fixture', 'executable_sha256': 'a' * 64,
                    'pairing': {'secret': 'never-export'}, 'peers': ['other-session']}
        responses = [{'Snapshot': snapshot}, {'ConnectionDiagnostics': [self.record]},
                     {'ConnectionDiagnostics': [self.record]}, {'Snapshot': snapshot}]
        with patch.object(inspection, 'request', side_effect=responses), patch.object(inspection.time, 'sleep'):
            result = inspection.inspect(Path('/fixture'), 123, 'a' * 64, True)
        self.assertTrue(result['closed_video_resources_confirmed'])
        self.assertNotIn('never-export', str(result))
        self.assertNotIn('other-session', str(result))
