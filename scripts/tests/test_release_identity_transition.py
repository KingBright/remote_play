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
                    transition_intent_sha256=state['transition']['intent_sha256'],owned_aux_stages=[])

    def test_incomplete_pins_rejected_before_lifecycle_or_child(self):
        os.environ.pop('WEAVELO_EXPECTED_MANIFEST_SHA256')
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
        self.assertEqual(validator.call_args.kwargs,{'transition_intent_sha256':'a'*64})

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
        state=self.state();result=self.reservation(state)
        hook.publish(state,result,['1'*64,'2'*64])
        action,args=self.calls[-1];self.assertEqual(action,'release-publish')
        for flag,value in [('--transition-intent-sha256','a'*64),('--expected-identity-sha256','c'*64),('--expected-manifest-sha256','d'*64)]:
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
