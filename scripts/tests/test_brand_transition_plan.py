import copy
import json
from pathlib import Path
import tempfile
import sys
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import unittest
from unittest.mock import patch
import plan_brand_transition as planner


class CombinedTransitionPlanTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.parent=Path(self.temp.name).resolve()
        self.repo=self.parent/'remote_play';self.repo.mkdir();(self.repo/'.git').mkdir()
        self.target=self.parent/'weavelo';output=self.parent/'package';output.mkdir()
        archive=output/'RemotePlay-macos-arm64.zip';receipt=archive.with_suffix('.verification.json')
        archive.write_bytes(b'old signed artifact fixture')
        proof={'archive':str(archive),'archive_sha256':planner.gate.hash_file(archive),'size_bytes':archive.stat().st_size,
               'signature_verified':True,'bundle_id':'com.remoteplay.unified','certificate_sha1':'f'*40,
               'build_identity_expectations':{'commit':'b'*40,'identity_sha256':'c'*64,'manifest_sha256':'d'*64}}
        receipt.write_text(json.dumps(proof))
        self.signature=lambda p:planner.gate.hash_file(p)
        record={'files':[str(archive),str(receipt)],'signatures':[self.signature(archive),self.signature(receipt)],
                'validation_file':str(receipt),'validation_digest':self.signature(receipt),'validation_kind':'signing','updated':1}
        old_id='delivery-'+planner.sha(str(self.repo).encode())[:24]
        declaration={'schema':1,'mode':'release-only','domain':'delivery','project_root':str(self.repo),'project_id':old_id,
                     'state_file':str(self.repo/planner.METADATA[3]),'output_roots':[str(output)]}
        authority={'schema':'historical-schema','project':'RemotePlay','owner':'RemotePlay','product_source_baseline':'9f2f80',
                   'observed':{'workspace_intake':{'members':{'app':{'package':'remote_play_app','version':'alpha.8','manifest_sha256':'old'}}}},
                   'release_retention':declaration}
        a=json.dumps(authority,indent=4).encode()+b'\n'
        binding={'project_root':str(self.repo),'authority_inventory':{'path':str(self.repo/planner.METADATA[0]),'sha256':planner.sha(a)},
                 'budgets':None,'coverage':{'closure':False},'selected_artifact_refs':[str(output)],'owned_budget_refs':[],
                 'owner_marker_sha256':{},'contract_sha256':{'Cargo.lock':'old observation'}}
        self.data={planner.METADATA[0]:authority,planner.METADATA[1]:binding,
                   planner.METADATA[2]:{'schema':1,'authority':str(self.repo/planner.METADATA[0]),'declaration':declaration},
                   planner.METADATA[3]:dict(declaration,entries=[],archived_entries=[],current_components={planner.COMPONENT:record})}
        for n,d in self.data.items():
            p=self.repo/n;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(a if n==planner.METADATA[0] else json.dumps(d,indent=2).encode()+b'\n')
        p=self.repo/'app/Cargo.toml';p.parent.mkdir();p.write_text('[package]\nname="weavelo_app"\nversion="2.0.0-alpha.9"\n')
        def git(repo,*a):
            if a[0]=='branch':return b'main\n'
            if a[0]=='rev-parse':return b'a'*40+b'\n'
            return b'fixture-status'
        self.patch=patch.object(planner.gate,'git',side_effect=git);self.patch.start()

    def tearDown(self):self.patch.stop();self.temp.cleanup()
    def plan(self):return planner.build_plan(self.repo,self.target,self.signature,{'fixture-helper':'pinned'})['plan']

    def test_read_only_four_file_plan_and_old_current_tuple_preserved(self):
        before={p:p.read_bytes() for p in self.parent.rglob('*') if p.is_file()}
        plan=self.plan();after={p:p.read_bytes() for p in self.parent.rglob('*') if p.is_file()}
        self.assertEqual(before,after);self.assertFalse(plan['ready_for_mutation']);self.assertFalse(plan['official_transition_api_available'])
        self.assertEqual(len(plan['metadata']),4)
        state=json.loads(plan['metadata'][3]['after_root_text'])
        self.assertEqual(state['current_components'],self.data[planner.METADATA[3]]['current_components'])
        self.assertIn('appears once',plan['registry_overlap'])

    def test_observed_bytes_and_historical_versions_hashes_baseline_are_untouched(self):
        row=self.plan()['metadata'][0];before=row['before_text'];after=row['after_root_text']
        b=planner.top_value_span(before,'observed');a=planner.top_value_span(after,'observed')
        self.assertEqual(before[b[0]:b[1]],after[a[0]:a[1]])
        parsed=json.loads(after);self.assertEqual(parsed['observed'],self.data[planner.METADATA[0]]['observed'])
        self.assertEqual(parsed['product_source_baseline'],'9f2f80')

    def test_relocated_id_matches_official_root_derived_rule_and_is_consistent(self):
        plan=self.plan();values=[json.loads(row['after_root_text']) for row in plan['metadata']]
        wanted='delivery-'+planner.sha(str(self.target).encode())[:24]
        self.assertEqual(values[0]['release_retention']['project_id'],wanted)
        self.assertEqual(values[2]['declaration']['project_id'],wanted);self.assertEqual(values[3]['project_id'],wanted)
        self.assertNotEqual(wanted,plan['project_id_transition']['before'])
        self.assertEqual(values[1]['coverage'],self.data[planner.METADATA[1]]['coverage'])

    def test_old_record_or_artifact_drift_rejected(self):
        state=self.data[planner.METADATA[3]];Path(state['current_components'][planner.COMPONENT]['files'][0]).write_bytes(b'foreign')
        with self.assertRaisesRegex(ValueError,'signatures drifted'):self.plan()

    def test_conflicting_new_default_or_target_directory_is_not_adopted(self):
        self.target.mkdir()
        with self.assertRaisesRegex(ValueError,'absent sibling'):self.plan()
        self.target.rmdir();old=Path(self.data[planner.METADATA[3]]['current_components'][planner.COMPONENT]['files'][0])
        old.with_name('Weavelo-macos-arm64.zip').write_bytes(b'unowned')
        with self.assertRaisesRegex(ValueError,'Unowned new default'):self.plan()

    def test_active_delivery_and_unknown_legacy_ownership_require_review(self):
        p=self.repo/planner.METADATA[3];d=copy.deepcopy(self.data[planner.METADATA[3]])
        d['current_update']={'phase':'building'};p.write_text(json.dumps(d))
        with self.assertRaisesRegex(ValueError,'Active/unknown'):self.plan()
        d.pop('current_update');d['entries']=[{'owner':'unknown'}];p.write_text(json.dumps(d))
        with self.assertRaisesRegex(ValueError,'separate explicit mapping'):self.plan()


if __name__=='__main__':unittest.main()
