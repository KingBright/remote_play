"""Execute the application's real Rust renderer and compare producer bytes."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import build_identity_gate as gate


class IdentityModuleTemplateTests(unittest.TestCase):
    def test_application_and_producer_emit_identical_linux_macos_windows_modules(self):
        rustc=shutil.which('rustc')
        if not rustc:self.skipTest('Rust compiler unavailable')
        source=(Path(__file__).resolve().parents[2]/'app/build.rs').read_text()
        start=source.index('fn generated_module(raw: &[u8]) -> String {')
        end=source.index('\n}',start)+2
        # Compile only this exact std-only production function in an owned tiny
        # fixture. No app, Cargo cache, profile, GUI or target ELF is executed.
        program=source[start:end]+'''\nfn main() {
            use std::io::Read;
            let mut raw=Vec::new();std::io::stdin().read_to_end(&mut raw).unwrap();
            print!("{}",generated_module(&raw));
        }\n'''
        with tempfile.TemporaryDirectory(prefix='weavelo-module-template-') as folder:
            root=Path(folder);path=root/'main.rs';path.write_text(program);exe=root/'template'
            result=subprocess.run([rustc,'--edition=2024',str(path),'-o',str(exe)],capture_output=True,text=True,timeout=30)
            self.assertEqual(result.returncode,0,result.stderr)
            for platform,arch,target in [('linux','x86_64','x86_64-unknown-linux-gnu'),
                                        ('macos','aarch64','aarch64-apple-darwin'),
                                        ('windows','x86_64','x86_64-pc-windows-msvc')]:
                with self.subTest(platform=platform):
                    config=gate.configuration(platform,arch,'2.0.0-alpha.9',target,'release','a'*64,'b'*64)
                    source_identity={'commit':'c'*40,'git_tree':'d'*40,'state':'clean','patch_sha256':None,
                                     'snapshot_sha256':'e'*64,'file_count':1}
                    identity=gate.identity(source_identity,config)
                    raw=json.dumps(identity,sort_keys=True,indent=2).encode()+b'\n'
                    output=subprocess.run([str(exe)],input=raw,capture_output=True,timeout=5)
                    self.assertEqual(output.returncode,0,output.stderr)
                    self.assertEqual(output.stdout,gate.compiled_module(identity))
                    self.assertIn(b'target_os = "linux", unsafe(link_section = ".rpbuild")',output.stdout)
