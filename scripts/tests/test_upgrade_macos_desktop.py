"""Owned disposable upgrade fixtures. No live service, signing or permission changes."""
import contextlib
import copy
import hashlib
import json
import os
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import upgrade_macos_desktop as upgrade
NativeControlOriginal = upgrade.NativeControl
import desktop_identity as desktop
import macos_release_guard as guard


class UpgradeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name).resolve() / 'home'; self.home.mkdir(mode=0o700)
        self.selected = desktop.paths(self.home)
        for path in (self.selected['app'], self.selected['profile'], self.selected['state'],
                     self.selected['plist'].parent):
            path.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.old_app = self.home / 'Applications/Weavelo.app'
        self.old_app.mkdir(); (self.old_app/'sentinel').write_bytes(b'retained-client')
        self.candidate = self.home/'candidate/Weavelo.app'
        self.make_app(self.selected['app'], b'old-image')
        self.make_app(self.candidate, b'new-image')
        for name in ('mesh.conf', 'mesh.secret'):
            path = self.selected['profile']/name
            path.write_bytes(b'synthetic-private-fixture'); path.chmod(0o600)
        self.selected['plist'].write_bytes(plistlib.dumps(desktop.launch_agent(self.home)))
        self.selected['plist'].chmod(0o600)
        self.registry = self.selected['state']/'install.json'
        self.record = {'application_id': desktop.identity()['application_id'], 'applied': True,
                       'phase': 'committed_not_started', 'paths': {k:str(v) for k,v in self.selected.items()},
                       'source': {'commit':'a'*40, 'identity_sha256':'b'*64, 'manifest_sha256':'c'*64},
                       'user_notes': {'keep': 'user content'}}
        self.registry.write_text(json.dumps(self.record)); self.registry.chmod(0o600)
        self.archive = self.home/'candidate.zip'; self.archive.write_bytes(b'synthetic-archive')
        self.pins = {'commit':'d'*40,'identity_sha256':'e'*64,'manifest_sha256':'f'*64}
        self.before_hook = self.stop_hook = self.start_hook = None
        self.calls = []
        owner = self
        class Control:
            def __init__(self, selected): self.selected=selected
            def before(self, expected):
                owner.calls.append('before')
                if owner.before_hook: owner.before_hook()
                return {'background': {'pid':101}, 'gui':{'pid':102}, 'loaded':True}
            def stop(self, before):
                owner.calls.append('stop')
                if owner.stop_hook: owner.stop_hook()
            def start(self, before, expected):
                owner.calls.append('start')
                if owner.start_hook: owner.start_hook()
                return {'background_pid':201,'frontend_pid':202,'native_pairing_cli_verified':True,
                        'executable_sha256':expected}
        self.control = Control
        for obj,name,value in ((guard,'verify_app',self.signed),
                               (guard,'verified_archive',self.archive_view),
                               (upgrade.fresh,'verified_product',lambda *a: {'desktop_identity':desktop.identity()}),
                               (upgrade,'product_current',lambda *a: {'desktop_identity':desktop.identity()}),
                               (upgrade,'NativeControl',Control)):
            p=patch.object(obj,name,value);p.start();self.addCleanup(p.stop)

    def make_app(self, path, value):
        (path/'Contents/MacOS').mkdir(parents=True)
        (path/'Contents/MacOS/weavelo').write_bytes(value)
        (path/'Contents/Info.plist').write_bytes(value+b'-info')

    def signed(self, path, policy, **kw):
        image=(path/'Contents/MacOS/weavelo').read_bytes()
        return {'version':'2.0.0-alpha.9','build':'20261022.10' if image==b'new-image' else '20261022.9',
                'bundle_id':policy.bundle_id,'certificate_sha1':policy.certificate_sha1,
                'designated_requirement':policy.requirement,
                'executable_sha256':hashlib.sha256(image).hexdigest(),
                'info_sha256':hashlib.sha256(image+b'-info').hexdigest(),'signature_verified':True}

    @contextlib.contextmanager
    def archive_view(self,*args,**kw):
        yield self.candidate,self.signed(self.candidate,guard.load_policy(guard.DESKTOP_POLICY_FILE))

    def current(self):
        return upgrade.inspect(self.home)['pins']

    def apply(self, pins=None, resume=None):
        return upgrade.apply(self.archive,self.home,self.pins,upgrade.gate.hash_file(self.archive),
                             self.current() if pins is None else pins, resume_sha=resume)

    def app_bytes(self):
        return (self.selected['app']/'Contents/MacOS/weavelo').read_bytes()

    def test_readonly_plan_preserves_membership_launch_policy_and_user_content(self):
        before=(self.registry.read_bytes(),self.selected['plist'].read_bytes(),
                upgrade.membership_metadata(self.selected['profile']))
        self.assertEqual(upgrade.inspect(self.home)['record']['user_notes'],self.record['user_notes'])
        self.assertEqual(before,(self.registry.read_bytes(),self.selected['plist'].read_bytes(),
                                upgrade.membership_metadata(self.selected['profile'])))
        self.assertEqual(self.calls,[])

    def test_each_current_version_pin_conflict_refuses_before_stop(self):
        for key in ('executable','install','plist'):
            pins=self.current();pins[key]='1'*64
            with self.assertRaisesRegex(guard.ReleaseRejected,'version conflict'):self.apply(pins)
        self.assertEqual(self.calls,[]);self.assertEqual(self.app_bytes(),b'old-image')

    def test_bad_archive_digest_refuses_before_destination_write(self):
        with self.assertRaisesRegex(guard.ReleaseRejected,'digest'):
            upgrade.apply(self.archive,self.home,self.pins,'1'*64,self.current())
        self.assertFalse((self.selected['state']/'upgrade.json').exists());self.assertFalse(self.calls)

    def test_bad_signature_and_bad_source_each_refuse_before_stop(self):
        with patch.object(guard,'verified_archive',side_effect=guard.ReleaseRejected('signature')):
            with self.assertRaises(guard.ReleaseRejected):self.apply()
        with patch.object(upgrade.fresh,'verified_product',side_effect=guard.ReleaseRejected('source')):
            with self.assertRaises(guard.ReleaseRejected):self.apply()
        self.assertFalse(self.calls)

    def test_same_build_different_bytes_and_signer_change_and_downgrade_are_rejected(self):
        before=self.signed(self.selected['app'],guard.load_policy(guard.DESKTOP_POLICY_FILE))
        for change in ({'build':before['build']},{'certificate_sha1':'0'*40},{'version':'2.0.0-alpha.8'}):
            candidate=dict(self.signed(self.candidate,guard.load_policy(guard.DESKTOP_POLICY_FILE)),**change)
            @contextlib.contextmanager
            def view(*a,**k):yield self.candidate,candidate
            with patch.object(guard,'verified_archive',view):
                with self.assertRaises(guard.ReleaseRejected):self.apply()
        self.assertFalse(self.calls)

    def test_incomplete_journal_or_existing_stage_is_never_replayed(self):
        journal=self.selected['state']/'upgrade.json';journal.write_text(json.dumps({'schema':1,'phase':'exchanged'}));journal.chmod(0o600)
        with self.assertRaisesRegex(guard.ReleaseRejected,'incomplete'):self.apply({})
        journal.unlink()
        stage=self.selected['app'].parent/'.desktop-upgrade.pending';stage.mkdir()
        pins=self.current()
        with self.assertRaisesRegex(guard.ReleaseRejected,'staging'):self.apply(pins)
        self.assertFalse(self.calls);self.assertEqual(self.app_bytes(),b'old-image')

    @unittest.skipUnless(sys.platform=='darwin','native atomic directory exchange')
    def test_native_exchange_upgrade_preserves_membership_plist_history_and_retained_client(self):
        before=(self.selected['plist'].read_bytes(),upgrade.membership_metadata(self.selected['profile']))
        result=self.apply()
        self.assertTrue(result['applied']);self.assertEqual(result['phase'],'completed')
        self.assertEqual(self.app_bytes(),b'new-image')
        self.assertEqual(before,(self.selected['plist'].read_bytes(),upgrade.membership_metadata(self.selected['profile'])))
        self.assertEqual((self.old_app/'sentinel').read_bytes(),b'retained-client')
        record=json.loads(self.registry.read_text())
        self.assertEqual(record['user_notes'],self.record['user_notes'])
        self.assertEqual(record['history'],[self.record]);self.assertEqual(record['source'],self.pins)
        self.assertEqual(self.calls,['before','stop','start'])
        self.assertFalse((self.selected['app'].parent/'.desktop-upgrade.pending').exists())
        self.assertFalse(self.apply()['applied']);self.assertEqual(self.calls,['before','stop','start'])

    def test_declined_stop_preserves_old_application_and_blocks_replay(self):
        self.stop_hook=lambda: (_ for _ in ()).throw(guard.ReleaseRejected('normal quit declined'))
        pins=self.current()
        with self.assertRaisesRegex(guard.ReleaseRejected,'declined'):self.apply(pins)
        self.assertEqual(self.app_bytes(),b'old-image')
        with self.assertRaisesRegex(guard.ReleaseRejected,'incomplete'):self.apply(pins)
        self.assertEqual(self.calls,['before','stop'])

    def test_concurrent_policy_and_membership_changes_refuse_before_exchange(self):
        def change():
            self.selected['plist'].write_bytes(self.selected['plist'].read_bytes()+b'\n')
        self.stop_hook=change;pins=self.current()
        with self.assertRaisesRegex(guard.ReleaseRejected,'version conflict'):self.apply(pins)
        self.assertEqual(self.app_bytes(),b'old-image')
        self.assertNotIn('start',self.calls)

    def test_concurrent_membership_metadata_change_is_not_repaired(self):
        def change():
            p=self.selected['profile']/'mesh.conf';s=p.stat();os.utime(p,ns=(s.st_atime_ns,s.st_mtime_ns+1000000))
        self.stop_hook=change
        with self.assertRaisesRegex(guard.ReleaseRejected,'membership changed'):self.apply()
        self.assertEqual(self.app_bytes(),b'old-image');self.assertNotIn('start',self.calls)

    @unittest.skipUnless(sys.platform=='darwin','native atomic directory exchange')
    def test_activation_failure_retains_owned_old_stage_without_automatic_rollback(self):
        self.start_hook=lambda: (_ for _ in ()).throw(guard.ReleaseRejected('activation failed'))
        pins=self.current()
        with self.assertRaisesRegex(guard.ReleaseRejected,'activation'):self.apply(pins)
        self.assertEqual(self.app_bytes(),b'new-image')
        staged=self.selected['app'].parent/'.desktop-upgrade.pending/Weavelo.app/Contents/MacOS/weavelo'
        self.assertEqual(staged.read_bytes(),b'old-image')
        self.assertEqual(json.loads(self.registry.read_text()),self.record)
        with self.assertRaisesRegex(guard.ReleaseRejected,'incomplete'):self.apply(pins)

    @unittest.skipUnless(sys.platform=='darwin','native atomic directory exchange')
    def test_concurrent_install_record_after_activation_is_preserved(self):
        def change():
            current=dict(self.record,user_notes={'new':'user edit'});self.registry.write_text(json.dumps(current))
        self.start_hook=change
        with self.assertRaisesRegex(guard.ReleaseRejected,'version conflict'):self.apply()
        self.assertEqual(json.loads(self.registry.read_text())['user_notes'],{'new':'user edit'})
        self.assertTrue((self.selected['app'].parent/'.desktop-upgrade.pending').is_dir())

    def test_redirected_or_hardlinked_install_record_and_wrong_namespace_are_refused(self):
        alias=self.registry.with_name('alias.json');os.link(self.registry,alias)
        with self.assertRaisesRegex(guard.ReleaseRejected,'Private'):upgrade.inspect(self.home)
        alias.unlink()
        data=dict(self.record,application_id='fun.hackerlife.weavelo')
        self.registry.write_text(json.dumps(data))
        with self.assertRaisesRegex(guard.ReleaseRejected,'Original'):upgrade.inspect(self.home)

    def test_unknown_history_and_overflow_never_discard_user_content(self):
        for history in ({'unexpected':'format'},['x'*64000]):
            data=dict(self.record,history=history);self.registry.write_text(json.dumps(data))
            with self.assertRaises(guard.ReleaseRejected):self.apply()
            self.assertEqual(json.loads(self.registry.read_text()),data)
        self.assertFalse(self.calls)


    def test_authenticated_gui_network_status_has_pid_and_malformed_status_is_refused(self):
        digest=self.current()['executable']
        background={'role':'background-service','pid':101,'profile':str(self.selected['profile']),
                    'executable_sha256':digest}
        gui={'ok':True,'pid':102,'executable_sha256':digest}
        control=NativeControlOriginal(self.selected);flags=[]
        def query(flag,required=True):
            flags.append(flag)
            if flag=='--background-status-json':return background
            if flag=='--gui-network-diagnostics-json':return gui
            self.fail('Wrong native GUI status command')
        with patch.object(control,'query',side_effect=query),patch.object(control,'owners',return_value={101,102}),patch.object(control,'loaded',return_value=True):
            self.assertEqual(control.before(digest)['gui']['pid'],102)
            self.assertEqual(flags,['--background-status-json','--gui-network-diagnostics-json'])
            gui.pop('pid')
            with self.assertRaisesRegex(guard.ReleaseRejected,'GUI process id'):
                control.before(digest)

    @unittest.skipUnless(sys.platform=='darwin','native atomic directory exchange')
    def test_exact_pre_stop_staged_transaction_can_resume_without_recopy_or_replay(self):
        pins=self.current()
        self.before_hook=lambda: (_ for _ in ()).throw(KeyError('pid'))
        with self.assertRaises(KeyError):self.apply(pins)
        journal=self.selected['state']/'upgrade.json';digest=upgrade.gate.hash_file(journal)
        self.assertEqual(json.loads(journal.read_text())['phase'],'staged')
        self.assertEqual(self.app_bytes(),b'old-image');self.before_hook=None
        with self.assertRaises(guard.ReleaseRejected):self.apply(pins,resume='1'*64)
        wrong=dict(self.pins,commit='1'*40)
        with self.assertRaisesRegex(guard.ReleaseRejected,'exact verified'):
            upgrade.apply(self.archive,self.home,wrong,upgrade.gate.hash_file(self.archive),pins,resume_sha=digest)
        result=self.apply(pins,resume=digest)
        self.assertEqual(result['phase'],'completed');self.assertEqual(result['resumed_from_sha256'],digest)
        self.assertEqual(self.calls,['before','before','stop','start'])
        self.assertEqual(self.app_bytes(),b'new-image')

    def test_explicit_resume_never_crosses_an_unconfirmed_stop(self):
        pins=self.current()
        self.stop_hook=lambda: (_ for _ in ()).throw(guard.ReleaseRejected('declined'))
        with self.assertRaises(guard.ReleaseRejected):self.apply(pins)
        journal=self.selected['state']/'upgrade.json';digest=upgrade.gate.hash_file(journal)
        self.assertEqual(json.loads(journal.read_text())['phase'],'stopping')
        self.stop_hook=None
        with self.assertRaisesRegex(guard.ReleaseRejected,'pre-stop staged'):
            self.apply(pins,resume=digest)
        self.assertEqual(self.calls,['before','stop']);self.assertEqual(self.app_bytes(),b'old-image')

    def test_changed_staged_candidate_is_preserved_and_never_resumed(self):
        pins=self.current()
        self.before_hook=lambda: (_ for _ in ()).throw(KeyError('pid'))
        with self.assertRaises(KeyError):self.apply(pins)
        journal=self.selected['state']/'upgrade.json';digest=upgrade.gate.hash_file(journal)
        staged=self.selected['app'].parent/'.desktop-upgrade.pending/Weavelo.app/Contents/MacOS/weavelo'
        staged.write_bytes(b'user-changed-staged-image');self.before_hook=None
        with self.assertRaisesRegex(guard.ReleaseRejected,'Staged signed application differs'):
            self.apply(pins,resume=digest)
        self.assertEqual(staged.read_bytes(),b'user-changed-staged-image')
        self.assertEqual(upgrade.gate.hash_file(journal),digest)
        self.assertEqual(self.calls,['before'])


if __name__=='__main__':unittest.main()
