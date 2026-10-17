import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location('zigtool',Path(__file__).parents[1]/'cmake/linux_zig_tool.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)


class CompilerEntryTests(unittest.TestCase):
    def test_cmake_compiler_is_executable_with_fixed_target_not_a_shell_string(self):
        args=m.command('/installed/zig','cc',['-c','input with spaces.c','--target=x86_64-unknown-linux-gnu'])
        self.assertEqual(args,['/installed/zig','cc','-target','x86_64-linux-gnu','-c','input with spaces.c','--target=x86_64-linux-gnu'])

    def test_cpp_and_archive_modes_stay_fixed(self):
        self.assertEqual(m.command('/installed/zig','c++',['-c','a.cpp'])[:4],['/installed/zig','c++','-target','x86_64-linux-gnu'])
        self.assertEqual(m.command('/installed/zig','ar',['crs','a.a','a.o']),['/installed/zig','ar','crs','a.a','a.o'])

    def test_foreign_targets_modes_and_missing_arguments_rejected(self):
        for mode,args in [('cc',['--target=aarch64-linux-gnu']),('cc',['-target','aarch64-linux-gnu']),('cc',['-target']),('ar',['--target=x86_64-linux-gnu']),('shell',[])]:
            with self.subTest(mode=mode,args=args),self.assertRaises(ValueError):m.command('/installed/zig',mode,args)
        with self.assertRaises(ValueError):m.command('zig','cc',[])
