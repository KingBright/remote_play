import copy
from pathlib import Path
import sys
import unittest
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from compare_performance import CONDITIONS, Incomparable, compare

# Deliberately synthetic data: validates the comparator, not RemotePlay speed.
def sample(build='a'):
    conditions={key:'synthetic' for key in CONDITIONS}
    conditions.update(workload_sha256='c'*64,width=1920,height=1080,fps=60,bitrate_kbps=8000,hdr=False,audio=True,concurrent_sessions=1)
    return {'schema':1,'build_sha256':build*64,'conditions':conditions,'quality_verified':True,'quality_evidence_sha256':'d'*64,
            'runs':[{'sample_id':str(i),'duration_s':60,'warmup_s':10,
                     'metrics':{'decode_to_present_ms_p95':10+i,'presented_fps':60,
                                'full_frame_cpu_copies_per_frame':0}} for i in range(3)]}

class ComparisonTests(unittest.TestCase):
    def test_matching_reports_are_not_release_authorization(self):
        result=compare(sample(),sample('b'));self.assertEqual(result['status'],'no_reported_median_regression')
        self.assertFalse(result['release_authorized']);self.assertFalse(result['global_optimum_proven'])
    def test_latency_regression_is_detected(self):
        candidate=sample('b')
        for run in candidate['runs']:run['metrics']['decode_to_present_ms_p95']*=2
        self.assertEqual(compare(sample(),candidate)['status'],'regression')
    def test_lower_fps_is_not_an_optimization(self):
        candidate=sample('b')
        for run in candidate['runs']:run['metrics']['presented_fps']=30
        self.assertEqual(compare(sample(),candidate)['status'],'regression')
    def test_new_copy_from_zero_baseline_is_detected(self):
        candidate=sample('b')
        for run in candidate['runs']:run['metrics']['full_frame_cpu_copies_per_frame']=1
        self.assertEqual(compare(sample(),candidate)['status'],'regression')
    def test_resolution_reduction_is_incomparable(self):
        candidate=sample('b');candidate['conditions']['width']=1280
        with self.assertRaises(Incomparable):compare(sample(),candidate)
    def test_all_changed_conditions_are_rejected(self):
        for field in CONDITIONS:
            candidate=sample('b');candidate['conditions'][field]='e'*64 if field=='workload_sha256' else 'different'
            with self.subTest(field=field),self.assertRaises(Incomparable):compare(sample(),candidate)
    def test_missing_metric_does_not_hide_regression(self):
        candidate=sample('b')
        for run in candidate['runs']:del run['metrics']['presented_fps']
        with self.assertRaises(Incomparable):compare(sample(),candidate)
    def test_nonfinite_values_rejected(self):
        for value in [float('nan'),float('inf'),-1,True,'0']:
            candidate=sample('b');candidate['runs'][0]['metrics']['presented_fps']=value
            with self.subTest(value=value),self.assertRaises(Incomparable):compare(sample(),candidate)
    def test_one_short_run_not_compared_with_repeated_runs(self):
        candidate=sample('b');candidate['runs']=candidate['runs'][:1]
        with self.assertRaises(Incomparable):compare(sample(),candidate)
    def test_duplicate_sample_does_not_count_as_repeat(self):
        candidate=sample('b');candidate['runs'][1]['sample_id']='0'
        with self.assertRaises(Incomparable):compare(sample(),candidate)
    def test_duration_change_rejected(self):
        candidate=sample('b')
        for run in candidate['runs']:run['duration_s']=10
        with self.assertRaises(Incomparable):compare(sample(),candidate)
    def test_undefined_presentation_boundary_rejected(self):
        candidate=sample('b');del candidate['conditions']['measurement_boundary']
        with self.assertRaises(Incomparable):compare(sample(),candidate)
    def test_quality_evidence_is_required(self):
        candidate=sample('b');candidate['quality_verified']=False
        with self.assertRaises(Incomparable):compare(sample(),candidate)
    def test_binary_digest_is_required(self):
        candidate=sample('b');candidate['build_sha256']='alpha.7'
        with self.assertRaises(Incomparable):compare(sample(),candidate)
    def test_metrics_with_unknown_units_rejected(self):
        candidate=sample('b');candidate['runs'][0]['metrics']['latency']=4
        with self.assertRaises(Incomparable):compare(sample(),candidate)
    def test_median_resists_one_fast_outlier(self):
        candidate=sample('b')
        for run,val in zip(candidate['runs'],[1,20,20]):run['metrics']['decode_to_present_ms_p95']=val
        self.assertEqual(compare(sample(),candidate)['status'],'regression')
    def test_large_tolerance_rejected(self):
        with self.assertRaises(Incomparable):compare(sample(),sample('b'),0.2)
    def test_explicit_small_noise_allowance_not_default(self):
        candidate=sample('b')
        for run in candidate['runs']:run['metrics']['decode_to_present_ms_p95']*=1.01
        self.assertEqual(compare(sample(),candidate)['status'],'regression')
        self.assertEqual(compare(sample(),candidate,0.02)['status'],'no_reported_median_regression')

if __name__=='__main__':unittest.main()
