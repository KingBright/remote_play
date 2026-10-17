import importlib.util
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest

TOOLS = Path(__file__).parents[1]/'cmake'
spec = importlib.util.spec_from_file_location('linux_cmake', TOOLS/'linux_cmake.py')
cmake_entry = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cmake_entry)


class CMakeArchiveTests(unittest.TestCase):
    def test_build_and_script_actions_are_not_configure_commands(self):
        for args in (['--build','cache','--target','install'],['--install','cache'],['-E','env'],['-P','a.cmake'],['--version']):
            self.assertEqual(cmake_entry.command('/installed/cmake','/tools',args), ['/installed/cmake',*args])

    def test_existing_host_archive_cache_is_reconfigured_without_cleaning(self):
        cmake = shutil.which('cmake')
        if not cmake:self.skipTest('CMake not installed')
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            source = root/'source';source.mkdir()
            (source/'CMakeLists.txt').write_text('cmake_minimum_required(VERSION 3.20)\nproject(ArchiveRouting NONE)\nfile(WRITE "${CMAKE_BINARY_DIR}/archive-tools" "${CMAKE_AR}|${CMAKE_RANLIB}")\n')
            env = os.environ.copy();env.pop('WEAVELO_VALIDATION_SYSROOT',None)
            args = ['-S',str(source),'-B',str(root/'build')]
            first = subprocess.run([cmake,*args,'-DCMAKE_AR=/host/ar','-DCMAKE_RANLIB=/host/ranlib'],env=env,capture_output=True,text=True,timeout=20)
            self.assertEqual(first.returncode,0,first.stderr)
            self.assertEqual((root/'build/archive-tools').read_text(),'/host/ar|/host/ranlib')
            result = subprocess.run(cmake_entry.command(str(Path(cmake).resolve()),str(TOOLS.resolve()),args),env=env,capture_output=True,text=True,timeout=20)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual((root/'build/archive-tools').read_text(),str(TOOLS.resolve()/'linux-zig-ar')+'|'+str(TOOLS.resolve()/'linux-zig-ranlib'))
