"""Manifest validation must not depend on framework icon resource lookup."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('verify_apk', Path(__file__).parents[1] / 'verify_release_apk.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
TREE = '''N: android=http://schemas.android.com/apk/res/android
  E: manifest (line=2)
    A: android:versionCode(0x0101021b)=(type 0x10)0x3
    A: android:versionName(0x0101021c)="2.0.0-alpha.2" (Raw: "2.0.0-alpha.2")
    A: package="fun.hackerlife.weavelo" (Raw: "fun.hackerlife.weavelo")
    E: uses-sdk (line=7)
      A: android:minSdkVersion(0x0101020c)=(type 0x10)0x1a
      A: android:targetSdkVersion(0x01010270)=(type 0x10)0x23
    E: application (line=32)
      A: android:icon(0x01010002)=@0x01080093
'''

class ManifestValidationTest(unittest.TestCase):
    def test_framework_icon_does_not_block_release(self):
        value = module.compiled_manifest_attributes(TREE)
        self.assertEqual(value['version_code'], 3)
        self.assertEqual(value['min_sdk'], 26)
        self.assertEqual(value['version'], '2.0.0-alpha.2')
        self.assertFalse(value['debuggable'])

    def test_debug_true_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'debuggable APK'):
            module.compiled_manifest_attributes(TREE + '      A: android:debuggable(0x0101000f)=(type 0x12)0xffffffff\n')

    def test_explicit_debug_false_is_accepted(self):
        self.assertFalse(module.compiled_manifest_attributes(TREE + '      A: android:debuggable(0x0101000f)=(type 0x12)0x0\n')['debuggable'])

    def test_unresolved_debug_resource_fails_closed(self):
        with self.assertRaisesRegex(ValueError, 'Unresolved'):
            module.compiled_manifest_attributes(TREE + '      A: android:debuggable(0x0101000f)=@0x7f010001\n')

    def test_duplicate_application_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'ambiguous'):
            module.compiled_manifest_attributes(TREE + '    E: application (line=60)\n')

    def test_missing_version_is_rejected(self):
        without = '\n'.join(line for line in TREE.splitlines() if 'android:versionName' not in line)
        with self.assertRaisesRegex(ValueError, 'versionName'):
            module.compiled_manifest_attributes(without)

    def test_nested_attributes_do_not_override_root(self):
        value = module.compiled_manifest_attributes(TREE + '      E: activity (line=60)\n        A: package="other.package"\n')
        self.assertEqual(value['package'], 'fun.hackerlife.weavelo')

    def test_duplicate_attribute_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'Duplicate'):
            module.compiled_manifest_attributes(TREE + '      A: android:icon(0x01010002)=@0x01080094\n')

if __name__ == '__main__':
    unittest.main()
