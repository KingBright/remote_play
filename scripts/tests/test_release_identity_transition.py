"""Project wiring fixtures; no real helper, registry, signing or compilation."""
import importlib.util
from pathlib import Path
import json
import os
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('release_hook_base',Path(__file__).with_name('test_release_artifact_lifecycle.py'))
base=importlib.util.module_from_spec(spec);spec.loader.exec_module(base)
hook=base.hook
REAL_REGULAR=hook.regular


class TransitionWiringTests(unittest.TestCase):
    setUp=base.ReleaseHookTests.setUp
    public_call=base.ReleaseHookTests.public_call
    current_guard=base.ReleaseHookTests.current_guard

    def environment(self):
        return {hook.TRANSITION_ENV:'a'*64,'WEAVELO_EXPECTED_COMMIT':'b'*40,
                'WEAVELO_EXPECTED_IDENTITY_SHA256':'c'*64,'WEAVELO_EXPECTED_MANIFEST_SHA256':'d'*64}

    def state(self):
        return dict(self.state_base,transition=hook.transition_config('macos'))

    def setUp(self):
        base.ReleaseHookTests.setUp(self)
        self.state_base=self.state
        del self.state
        self.environment_patch=patch.dict(os.environ,self.environment())
        self.environment_patch.start();self.addCleanup(self.environment_patch.stop)

    def reservation(self,state):
        files=list(map(str,[self.root/'pkg/Weavelo.zip',self.root/'pkg/Weavelo.verification.json']))
        return dict(component='macos-arm64-release',files=files,staged_files=list(map(str,hook.stage_paths(files))),
                    transition_intent_sha256=state['transition']['intent_sha256'],owned_aux_stages=[],
                    candidate_state='unsealed',candidate_expectations=state['transition']['candidate_expectations'],reservation_sha256='f'*64)

    def test_incomplete_pins_rejected_before_lifecycle_or_child(self):
        os.environ.pop('WEAVELO_EXPECTED_IDENTITY_SHA256')
        with self.assertRaises(hook.Refusal),hook.producer('macos'):pass
        self.assertEqual(self.calls,[])
        self.assertFalse((self.root/'.rust-storage').exists())

    def test_transition_rejected_for_android_before_lifecycle(self):
        with self.assertRaises(hook.Refusal),hook.producer('android-release'):pass
        self.assertEqual(self.calls,[])

    def test_inherited_child_environment_drift_is_rejected(self):
        with hook.producer('macos') as (fd,state),patch.dict(os.environ,{hook.LOCK_ENV:str(fd)}):
            self.assertEqual(hook.check_lock('macos')[1],state)
            os.environ[hook.TRANSITION_ENV]='e'*64
            with self.assertRaises(hook.Refusal):hook.check_lock('macos')

    def test_exact_intent_passed_to_official_admission_and_child_guard(self):
        state=self.state();result=self.reservation(state)
        actual=dict(result,candidate_expectations=state['transition']['candidate_expectations'])
        validator=self.current_guard(actual)
        def public(action,*args):
            self.calls.append((action,list(args)))
            return dict(result,admission='reserved')
        with patch.object(hook,'public',side_effect=public):
            returned=hook.admit(state,result['component'],result['files'])
        self.assertEqual(returned['transition_intent_sha256'],'a'*64)
        self.assertIn('--transition-intent-sha256',self.calls[-1][1])
        self.assertNotIn('--adopt-current',self.calls[-1][1])
        self.assertEqual(validator.call_args.kwargs,{'transition_intent_sha256':'a'*64,'transition_phase':'prepare'})

    def test_mismatched_official_candidate_pins_refuse_before_packaging(self):
        state=self.state();result=self.reservation(state)
        self.current_guard(dict(result,candidate_expectations=dict(state['transition']['candidate_expectations'],manifest_sha256='e'*64)))
        with self.assertRaises(hook.Refusal):hook.verify_transition_producer(state,result)

    def test_missing_official_intent_echo_refuses(self):
        state=self.state();result=self.reservation(state)
        with patch.object(hook,'public',return_value=dict(result,admission='reserved',transition_intent_sha256=None)):
            with self.assertRaises(hook.Refusal):hook.admit(state,result['component'],result['files'])

    def test_transition_cannot_adopt_existing_output(self):
        state=self.state();result=self.reservation(state)
        with self.assertRaises(hook.Refusal):hook.admit(state,result['component'],result['files'],current=['1'*64,'2'*64])
        self.assertEqual(self.calls,[])

    def test_publish_repeats_same_intent_and_independent_pins(self):
        state=self.state();result=dict(self.reservation(state),candidate_state='sealed',candidate_seal_sha256='9'*64,
                                     candidate_expectations=dict(state['transition']['candidate_expectations'],manifest_sha256='d'*64))
        self.current_guard(result)
        hook.publish(state,result,['1'*64,'2'*64])
        action,args=self.calls[-1];self.assertEqual(action,'release-publish')
        for flag,value in [('--transition-intent-sha256','a'*64),('--expected-identity-sha256','c'*64),('--expected-manifest-sha256','d'*64),('--candidate-seal-sha256','9'*64)]:
            self.assertEqual(args[args.index(flag)+1],value)
        self.assertEqual(args.count('--expected-sha256'),2)

    def test_wrong_signed_manifest_never_published(self):
        state=self.state();admission=self.reservation(state)
        paths=list(map(Path,admission['staged_files']));paths[0].parent.mkdir()
        paths[0].write_bytes(b'candidate')
        import hashlib
        report=dict(signature_verified=True,archive=admission['files'][0],archive_sha256=hashlib.sha256(b'candidate').hexdigest(),size_bytes=9,
                    build_identity_expectations=dict(state['transition']['candidate_expectations'],manifest_sha256='e'*64))
        paths[1].write_text(json.dumps(report))
        with self.assertRaises(hook.Refusal):hook.publish_macos(state,admission,report)
        self.assertEqual(self.calls,[])
        self.assertEqual(paths[0].read_bytes(),b'candidate')

    def test_wrong_publication_intent_never_reaches_helper(self):
        state=self.state();result=dict(self.reservation(state),transition_intent_sha256='e'*64)
        with self.assertRaises(hook.Refusal):hook.publish(state,result,['1'*64,'2'*64])
        self.assertEqual(self.calls,[])


    def candidate(self):
        import hashlib
        state=self.state();a=self.reservation(state)
        aux=self.root/'pkg/.weavelo-macos-package.pending';aux.mkdir(parents=True)
        a['owned_aux_stages']=[str(aux)]
        manifest=aux/'weavelo-release-manifest.json';manifest.write_bytes(b'{"kind":"synthetic-fixture"}')
        pins=dict(state['transition']['candidate_expectations'],manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest())
        archive,receipt=map(Path,a['staged_files']);archive.write_bytes(b'signed-fixture')
        report=dict(signature_verified=True,archive=a['files'][0],archive_sha256=hashlib.sha256(archive.read_bytes()).hexdigest(),
                    size_bytes=archive.stat().st_size,build_identity_expectations=pins)
        receipt.write_text(json.dumps(report))
        sealed=dict(a,candidate_state='sealed',candidate_expectations=pins,candidate_seal_sha256='9'*64)
        return state,a,manifest,report,sealed

    def test_preparation_needs_no_manifest_pin(self):
        os.environ.pop('WEAVELO_EXPECTED_MANIFEST_SHA256')
        self.assertEqual(set(hook.transition_config('macos')['candidate_expectations']),{'commit','identity_sha256'})

    def test_unsealed_publish_refused_before_public_call(self):
        state=self.state()
        with self.assertRaises(hook.Refusal):hook.publish(state,self.reservation(state),['1'*64,'2'*64])
        self.assertEqual(self.calls,[])

    def test_real_manifest_seal_then_sealed_child_check_then_publish(self):
        state,a,manifest,report,sealed=self.candidate();events=[]
        validator=self.current_guard(sealed)
        validator.side_effect=lambda *args,**kw: events.append(('child',kw)) or sealed
        def public(action,*args):
            events.append((action,list(map(str,args))))
            if action=='release-transition-seal':return sealed
            return dict(current_committed=True,paths=a['files'],cleanup_status='completed')
        with patch.object(hook,'public',side_effect=public),patch.object(hook,'regular',new=REAL_REGULAR):
            hook.publish_macos(state,a,report)
        self.assertEqual([e[0] for e in events],['release-transition-seal','child','release-publish'])
        seal_args=events[0][1]
        self.assertEqual(seal_args[seal_args.index('--manifest-file')+1],str(manifest))
        self.assertEqual(seal_args[seal_args.index('--expected-reservation-sha256')+1],'f'*64)
        self.assertEqual(events[1][1],dict(transition_intent_sha256='a'*64,transition_phase='sealed',candidate_seal_sha256='9'*64))
        self.assertEqual(events[2][1][events[2][1].index('--candidate-seal-sha256')+1],'9'*64)

    def test_tampered_actual_manifest_never_calls_seal(self):
        state,a,manifest,report,_=self.candidate();manifest.write_bytes(b'foreign')
        with self.assertRaises(hook.Refusal):hook.publish_macos(state,a,report)
        self.assertEqual(self.calls,[])
        self.assertEqual(manifest.read_bytes(),b'foreign')

    def test_seal_rejection_never_publishes_or_deletes_stages(self):
        state,a,manifest,report,_=self.candidate()
        with patch.object(hook,'public',side_effect=hook.Refusal('official seal rejected')) as public:
            with self.assertRaises(hook.Refusal):hook.publish_macos(state,a,report)
        self.assertEqual(public.call_args.args[0],'release-transition-seal')
        self.assertTrue(manifest.exists());self.assertTrue(Path(a['staged_files'][0]).exists())

    def test_missing_or_different_seal_proof_never_publishes(self):
        state,a,manifest,report,sealed=self.candidate()
        for bad in [dict(sealed,candidate_seal_sha256=''),dict(sealed,candidate_state='unsealed'),
                    dict(sealed,candidate_expectations=dict(sealed['candidate_expectations'],identity_sha256='e'*64))]:
            with self.subTest(bad=bad),patch.object(hook,'public',return_value=bad) as public:
                with self.assertRaises(hook.Refusal):hook.publish_macos(state,a,report)
                self.assertEqual(public.call_count,1)
                self.assertEqual(public.call_args.args[0],'release-transition-seal')
