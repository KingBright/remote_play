"""Deferred Linux identity contracts; synthetic ELF bytes, no product execution."""
from copy import deepcopy
import json
import os
from pathlib import Path
import shutil
import struct
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import build_identity_gate as g
import install_native_first_identity as install
import execute_linux_lifecycle as lifecycle


def elf_bytes(identity, machine=62, writable=False, duplicate=False, writable_segment=False):
    raw = g.canonical(identity)+b'\n';offset=128;base=0x400000
    names=b'\0.shstrtab\0.rpbuild\0';name_offset=offset+len(raw)
    table=(name_offset+len(names)+7)&~7
    sections=[(0,0,0,0,0,0,0,0,0,0),
              (1,3,0,0,name_offset,len(names),0,0,1,0),
              (names.index(b'.rpbuild'),1,3 if writable else 2,base+offset,offset,len(raw),0,0,1,0)]
    if duplicate:sections.append(sections[-1])
    ident=b'\x7fELF\x02\x01\x01'+b'\0'*9
    header=struct.pack('<16sHHIQQQIHHHHHH',ident,3,machine,1,0,64,table,0,64,56,1,64,len(sections),1)
    segment=struct.pack('<IIQQQQQQ',1,6 if writable_segment else 4,0,base,base,name_offset,name_offset,4096)
    body=header+segment+b'\0'*(offset-120)+raw+names
    body+=b'\0'*(table-len(body))
    return body+b''.join(struct.pack('<IIQQQQIIQQ',*section) for section in sections)


