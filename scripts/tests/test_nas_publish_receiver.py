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

    def receive(self, scope=None):
        archive = self.root / 'incoming.tar'
        with tarfile.open(archive, 'w') as tar:
            for p in self.stage.iterdir(): tar.add(p, arcname=p.name)
        return subprocess.run([sys.executable, '-B', '-c', self.code, str(archive), str(self.target)] + ([scope] if scope else []), capture_output=True, text=True)

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

    def prepare_macos_update(self):
        doc = json.loads((self.stage / 'desktop-releases.json').read_text())
        doc['acceptance_file'] = 'acceptance-2.0.0-alpha.2.html'
        (self.stage / 'desktop-releases.json').write_text(json.dumps(doc))
        (self.stage / 'index.html').write_text('Android unchanged; <a href="' + doc['acceptance_file'] + '">notes</a>')
        self.assertEqual(self.receive().returncode, 0)
        before = (self.target / 'desktop-releases.json').read_bytes()
        (self.stage / 'desktop-releases.before.json').write_bytes(before)
        (self.stage / 'index.before.html').write_bytes((self.target / 'index.html').read_bytes())
        for entry in doc['releases']:
            (self.stage / entry['file']).unlink()
        (self.stage / doc['acceptance_file']).unlink()
        package = self.stage / 'macos-new.zip'; package.write_bytes(b'new own fixture')
        doc['releases'][0] = {'file': package.name, 'version': 'fixture.2', 'size_bytes': package.stat().st_size,
                              'sha256': hashlib.sha256(package.read_bytes()).hexdigest(),
                              'build_identity_expectations': {'commit': 'a' * 40, 'identity_sha256': 'b' * 64,
                                                              'manifest_sha256': 'c' * 64}}
        doc['acceptance_file'] = 'acceptance-macos-new.html'
        (self.stage / doc['acceptance_file']).write_text('Only fixture Mac changed; other platforms retained.')
        (self.stage / 'desktop-releases.json').write_text(json.dumps(doc))
        (self.stage / 'index.html').write_bytes((self.stage / 'index.before.html').read_bytes().replace(
            b'acceptance-2.0.0-alpha.2.html', b'acceptance-macos-new.html'))
        return json.loads(before)

    def test_macos_update_preserves_other_entries_bytes_and_rollback(self):
        before = self.prepare_macos_update()
        (self.target / 'release.json').write_bytes(b'unrelated Android metadata')
        preserved = {p.name: (p.read_bytes(), p.stat().st_mtime_ns) for p in self.target.iterdir()
                     if p.name not in ('desktop-releases.json', 'index.html', '.desktop-publish.lock')}
        result = self.receive('macos'); self.assertEqual(result.returncode, 0, result.stderr)
        after = json.loads((self.target / 'desktop-releases.json').read_text())
        self.assertEqual(after['releases'][1:], before['releases'][1:])
        self.assertEqual(preserved, {name: ((self.target/name).read_bytes(), (self.target/name).stat().st_mtime_ns)
                                     for name in preserved})
        self.assertEqual((self.target / 'macos-new.zip').read_bytes(), b'new own fixture')

    def test_macos_update_rejects_changes_to_another_platform_before_writing(self):
        self.prepare_macos_update()
        original = (self.target / 'desktop-releases.json').read_bytes()
        doc = json.loads((self.stage / 'desktop-releases.json').read_text())
        doc['releases'][1]['version'] = 'invented update'
        (self.stage / 'desktop-releases.json').write_text(json.dumps(doc))
        result = self.receive('macos'); self.assertNotEqual(result.returncode, 0)
        self.assertIn('other platform entries changed', result.stderr)
        self.assertEqual((self.target / 'desktop-releases.json').read_bytes(), original)
        self.assertFalse((self.target / 'macos-new.zip').exists())

    def test_macos_update_rejects_stale_baseline_before_writing(self):
        self.prepare_macos_update()
        original = (self.target / 'desktop-releases.json').read_bytes() + b'\n'
        (self.target / 'desktop-releases.json').write_bytes(original)
        result = self.receive('macos'); self.assertNotEqual(result.returncode, 0)
        self.assertIn('publication changed since read', result.stderr)
        self.assertEqual((self.target / 'desktop-releases.json').read_bytes(), original)
        self.assertFalse((self.target / 'macos-new.zip').exists())

    def test_macos_update_rejects_changes_to_unrelated_index_content(self):
        self.prepare_macos_update()
        original = (self.target / 'index.html').read_bytes()
        (self.stage / 'index.html').write_text('replaced Android download page')
        result = self.receive('macos'); self.assertNotEqual(result.returncode, 0)
        self.assertIn('unrelated index content changed', result.stderr)
        self.assertEqual((self.target / 'index.html').read_bytes(), original)
        self.assertFalse((self.target / 'macos-new.zip').exists())

    def test_macos_update_checks_preserved_package_bytes_before_writing(self):
        self.prepare_macos_update()
        (self.target / 'linux.tar.gz').write_bytes(b'changed behind the manifest')
        original = (self.target / 'desktop-releases.json').read_bytes()
        result = self.receive('macos'); self.assertNotEqual(result.returncode, 0)
        self.assertIn('checksum mismatch', result.stderr)
        self.assertEqual((self.target / 'desktop-releases.json').read_bytes(), original)
        self.assertFalse((self.target / 'macos-new.zip').exists())

    def test_macos_update_preserves_an_existing_conflicting_acceptance_page(self):
        self.prepare_macos_update()
        (self.target / 'acceptance-macos-new.html').write_bytes(b'existing different page')
        result = self.receive('macos'); self.assertNotEqual(result.returncode, 0)
        self.assertIn('immutable acceptance conflict', result.stderr)
        self.assertEqual((self.target / 'acceptance-macos-new.html').read_bytes(), b'existing different page')
        self.assertFalse((self.target / 'macos-new.zip').exists())


if __name__ == '__main__':
    unittest.main()
