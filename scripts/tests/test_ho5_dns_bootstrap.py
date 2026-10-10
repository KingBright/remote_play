"""Exercise the documented root bootstrap as an ordinary user on public fixtures."""
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
DOCUMENT = ROOT / "docs/reviews/2026-10-10/HO5-EXACT-DNS-USER-STEP.md"
SCRIPT = ROOT / "scripts/repair_ho5_portal_transfer_dns.py"


def documented_command():
    lines = DOCUMENT.read_text().splitlines()
    line = next(line for line in lines if line.startswith("sudo /usr/bin/python3 -I -c "))
    args = shlex.split(line)
    if args[:4] != ["sudo", "/usr/bin/python3", "-I", "-c"] or len(args) != 5:
        raise AssertionError("unexpected documented administrator command")
    return args[4]


@unittest.skipUnless(os.name == "posix", "native HO5 bootstrap uses Unix file interfaces")
class IsolatedBootstrapTests(unittest.TestCase):
    def run_fixture(self, path, expected, cwd):
        command = documented_command()
        command = command.replace(
            'p="/var/home/liang/workspace/.rp-source-picture-20261009/repair-ho5-exact-dns.py"',
            "p=" + json.dumps(str(path)))
        command = re.sub(r'hexdigest\(\)=="[0-9a-f]{64}"', 'hexdigest()=="' + expected + '"', command)
        return subprocess.run([sys.executable, "-I", "-c", command], cwd=cwd,
                              capture_output=True, text=True, timeout=5)

    def test_document_hash_binds_current_script(self):
        digest = hashlib.sha256(SCRIPT.read_bytes()).hexdigest()
        self.assertIn('hexdigest()=="' + digest + '"', documented_command())

    def test_user_directory_cannot_shadow_stdlib_imports(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = root / "script.py"
            payload = b'import json; print(json.dumps({"fixture": "verified"}))\n'
            script.write_bytes(payload)
            (root / "json.py").write_text('raise RuntimeError("untrusted module executed")\n')
            result = self.run_fixture(script, hashlib.sha256(payload).hexdigest(), root)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout), {"fixture": "verified"})

    def test_changed_script_is_never_executed(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "script.py"
            expected = hashlib.sha256(b'print("old")\n').hexdigest()
            script.write_text('print("changed payload executed")\n')
            result = self.run_fixture(script, expected, directory)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")
            self.assertIn("script identity mismatch", result.stderr)

    def test_symlink_is_not_followed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target.py"
            payload = b'print("fixture")\n'
            target.write_bytes(payload)
            link = root / "link.py"
            link.symlink_to(target)
            result = self.run_fixture(link, hashlib.sha256(payload).hexdigest(), directory)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")

    def test_named_pipe_is_rejected_without_waiting_for_writer(self):
        with tempfile.TemporaryDirectory() as directory:
            fifo = Path(directory) / "script.py"
            os.mkfifo(fifo)
            result = self.run_fixture(fifo, "0" * 64, directory)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")

    def test_oversized_script_is_rejected_before_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "script.py"
            payload = b'print("fixture")\n' + b'#' * 65536
            script.write_bytes(payload)
            result = self.run_fixture(script, hashlib.sha256(payload).hexdigest(), directory)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
