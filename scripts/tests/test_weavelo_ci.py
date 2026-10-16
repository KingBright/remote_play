"""CI guard tests use private disposable artifacts, never a running App or NAS."""
import copy
import hashlib
import json
from pathlib import Path
import platform
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import weavelo_ci as ci


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='rp-ci-fixture-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        scripts = self.root / 'scripts'; scripts.mkdir()
        (scripts / 'verify_prebuilt_binary.py').write_bytes((Path(ci.__file__).parent / 'verify_prebuilt_binary.py').read_bytes())
        cpu = 0x100000c if platform.machine() in ('arm64', 'aarch64') else 0x1000007
        self.binary = self.root / 'fixture.bin'; self.binary.write_bytes(b'\xcf\xfa\xed\xfe' + cpu.to_bytes(4, 'little') + b'fixture bytes')
        self.spec = {'schema': 1, 'machine': platform.node(), 'architecture': platform.machine(),
                     'execution_role': 'builder', 'repo': str(self.root), 'source_commit': '1' * 40,
                     'platform': 'macos', 'artifact': {'path': str(self.binary),
                     'sha256': hashlib.sha256(self.binary.read_bytes()).hexdigest(), 'purpose': 'diagnostic'}}
        self.journal = self.root / 'run.json'
        self.git = patch.object(ci.subprocess, 'check_output', return_value='1' * 40 + '\n')
        self.git.start(); self.addCleanup(self.git.stop)

    def test_wrong_machine_stops_before_git_or_execution(self):
        self.spec['machine'] = 'wrong-machine'
        with self.assertRaisesRegex(ValueError, 'machine'): ci.run(self.spec, 'verify', self.journal)
        self.assertFalse(self.journal.exists())

    def test_wrong_source_commit_is_rejected(self):
        self.spec['source_commit'] = '2' * 40
        with self.assertRaisesRegex(ValueError, 'commit'): ci.run(self.spec, 'verify', self.journal)

    def test_nas_never_runs_compile_test_or_package(self):
        self.spec['execution_role'] = 'nas-runtime'
        for step in ('build', 'test', 'package'):
            with self.assertRaisesRegex(ValueError, 'NAS'): ci.run(self.spec, step, self.journal)

    def test_wrong_artifact_hash_is_rejected(self):
        self.spec['artifact']['sha256'] = 'f' * 64
        with self.assertRaisesRegex(ValueError, 'differs'): ci.run(self.spec, 'verify', self.journal)

    def test_platform_mismatch_cannot_be_hidden_by_a_valid_hash(self):
        self.spec['platform'] = 'linux'
        with self.assertRaisesRegex(ValueError, 'format'): ci.run(self.spec, 'verify', self.journal)

    def test_source_snapshot_required_for_compilation(self):
        with patch.object(ci.platform, 'system', return_value='Darwin'), patch.object(ci.identity, 'source_snapshot', return_value={'snapshot_sha256': 'a' * 64}):
            with self.assertRaisesRegex(ValueError, 'source bytes'): ci.run(self.spec, 'build', self.journal)

    def test_diagnostic_is_not_an_installable_release(self):
        with self.assertRaisesRegex(ValueError, 'diagnostic helper'): ci.command(self.spec, 'deploy', self.root)

    def test_unknown_fields_cannot_store_secret_values(self):
        self.spec['token'] = 'never-record-this'
        with self.assertRaisesRegex(ValueError, 'unknown spec'): ci.run(self.spec, 'verify', self.journal)
        self.assertFalse(self.journal.exists())

    def test_dry_run_does_not_execute_or_write_journal(self):
        with patch.object(ci.subprocess, 'run') as runner:
            result = ci.run(self.spec, 'verify', self.journal)
        self.assertEqual(result['state'], 'dry_run'); runner.assert_not_called()
        self.assertFalse(self.journal.exists())

    def test_actual_verifier_runs_once_then_same_hash_is_not_republished(self):
        result = ci.run(self.spec, 'verify', self.journal, True)
        self.assertEqual(result['state'], 'completed')
        with patch.object(ci.subprocess, 'run') as runner:
            repeated = ci.run(self.spec, 'verify', self.journal, True)
        self.assertEqual(repeated['state'], 'already_completed'); runner.assert_not_called()

    def test_prior_unknown_outcome_must_be_observed_not_replayed(self):
        key = ci.digest({'spec': self.spec, 'step': 'verify'})
        self.journal.write_text(json.dumps({'schema': 1, 'stages': {key: {'state': 'outcome_unknown'}}}))
        with self.assertRaisesRegex(ValueError, 'original receipt'): ci.run(self.spec, 'verify', self.journal, True)

    def test_concurrent_run_lock_is_not_stolen(self):
        self.journal.with_name(self.journal.name + '.lock').mkdir()
        with self.assertRaisesRegex(ValueError, 'locked'): ci.run(self.spec, 'verify', self.journal, True)

    def test_changed_bytes_cannot_reuse_a_completed_receipt(self):
        ci.run(self.spec, 'verify', self.journal, True)
        self.binary.write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'differs'): ci.run(self.spec, 'verify', self.journal, True)


if __name__ == '__main__':
    unittest.main()
