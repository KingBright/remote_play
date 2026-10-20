"""Execute the application's real Rust renderer and compare producer bytes."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import build_identity_gate as gate


def installed_rustc():
    """Resolve the repository pin without letting a rustup proxy install it."""
    repo = Path(__file__).resolve().parents[2]
    channel = tomllib.loads((repo / 'rust-toolchain.toml').read_text())['toolchain']['channel']
    rustup = shutil.which('rustup')
    if not rustup:
        raise unittest.SkipTest('rustup unavailable; pinned Rust fixture not run')
    result = subprocess.run([rustup, 'which', '--toolchain', channel, 'rustc'],
                            cwd=repo, env=dict(os.environ, RUSTUP_AUTO_INSTALL='0'),
                            capture_output=True, text=True, timeout=10)
    if result.returncode:
        raise unittest.SkipTest(f'Rust {channel} unavailable; install it explicitly before running this fixture')
    compiler = Path(result.stdout.strip())
    if not compiler.is_absolute() or not compiler.is_file():
        raise RuntimeError('rustup returned an unavailable compiler path')
    return str(compiler)


class CompilerSelectionTests(unittest.TestCase):
    def test_missing_rustup_skips_without_invoking_a_proxy(self):
        with patch.object(shutil, 'which', return_value=None), patch.object(subprocess, 'run') as run:
            with self.assertRaises(unittest.SkipTest):
                installed_rustc()
        run.assert_not_called()

    def test_missing_pin_never_falls_back_or_installs(self):
        result = subprocess.CompletedProcess([], 1, '', 'toolchain not installed')
        with patch.object(shutil, 'which', return_value='/fixture/rustup'), \
             patch.object(subprocess, 'run', return_value=result) as run, \
             patch.dict(os.environ, {'RUSTUP_AUTO_INSTALL': '1'}):
            with self.assertRaises(unittest.SkipTest):
                installed_rustc()
            self.assertEqual(os.environ['RUSTUP_AUTO_INSTALL'], '1')
        run.assert_called_once()
        self.assertEqual(run.call_args.args[0][1:3], ['which', '--toolchain'])
        self.assertEqual(run.call_args.kwargs['env']['RUSTUP_AUTO_INSTALL'], '0')

    def test_installed_pin_resolves_to_direct_compiler(self):
        with tempfile.TemporaryDirectory(prefix='weavelo-compiler-selection-') as folder:
            compiler = Path(folder).resolve() / 'rustc'
            compiler.write_bytes(b'synthetic compiler path; never executed')
            result = subprocess.CompletedProcess([], 0, str(compiler) + '\n', '')
            with patch.object(shutil, 'which', return_value='/fixture/rustup'), \
                 patch.object(subprocess, 'run', return_value=result) as run:
                self.assertEqual(installed_rustc(), str(compiler))
            repo = Path(__file__).resolve().parents[2]
            channel = tomllib.loads((repo / 'rust-toolchain.toml').read_text())['toolchain']['channel']
            self.assertEqual(run.call_args.args[0], ['/fixture/rustup', 'which', '--toolchain', channel, 'rustc'])
            self.assertEqual(run.call_args.kwargs['cwd'], repo)
            run.assert_called_once()

    def test_invalid_resolver_output_fails_without_executing_it(self):
        result = subprocess.CompletedProcess([], 0, 'relative/compiler\n', '')
        with patch.object(shutil, 'which', return_value='/fixture/rustup'), \
             patch.object(subprocess, 'run', return_value=result) as run:
            with self.assertRaisesRegex(RuntimeError, 'compiler path'):
                installed_rustc()
        run.assert_called_once()


class IdentityModuleTemplateTests(unittest.TestCase):
    def test_application_and_producer_emit_identical_linux_macos_windows_modules(self):
        rustc=installed_rustc()
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
            root=Path(folder);path=root/'main.rs';path.write_text(program)
            exe=root/('template.exe' if os.name == 'nt' else 'template')
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
