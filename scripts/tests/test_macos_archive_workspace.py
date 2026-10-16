"""Tiny archive fixtures only: no real product, authority, signing, or installation."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import macos_release_guard as guard


class ArchiveWorkspaceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='rp-archive-workspace-fixture-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.aux = self.root / '.owned-package.pending'
        self.aux.mkdir()
        self.archive = self.aux / 'candidate.zip'
        with zipfile.ZipFile(self.archive, 'w') as output:
            output.writestr('Weavelo.app/Contents/MacOS/weavelo', b'tiny fixture')
        self.policy = guard.Policy('com.example.fixture', 'weavelo', 'Weavelo', 'a' * 40)
        self.report = dict(bundle_id=self.policy.bundle_id, certificate_sha1=self.policy.certificate_sha1,
                           designated_requirement=self.policy.requirement, version='1.0.0', build='1',
                           executable_sha256='b' * 64, info_sha256='c' * 64, signature_verified=True)

    def test_owned_candidate_uses_fixed_aux_child_and_preserves_validation_arguments(self):
        runner, check = object(), lambda: None
        with patch.object(guard.tempfile, 'TemporaryDirectory', side_effect=AssertionError('system temporary root forbidden')), \
                patch.object(guard, 'verify_app', return_value=dict(self.report)) as verify:
            with guard.verified_archive(self.archive, self.policy, runner=runner, check=check,
                                        owned_workspace=self.aux) as (app, report):
                self.assertEqual(app, self.aux / '.archive-check/Weavelo.app')
                self.assertEqual((app / 'Contents/MacOS/weavelo').read_bytes(), b'tiny fixture')
                self.assertEqual(report['archive_sha256'], guard.sha256(self.archive))
                verify.assert_called_once_with(app, self.policy, runner=runner, check=check)
        self.assertEqual(list(self.aux.iterdir()), [self.archive])

    def test_standalone_readonly_interface_retains_temporary_cleanup(self):
        with patch.object(guard, 'verify_app', return_value=dict(self.report)):
            with guard.verified_archive(self.archive, self.policy) as (app, report):
                self.assertFalse(app.is_relative_to(self.aux))
                self.assertTrue(app.parent.name.startswith('rp-release-check-'))
                self.assertTrue(report['signature_verified'])
            self.assertFalse(app.exists())
        self.assertTrue(self.archive.is_file())

    def test_verification_failure_removes_only_its_new_child(self):
        sibling = self.aux / 'Weavelo.app'; sibling.mkdir()
        (sibling / 'untouched').write_bytes(b'current package input')
        with patch.object(guard, 'verify_app', side_effect=guard.ReleaseRejected('signature rejected')):
            with self.assertRaisesRegex(guard.ReleaseRejected, 'signature rejected'):
                with guard.verified_archive(self.archive, self.policy, owned_workspace=self.aux): pass
        self.assertFalse((self.aux / '.archive-check').exists())
        self.assertEqual((sibling / 'untouched').read_bytes(), b'current package input')
        self.assertTrue(self.archive.exists())

    def test_existing_verification_child_is_not_reused_or_removed(self):
        child = self.aux / '.archive-check'; child.mkdir()
        evidence = child / 'partial'; evidence.write_bytes(b'preserve')
        with self.assertRaisesRegex(guard.ReleaseRejected, 'already exists'):
            with guard.verified_archive(self.archive, self.policy, owned_workspace=self.aux): pass
        self.assertEqual(evidence.read_bytes(), b'preserve')

    def test_linked_or_different_owner_workspace_is_refused(self):
        link = self.root / 'alias'; link.symlink_to(self.aux, target_is_directory=True)
        with self.assertRaises(guard.ReleaseRejected):
            with guard.verified_archive(self.archive, self.policy, owned_workspace=link): pass
        uid = os.getuid()
        with patch.object(guard.os, 'getuid', return_value=uid + 1), self.assertRaises(guard.ReleaseRejected):
            with guard.verified_archive(self.archive, self.policy, owned_workspace=self.aux): pass
        self.assertFalse((self.aux / '.archive-check').exists())

    def test_archive_path_and_collision_gates_remain_in_force(self):
        for entries in [[('../outside', b'x')], [('Weavelo.app/A', b'x'), ('weavelo.app/a', b'y')]]:
            with zipfile.ZipFile(self.archive, 'w') as output:
                for name, data in entries: output.writestr(name, data)
            with patch.object(guard, 'verify_app') as verify, self.assertRaises(guard.ReleaseRejected):
                with guard.verified_archive(self.archive, self.policy, owned_workspace=self.aux): pass
            verify.assert_not_called()
            self.assertFalse((self.aux / '.archive-check').exists())

    def test_abrupt_exit_leaves_extraction_only_beneath_owned_aux(self):
        script = ('import os,sys; from pathlib import Path; '
                  f'sys.path.insert(0,{str(Path(guard.__file__).parent)!r}); import macos_release_guard as g\n'
                  'g.tempfile.TemporaryDirectory=lambda *a,**k: (_ for _ in ()).throw(AssertionError("system tmp forbidden"))\n'
                  'g.verify_app=lambda *a,**k: {}\n'
                  'p=g.Policy("fixture","weavelo","Weavelo","a"*40)\n'
                  f'with g.verified_archive(Path({str(self.archive)!r}),p,owned_workspace=Path({str(self.aux)!r})) as (app,report):\n'
                  ' os._exit(23)\n')
        result = subprocess.run([sys.executable, '-B', '-c', script], capture_output=True, text=True)
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertEqual((self.aux / '.archive-check/Weavelo.app/Contents/MacOS/weavelo').read_bytes(), b'tiny fixture')
        self.assertEqual({path.name for path in self.root.iterdir()}, {self.aux.name})
        # The original aux journal owns this entire subtree; this test never
        # calls a real project's recovery or scans unrelated system tmp paths.

    def test_producer_package_routes_roundtrip_into_existing_owned_aux(self):
        self.archive.unlink()
        binary = self.root / 'binary'; binary.write_bytes(b'fixture binary')
        output = self.root / 'output'; output.mkdir()
        archive = output / '.Weavelo-macos-arm64.zip.pending'
        receipt = output / '.Weavelo-macos-arm64.verification.json.pending'
        calls = []
        def command(args, *unused):
            calls.append(args)
            if args[0] == '/usr/bin/ditto':
                app, target = map(Path, args[-2:])
                with zipfile.ZipFile(target, 'w') as z:
                    for source in app.rglob('*'):
                        if source.is_file(): z.write(source, source.relative_to(app.parent))
            return ''
        verified = []
        def verify(app, policy, **kwargs):
            verified.append(app)
            return dict(self.report)
        with patch.object(guard, 'preflight', return_value='A' * 40), patch.object(guard, 'run', side_effect=command), \
                patch.object(guard, 'verify_app', side_effect=verify), \
                patch.object(guard.tempfile, 'TemporaryDirectory', side_effect=AssertionError('system tmp forbidden')):
            result = guard.package(binary, output, '1.0.0', '1', self.policy,
                                   archive=archive, receipt=receipt, workspace=self.aux)
        self.assertEqual(verified, [self.aux / 'Weavelo.app', self.aux / '.archive-check/Weavelo.app'])
        self.assertEqual(calls[0][0], '/usr/bin/codesign')
        self.assertTrue(archive.is_file()); self.assertTrue(receipt.is_file())
        self.assertFalse((self.aux / '.archive-check').exists())
        self.assertEqual(result['archive_sha256'], guard.sha256(archive))


if __name__ == '__main__':
    unittest.main()
