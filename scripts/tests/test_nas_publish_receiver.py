"""Exercise the existing NAS receiver in a local temp folder: zero remote calls."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest


class ReceiverTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='rp-nas-receiver-fixture-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name); self.stage = self.root / 'stage'; self.stage.mkdir()
        self.target = self.root / 'receiver'
        script = (Path(__file__).parents[1] / 'publish_desktop_nas.sh').read_text()
        begin = script.index("<<'PY'", script.index('ssh -p')) + len("<<'PY'\n")
        self.code = script[begin:script.index('\nPY\n', begin)]
        releases = []
        for name in ('macos.zip', 'linux.tar.gz', 'windows.zip'):
            p = self.stage / name; p.write_bytes(('own fixture ' + name).encode())
            releases.append({'file': name, 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()})
        (self.stage / 'desktop-releases.json').write_text(json.dumps({'releases': releases}))
        (self.stage / 'acceptance-2.0.0-alpha.2.html').write_text('own fixture')
        (self.stage / 'index.html').write_text('own fixture')

    def receive(self):
        archive = self.root / 'incoming.tar'
        with tarfile.open(archive, 'w') as tar:
            for p in self.stage.iterdir(): tar.add(p, arcname=p.name)
        return subprocess.run([sys.executable, '-B', '-c', self.code, str(archive), str(self.target)], capture_output=True, text=True)

    def test_receiver_only_installs_finished_files_and_same_bytes_are_noop(self):
        first = self.receive(); self.assertEqual(first.returncode, 0, first.stderr)
        self.assertIn('INSTALLED', first.stdout)
        times = {p.name: p.stat().st_mtime_ns for p in self.target.iterdir()}
        second = self.receive(); self.assertEqual(second.returncode, 0, second.stderr)
        self.assertNotIn('INSTALLED', second.stdout)
        self.assertEqual(times, {p.name: p.stat().st_mtime_ns for p in self.target.iterdir()})

    def test_same_filename_with_changed_bytes_stops_before_metadata_update(self):
        self.assertEqual(self.receive().returncode, 0)
        original = (self.target / 'desktop-releases.json').read_bytes()
        p = self.stage / 'macos.zip'; p.write_bytes(b'different')
        data = json.loads((self.stage / 'desktop-releases.json').read_text())
        data['releases'][0]['sha256'] = hashlib.sha256(p.read_bytes()).hexdigest()
        (self.stage / 'desktop-releases.json').write_text(json.dumps(data))
        failed = self.receive(); self.assertNotEqual(failed.returncode, 0)
        self.assertIn('immutable artifact conflict', failed.stderr)
        self.assertEqual(original, (self.target / 'desktop-releases.json').read_bytes())

    def test_receiver_has_no_build_or_source_sync_command(self):
        for command in ('cargo', 'rustc', 'gradle', 'zigbuild', 'docker build', 'npm', 'pnpm'):
            self.assertNotIn(command, self.code)


if __name__ == '__main__':
    unittest.main()
