import hashlib
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('refresh_transferred_sources', Path(__file__).resolve().parents[1] / 'refresh_transferred_sources.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class SourceRefreshTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.source = self.root / 'app.rs'
        self.source.write_bytes(b'fn main() {}\n')
        os.utime(self.source, ns=(1000000000, 1000000000))
        self.delta = {'app.rs': {'after': hashlib.sha256(self.source.read_bytes()).hexdigest()}}

    def test_plan_is_read_only(self):
        result = module.refresh(self.root, self.delta)
        self.assertFalse(result['applied'])
        self.assertEqual(self.source.stat().st_mtime_ns, 1000000000)

    def test_older_transferred_mtime_is_refreshed_without_changing_bytes(self):
        before = self.source.read_bytes()
        result = module.refresh(self.root, self.delta, apply=True)
        self.assertGreater(self.source.stat().st_mtime_ns, 1000000000)
        self.assertEqual(self.source.read_bytes(), before)
        self.assertFalse(result['compiled_binary_verified'])

    def test_all_hashes_checked_before_any_timestamp_write(self):
        bad = self.root / 'bad.rs'
        bad.write_bytes(b'changed')
        self.delta['bad.rs'] = {'after': '0' * 64}
        with self.assertRaises(ValueError):
            module.refresh(self.root, self.delta, apply=True)
        self.assertEqual(self.source.stat().st_mtime_ns, 1000000000)

    def test_unrelated_files_unchanged(self):
        other = self.root / 'untouched.rs'
        other.write_bytes(b'other')
        stamp = other.stat().st_mtime_ns
        module.refresh(self.root, self.delta, apply=True)
        self.assertEqual(other.stat().st_mtime_ns, stamp)

    def test_path_escape_and_missing_hash_rejected(self):
        for name in ('../app.rs', '/app.rs', 'C:/app.rs', 'a\\app.rs'):
            with self.assertRaises(ValueError):
                module.refresh(self.root, {name: self.delta['app.rs']}, apply=True)
        with self.assertRaises(ValueError):
            module.refresh(self.root, {'app.rs': {}}, apply=True)

    def test_symbolic_link_rejected(self):
        link = self.root / 'link.rs'
        try:
            link.symlink_to(self.source)
        except OSError:
            self.skipTest('symbolic links unavailable')
        with self.assertRaises(ValueError):
            module.refresh(self.root, {'link.rs': self.delta['app.rs']}, apply=True)
