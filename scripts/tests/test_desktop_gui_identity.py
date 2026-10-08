import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace
import json

path = Path(__file__).resolve().parents[1] / 'verify_desktop_gui.py'
spec = importlib.util.spec_from_file_location('desktop_gui_gate', path)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

class DesktopGuiIdentityTests(unittest.TestCase):
    def info(self):
        return dict(schema=1, product='RemotePlay', version='2.0.0-alpha.8',
                    platform='linux', architecture='x86_64', default_gui='restored-original-gpui',
                    original_gui_compiled=True, native_video_compiled=True)
    def test_original_with_native_renderer_is_accepted(self):
        info=self.info()
        self.assertNotIn('diagnostic_gui_requires_opt_in', info)
        gate.validate(info, 'linux', '2.0.0-alpha.8')
    def test_release_rejects_legacy_diagnostic_metadata(self):
        data=self.info();data['diagnostic_gui_requires_opt_in']=True
        with self.assertRaises(gate.GuiReleaseRejected):gate.validate(data,'linux','2.0.0-alpha.8')
    def test_diagnostic_gui_cannot_be_relabelled_as_product(self):
        data=self.info();data['default_gui']='egui-diagnostic'
        with self.assertRaises(gate.GuiReleaseRejected):gate.validate(data,'linux','2.0.0-alpha.8')
    def test_missing_feature_or_native_adapter_rejected(self):
        for key in ('original_gui_compiled','native_video_compiled'):
            data=self.info();data[key]=False
            with self.assertRaises(gate.GuiReleaseRejected):gate.validate(data,'linux','2.0.0-alpha.8')
    def test_platform_and_version_must_match(self):
        for platform,version in [('windows','2.0.0-alpha.8'),('linux','2.0.0-alpha.7')]:
            with self.assertRaises(gate.GuiReleaseRejected):gate.validate(self.info(),platform,version)
    def test_string_boolean_is_not_a_capability(self):
        data=self.info();data['native_video_compiled']='true'
        with self.assertRaises(gate.GuiReleaseRejected):gate.validate(data,'linux','2.0.0-alpha.8')
    def test_missing_old_binary_report_is_not_success(self):
        with tempfile.TemporaryDirectory() as tmp:
            binary=Path(tmp)/'remote_play';binary.write_bytes(b'fixture')
            with patch.object(gate.subprocess,'run',return_value=SimpleNamespace(returncode=0,stdout='old program started')):
                with self.assertRaises(gate.GuiReleaseRejected):gate.verify(binary,'linux','2.0.0-alpha.8')
    def test_identity_check_does_not_claim_visual_or_functional_acceptance(self):
        with tempfile.TemporaryDirectory() as tmp:
            binary=Path(tmp)/'remote_play';binary.write_bytes(b'fixture')
            with patch.object(gate.subprocess,'run',return_value=SimpleNamespace(returncode=0,stdout=json.dumps(self.info()))) as execute:
                result=gate.verify(binary,'linux','2.0.0-alpha.8')
                self.assertEqual(execute.call_args.args[0],[str(binary.resolve()),'--product-info-json'])
                self.assertEqual(result['visual_acceptance'],'not_evaluated')
                self.assertFalse(result['network_started'])
