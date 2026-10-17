import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('linux_link', Path(__file__).parents[1]/'cmake/linux_clang_link.py')
link = importlib.util.module_from_spec(spec)
spec.loader.exec_module(link)


class LinuxLinkTests(unittest.TestCase):
    def test_fixed_target_sysroot_and_real_gcc_soname(self):
        cmd = link.command('/installed/clang', '/verified/sysroot', ['a.o', '-lgcc_s', '-lc', '-o', 'path with spaces'])
        self.assertEqual(cmd[:4], ['/installed/clang', '--target=x86_64-unknown-linux-gnu', '--sysroot=/verified/sysroot', '-fuse-ld=lld'])
        self.assertIn('-Wl,--no-allow-shlib-undefined', cmd)
        self.assertIn('-Wl,-rpath-link,/verified/sysroot/usr/lib64', cmd)
        self.assertEqual(cmd[-5:], ['a.o', '-l:libgcc_s.so.1', '-lc', '-o', 'path with spaces'])

    def test_foreign_or_missing_target_configuration_refused(self):
        for args in (['--target=aarch64-linux-gnu'], ['--target', 'x86_64-linux-gnu'], ['--sysroot=/host'], ['-isysroot/host']):
            with self.subTest(args=args), self.assertRaises(ValueError):
                link.command('/clang', '/sysroot', args)
        for compiler, root in [('clang', '/sysroot'), ('/clang', 'sysroot')]:
            with self.assertRaises(ValueError): link.command(compiler, root, [])
