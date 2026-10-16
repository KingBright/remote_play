"""Packaging preflight never grants policy or starts a compiler through a refusal."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_windows_current as windows

class WindowsPackagingPreflightTests(unittest.TestCase):
    def report(self, policy='RemoteSigned', zone=0, signature='NotSigned'):
        return dict(effective=policy, scripts=[dict(path=name, signature=signature, zone=zone)
                    for name in ('package_windows_unified.ps1','verify_desktop_gui.ps1')])

    def test_no_inherited_policy_is_not_a_policy_grant(self):
        original={'PATH':'existing'}
        self.assertEqual(windows.packaging_environment(original,{}),original)
        self.assertNotIn('PSExecutionPolicyPreference',original)

    def test_preserves_only_previously_chosen_process_policy(self):
        original={'PATH':'existing'}; inherited={'PSExecutionPolicyPreference':'RemoteSigned'}
        result=windows.packaging_environment(original,inherited)
        self.assertEqual(result,dict(original,PSExecutionPolicyPreference='RemoteSigned'))
        self.assertEqual(original,{'PATH':'existing'})
        self.assertEqual(inherited,{'PSExecutionPolicyPreference':'RemoteSigned'})

    def test_restricted_is_rejected_without_a_fallback(self):
        with self.assertRaisesRegex(windows.gate.IdentityRejected,'Restricted'):
            windows.validate_packaging_policy(self.report('Restricted'))

    def test_signed_policy_and_internet_zone_remain_enforced(self):
        for report in (self.report('AllSigned'),self.report(zone=3)):
            with self.assertRaises(windows.gate.IdentityRejected):windows.validate_packaging_policy(report)
        windows.validate_packaging_policy(self.report(zone=3,signature='Valid'))
        windows.validate_packaging_policy(self.report())

    def test_incomplete_observation_is_not_success(self):
        for report in ({'effective':'RemoteSigned','scripts':[]},self.report(zone='0')):
            with self.assertRaises(windows.gate.IdentityRejected):windows.validate_packaging_policy(report)

    def test_policy_refusal_occurs_before_source_scan_or_compiler(self):
        with patch.object(sys,'argv',['build_windows_current.py','--expected-commit','a'*40,'--ffmpeg','C:/private/ffmpeg.exe']), \
             patch.object(sys,'platform','win32'), \
             patch.object(windows.gate,'git',return_value=b'main'), \
             patch.object(windows.gate,'build_environment',return_value={}), \
             patch.object(windows,'packaging_preflight',side_effect=windows.gate.IdentityRejected('Restricted')), \
             patch.object(windows.gate,'source_snapshot') as source, \
             patch.object(windows.subprocess,'Popen') as compiler:
            with self.assertRaisesRegex(windows.gate.IdentityRejected,'Restricted'):windows.main()
            source.assert_not_called();compiler.assert_not_called()

    def test_probe_reads_policy_and_metadata_without_running_script(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve();(root/'scripts').mkdir()
            for name in ('package_windows_unified.ps1','verify_desktop_gui.ps1'):(root/'scripts'/name).write_text('param()')
            response=subprocess.CompletedProcess([],0,json.dumps(self.report()),'')
            with patch.object(windows.subprocess,'run',return_value=response) as run:
                windows.packaging_preflight(root,{'PATH':'existing'})
            argv=run.call_args.args[0]
            self.assertEqual(argv[:3],['powershell.exe','-NoProfile','-Command'])
            for forbidden in ('Set-ExecutionPolicy','Unblock-File'):
                self.assertNotIn(forbidden,argv[3])
            self.assertNotIn('-ExecutionPolicy',argv)
            self.assertNotIn('-File',argv)