class LinuxCandidateTests(unittest.TestCase):
    def setUp(self):
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup)
        self.root=Path(temporary.name).resolve();self.binary=self.root/'weavelo'
        source={'commit':'1'*40,'git_tree':'2'*40,'state':'clean','patch_sha256':None,
                'snapshot_sha256':'3'*64,'file_count':1}
        self.environment={'variables':{'PATH':'/fixture/tools'}}
        config=g.configuration('linux','x86_64','2.0.0-alpha.9','x86_64-unknown-linux-gnu',
                               'release','4'*64,g.digest(g.canonical(self.environment)))
        self.identity=g.identity(source,config);self.commit=source['commit'];self.identity_sha=self.identity['identity_sha256']
        self.binary.write_bytes(elf_bytes(self.identity));self.binary.chmod(0o755)
        self.value=g.hash_file(self.binary)
        self.receipt={'schema':2,'kind':'compiled-build-awaiting-native','native_observation':'pending',
            'build_identity':self.identity,'compiler_binary_sha256':self.value,'compiler_binary_bytes':self.binary.stat().st_size,
            'source_unchanged':True,'fresh_target':False,'product_info':None,
            'build_command':g.compiler_command(config),'environment_record':self.environment,
            'build_storage':{'mode':'shared-incremental','source_directory':'/original/source','target_directory':'/original/cache',
                             'observation_binding':{'path':'/original/source/.rust-storage/binding.json','sha256':'5'*64}},
            'static_identity':g.linux_elf_identity(self.binary,self.identity,self.value)}
        self.receipt_path=self.root/'compiled.json';self.receipt_sha=g.write_new(self.receipt_path,self.receipt)
        self.manifest=g.candidate_manifest(self.binary,self.receipt,self.receipt_sha,self.commit,self.identity_sha)
        self.manifest_path=self.root/'candidate-manifest.json';self.manifest_sha=g.write_new(self.manifest_path,self.manifest)
        self.info={'schema':1,'product':'Weavelo','version':'2.0.0-alpha.9','platform':'linux','architecture':'x86_64',
            'default_gui':g.RENDERER,'original_gui_compiled':True,'native_video_compiled':True,'build_identity':self.identity,
            'separated_lifecycle':True,'frontend_entry':'--gui','background_entry':'--background-service'}

    def observe_fixture(self):
        with patch.object(g.sys,'platform','linux'),patch('platform.machine',return_value='x86_64'),patch.object(g,'product_info',return_value=self.info) as metadata:
            result=g.observe_compiled_linux(self.binary,self.receipt,self.receipt_sha,self.manifest,self.manifest_sha,self.commit,self.identity_sha)
            metadata.assert_called_once_with(self.binary,self.identity,self.value)
        return result

    def test_static_candidate_has_exact_identity_and_never_claims_execution(self):
        self.assertEqual(self.manifest['kind'],'weavelo-compiled-release-candidate')
        self.assertEqual(self.manifest['native_observation'],'pending')
        self.assertIsNone(self.manifest['product_info'])
        self.assertFalse(self.receipt['static_identity']['runtime_observed'])
        with patch.object(g,'product_info') as runtime:
            self.assertEqual(g.candidate_manifest(self.binary,self.receipt,self.receipt_sha,self.commit,self.identity_sha),self.manifest)
            runtime.assert_not_called()

    def test_original_compiler_deferred_stage_never_calls_product_metadata(self):
        repo=self.root/'source';repo.mkdir();target=self.root/'cache';target.mkdir()
        output=target/self.identity['configuration']['target']/'release/weavelo'
        output.parent.mkdir(parents=True);output.write_bytes(self.binary.read_bytes())
        binding=repo/'binding.json';binding.write_text('{}')
        with patch.dict(os.environ,{'WEAVELO_VALIDATION_SYSROOT':'/verified'}), \
                patch.object(g,'shared_target',return_value=target), \
                patch.object(g,'source_snapshot',return_value=self.identity['source']), \
                patch.object(g,'toolchain_digest',return_value='4'*64), \
                patch.object(g,'environment_digest',return_value=self.identity['configuration']['environment_sha256']), \
                patch.object(g,'compiler_environment_record',return_value=self.environment), \
                patch.object(g,'build_environment',return_value={'PATH':'/usr/bin'}), \
                patch.object(g.BuildInputs,'run',return_value=0) as compiler, \
                patch.object(g,'product_info') as metadata:
            receipt=g.observed_build(repo,self.identity,target,lifecycle_binding=binding,defer_linux_observation=True)
            self.assertEqual(receipt['kind'],'compiled-build-awaiting-native')
            g.validate_compiled_linux(receipt,self.commit,self.identity_sha)
            metadata.assert_not_called()
            self.assertEqual(compiler.call_args.args[0],g.compiler_command(self.identity['configuration']))
        self.assertFalse((target/'.weavelo-build-identity.json').exists())
        self.assertFalse((target/'.weavelo-build-identity.rs').exists())

    def test_original_compiler_failure_never_returns_pending_or_observed_record(self):
        repo=self.root/'source';repo.mkdir();target=self.root/'cache';target.mkdir()
        binding=repo/'binding.json';binding.write_text('{}')
        with patch.dict(os.environ,{'WEAVELO_VALIDATION_SYSROOT':'/verified'}), \
                patch.object(g,'shared_target',return_value=target), \
                patch.object(g,'source_snapshot',return_value=self.identity['source']), \
                patch.object(g,'toolchain_digest',return_value='4'*64), \
                patch.object(g,'environment_digest',return_value=self.identity['configuration']['environment_sha256']), \
                patch.object(g,'build_environment',return_value={'PATH':'/usr/bin'}), \
                patch.object(g.BuildInputs,'run',return_value=101),patch.object(g,'product_info') as metadata:
            with self.assertRaises(g.IdentityRejected):
                g.observed_build(repo,self.identity,target,lifecycle_binding=binding,defer_linux_observation=True)
            metadata.assert_not_called()
        self.assertFalse((target/'.weavelo-build-identity.json').exists())

    def test_development_or_foreign_elf_cannot_gain_identity_receipt(self):
        for raw in (b'Mach-O development bytes',elf_bytes(self.identity,machine=183),
                    elf_bytes(self.identity,writable=True),elf_bytes(self.identity,duplicate=True),
                    elf_bytes(self.identity,writable_segment=True)):
            self.binary.write_bytes(raw)
            with self.assertRaises(g.IdentityRejected):g.linux_elf_identity(self.binary,self.identity,g.hash_file(self.binary))

    def test_modified_identity_or_binary_hash_refused(self):
        changed=deepcopy(self.identity);changed['source']['commit']='9'*40
        self.binary.write_bytes(elf_bytes(changed))
        with self.assertRaises(g.IdentityRejected):g.linux_elf_identity(self.binary,self.identity,g.hash_file(self.binary))
        with self.assertRaises(g.IdentityRejected):g.linux_elf_identity(self.binary,self.identity,self.value)

    def test_compiler_recipe_environment_source_and_runtime_claim_drift_refused(self):
        for key,value in [('build_command',['cargo','build']),('environment_record',{}),
                          ('source_unchanged',False),('product_info',self.info),('native_observation','verified')]:
            receipt=dict(self.receipt,**{key:value})
            with self.subTest(key=key),self.assertRaises(g.IdentityRejected):g.validate_compiled_linux(receipt,self.commit,self.identity_sha)

    def test_pending_manifest_and_receipt_rejected_by_existing_release_and_install_gates(self):
        with self.assertRaises(g.IdentityRejected):g.manifest_document(self.manifest_path.read_bytes(),self.manifest_sha,self.commit,self.identity_sha)
        with patch.object(g,'product_info') as runtime:
            with self.assertRaises(g.IdentityRejected):g.seal_tree(self.root,self.receipt,self.commit,self.identity_sha)
            runtime.assert_not_called()
        args=self.arguments(self.receipt_path,self.receipt_sha,self.manifest_path,self.manifest_sha)
        with self.assertRaises(RuntimeError):install.reviewed_linux_binding(g,args)

    def test_observation_requires_real_matching_linux_before_execution(self):
        for os_name,machine in [('darwin','x86_64'),('linux','aarch64')]:
            with self.subTest(os=os_name),patch.object(g.sys,'platform',os_name),patch('platform.machine',return_value=machine),patch.object(g,'product_info') as runtime:
                with self.assertRaises(g.IdentityRejected):g.observe_compiled_linux(self.binary,self.receipt,self.receipt_sha,self.manifest,self.manifest_sha,self.commit,self.identity_sha)
                runtime.assert_not_called()

    def test_manifest_drift_never_executes_candidate(self):
        manifest=dict(self.manifest,binary_sha256='f'*64)
        with patch.object(g.sys,'platform','linux'),patch('platform.machine',return_value='x86_64'),patch.object(g,'product_info') as runtime:
            with self.assertRaises(g.IdentityRejected):g.observe_compiled_linux(self.binary,self.receipt,self.receipt_sha,manifest,self.manifest_sha,self.commit,self.identity_sha)
            runtime.assert_not_called()

    def test_native_observation_binds_both_original_document_pins(self):
        observed=self.observe_fixture()
        self.assertEqual(observed['kind'],'observed-build')
        self.assertEqual(observed['native_observation'],'verified')
        self.assertEqual(observed['deferred_compiler_receipt_sha256'],self.receipt_sha)
        self.assertEqual(observed['candidate_manifest_sha256'],self.manifest_sha)
        self.assertEqual(observed['build_storage']['source_directory'],'/original/source')

    def test_actual_metadata_mismatch_cannot_mint_observed_receipt(self):
        with patch.object(g.sys,'platform','linux'),patch('platform.machine',return_value='x86_64'),patch.object(g,'product_info',return_value=dict(self.info,build_identity=None)):
            with self.assertRaises(g.IdentityRejected):g.observe_compiled_linux(self.binary,self.receipt,self.receipt_sha,self.manifest,self.manifest_sha,self.commit,self.identity_sha)

    def arguments(self,receipt,receipt_sha,manifest,manifest_sha):
        return SimpleNamespace(build_receipt=receipt,build_receipt_sha256=receipt_sha,release_manifest=manifest,
            manifest_sha256=manifest_sha,expected_commit=self.commit,identity_sha256=self.identity_sha,
            compiler_binary=self.binary,old_binary_sha256='a'*64)

    def installed_fixture(self):
        observed=self.observe_fixture();path=self.root/'observed.json';value=g.write_new(path,observed)
        stage=self.root/'stage';app=stage/'Weavelo';app.mkdir(parents=True);shutil.copyfile(self.binary,app/'weavelo')
        with patch.object(g,'product_info',return_value=self.info):sealed=g.seal_tree(stage,observed,self.commit,self.identity_sha)
        return observed,self.arguments(path,value,stage/g.MANIFEST,sealed['manifest_sha256'])

    def test_new_source_installer_requires_receipt_manifest_and_real_elf_binding(self):
        observed,args=self.installed_fixture();binding=install.reviewed_linux_binding(g,args)
        self.assertEqual(binding['commit'],self.commit)
        self.assertEqual(binding['pin']['binary'],self.value)
        changed=g.read_json(args.release_manifest);changed['build_receipt_canonical_sha256']='e'*64
        wrong=self.root/'wrong-manifest.json';value=g.write_new(wrong,changed)
        args.release_manifest=wrong;args.manifest_sha256=value
        with self.assertRaises(RuntimeError):install.reviewed_linux_binding(g,args)

    def test_no_linux_installer_fallback_to_historical_source(self):
        self.assertFalse(hasattr(install,'PINS'))
        with self.assertRaises(RuntimeError):install.reviewed_linux_binding(g,None)

    def test_lifecycle_uses_original_journal_binding_for_any_reviewed_source(self):
        observed,args=self.installed_fixture()
        original={'platform':'linux','source_commit':self.commit,'new_binary_sha256':self.value,
                  'receipt_sha256':args.build_receipt_sha256,'build_identity_sha256':self.identity_sha}
        journal={'phase':'installed_and_running_verified','plan':original,'plan_sha256':g.digest(g.canonical(original))}
        plan={'source_commit':self.commit,'binary_sha256':self.value}
        self.assertEqual(lifecycle.installed_journal_identity(g,journal,plan,args.build_receipt),self.identity)
        for changed in (dict(plan,source_commit='f'*40),dict(plan,binary_sha256='f'*64)):
            with self.assertRaises(lifecycle.policy.LifecyclePlanRejected):lifecycle.installed_journal_identity(g,journal,changed,args.build_receipt)
        journal['plan_sha256']='f'*64
        with self.assertRaises(lifecycle.policy.LifecyclePlanRejected):lifecycle.installed_journal_identity(g,journal,plan,args.build_receipt)
