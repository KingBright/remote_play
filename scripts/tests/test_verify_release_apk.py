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



class UnsignedPreparationTests(unittest.TestCase):
    def setUp(self):
        import tempfile, zipfile, struct, os
        from unittest.mock import patch
        self.patch = patch
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.sdk = self.root / 'sdk'
        (self.sdk / 'build-tools/35.0.0').mkdir(parents=True)
        (self.root / 'android/app').mkdir(parents=True)
        (self.root / 'android/app/build.gradle.kts').write_text(
            'versionCode = 3\nversionName = "2.0.0-alpha.2"\n')
        kotlin = self.root / 'android/app/src/main/java/fun/hackerlife/weavelo'
        kotlin.mkdir(parents=True)
        (kotlin / 'WeaveloClient.kt').write_text('external fun nativeInit()')
        self.native = bytearray(120)
        self.native[:6] = b'\x7fELF\x02\x01'
        struct.pack_into('<H', self.native, 18, 183)
        struct.pack_into('<Q', self.native, 32, 64)
        struct.pack_into('<HH', self.native, 54, 56, 1)
        struct.pack_into('<I', self.native, 64, 1)
        struct.pack_into('<Q', self.native, 112, 16384)
        staged = self.root / 'android/app/src/main/jniLibs/arm64-v8a'
        staged.mkdir(parents=True)
        self.staged = staged / 'libweavelo_android.so'
        self.staged.write_bytes(self.native)
        self.apk = self.root / 'unsigned.apk'
        with zipfile.ZipFile(self.apk, 'w') as archive:
            archive.writestr('lib/arm64-v8a/libweavelo_android.so', self.native)
        self.addCleanup(patch.stopall)
        patch.dict(os.environ, {'ANDROID_HOME': str(self.sdk)}).start()
        patch.object(module, 'exported_symbols', return_value={
            'Java_fun_hackerlife_weavelo_WeaveloClient_nativeInit'}).start()
        self.run = patch.object(module, 'run', side_effect=self.command).start()

    def command(self, args):
        if 'aapt' in str(args[0]):
            return TREE
        if 'apksigner' in str(args[0]):
            return 'Signer #1 certificate SHA-256 digest: 215aedabf17f40a454ad381d9027772eef9446346cae3db031cac2a5ae51c3a5\n'
        return ''

    def test_unsigned_report_has_no_release_identity(self):
        from types import SimpleNamespace
        with self.patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=1)):
            value = module.verify_unsigned(self.apk, self.root)
        self.assertEqual(value['signing_status'], 'unsigned')
        self.assertIsNone(value['signer_sha256'])
        self.assertEqual(value['version_code'], 3)
        self.assertTrue(value['native_16kb_aligned'])

    def test_unsigned_path_refuses_an_already_signed_apk(self):
        from types import SimpleNamespace
        with self.patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=0)):
            with self.assertRaisesRegex(ValueError, 'signed APK'):
                module.verify_unsigned(self.apk, self.root)

    def test_signed_entry_still_requires_original_fingerprint(self):
        self.run.side_effect = lambda args: TREE if 'aapt' in str(args[0]) else 'Signer #1 certificate SHA-256 digest: ' + '0' * 64
        with self.assertRaisesRegex(ValueError, 'previously published'):
            module.verify(self.apk, self.root)

    def test_signed_entry_never_falls_back_after_signature_failure(self):
        import subprocess
        def failed(args):
            if 'aapt' in str(args[0]):
                return TREE
            raise subprocess.CalledProcessError(1, args)
        self.run.side_effect = failed
        with self.assertRaises(subprocess.CalledProcessError):
            module.verify(self.apk, self.root)

    def test_unsigned_native_hash_and_version_checks_are_retained(self):
        from types import SimpleNamespace
        with self.patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=1)):
            self.staged.write_bytes(b'other native build')
            with self.assertRaisesRegex(ValueError, 'stale'):
                module.verify_unsigned(self.apk, self.root)
            self.staged.write_bytes(self.native)
            (self.root / 'android/app/build.gradle.kts').write_text(
                'versionCode = 4\nversionName = "2.0.0-alpha.2"\n')
            with self.assertRaisesRegex(ValueError, 'version'):
                module.verify_unsigned(self.apk, self.root)


if __name__ == '__main__':
    unittest.main()
