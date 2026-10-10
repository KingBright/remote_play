import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
spec=importlib.util.spec_from_file_location('verify_prebuilt_binary',Path(__file__).resolve().parents[1]/'verify_prebuilt_binary.py')
m=importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
class PrebuiltBinaryTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.file=Path(self.tmp.name)/'binary';self.file.write_bytes(b'verified build fixture')
        self.sha=hashlib.sha256(self.file.read_bytes()).hexdigest()
    def test_verified_artifact_is_accepted_without_modification(self):
        before=self.file.stat().st_mtime_ns
        self.assertEqual(m.verified_path(self.file,self.sha),self.file.resolve())
        self.assertEqual(before,self.file.stat().st_mtime_ns)
    def test_changed_artifact_is_rejected(self):
        self.file.write_bytes(b'not the tested build')
        with self.assertRaises(ValueError):m.verified_path(self.file,self.sha)
    def test_relative_missing_and_non_digest_are_rejected(self):
        for path,digest in [(Path('relative'),self.sha),(self.file.parent/'absent',self.sha),(self.file,''),(self.file,'0'*63)]:
            with self.assertRaises(ValueError):m.verified_path(path,digest)
    def test_symbolic_link_is_not_silently_followed(self):
        link=self.file.parent/'alias';link.symlink_to(self.file)
        with self.assertRaises(ValueError):m.verified_path(link,self.sha)
