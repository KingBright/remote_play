#!/usr/bin/env python3
"""Compare repeated, matching benchmark reports; never run or deploy an application.

This checks supplied measurement consistency, not hardware truth or global optimality.
Raw measurement collection and visual/functional acceptance remain separate.
Exit 0: no reported median regression; 1: regression; 2: invalid/incomparable data.
"""
from __future__ import annotations
import argparse
import json
import math
from pathlib import Path
import re
import statistics
import sys

CONDITIONS = (
    'device_id', 'platform', 'os_build', 'gpu', 'driver', 'power_mode',
    'thermal_state', 'peer_id', 'peer_build', 'route', 'network_profile',
    'workload_sha256', 'width', 'height', 'fps', 'bitrate_kbps', 'codec',
    'pixel_format', 'hdr', 'audio', 'concurrent_sessions', 'measurement_boundary',
)
LOWER = frozenset({
    'first_frame_ms_p95', 'decode_to_present_ms_p95', 'decode_to_present_ms_p99',
    'input_response_ms_p95', 'cpu_percent', 'gpu_percent', 'rss_peak_mib',
    'retained_growth_mib', 'frame_drop_percent', 'idle_wakeups_per_s',
    'full_frame_cpu_copies_per_frame', 'queued_decoded_frames_max', 'power_watts',
})
HIGHER = frozenset({'presented_fps', 'file_goodput_mib_per_s'})
DIGEST = re.compile(r'[0-9a-f]{64}')

class Incomparable(ValueError):
    pass

def numeric(value, name):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0:
        raise Incomparable(f'{name} must be a finite nonnegative number')
    return value

def validate(report):
    if not isinstance(report, dict) or report.get('schema') != 1:
        raise Incomparable('Expected measurement report schema 1')
    if not isinstance(report.get('build_sha256'), str) or not DIGEST.fullmatch(report['build_sha256']):
        raise Incomparable('Missing exact tested binary SHA-256')
    conditions = report.get('conditions')
    if not isinstance(conditions, dict) or any(k not in conditions or conditions[k] is None or conditions[k] == '' for k in CONDITIONS):
        raise Incomparable('Incomplete test conditions; unknown data cannot count as a pass')
    if not isinstance(conditions['workload_sha256'], str) or not DIGEST.fullmatch(conditions['workload_sha256']):
        raise Incomparable('Workload must be identified by content digest')
    if report.get('quality_verified') is not True or not isinstance(report.get('quality_evidence_sha256'), str) or not DIGEST.fullmatch(report['quality_evidence_sha256']):
        raise Incomparable('Explicit same-quality verification evidence is required')
    runs = report.get('runs')
    if not isinstance(runs, list) or len(runs) < 3:
        raise Incomparable('At least three comparable measured runs are required')
    signatures = set()
    all_ids = set()
    for run in runs:
        if not isinstance(run, dict) or not isinstance(run.get('sample_id'), str) or not run['sample_id'] or run['sample_id'] in all_ids:
            raise Incomparable('Each run requires a unique sample id')
        all_ids.add(run['sample_id'])
        duration = numeric(run.get('duration_s'), 'duration_s')
        warmup = numeric(run.get('warmup_s'), 'warmup_s')
        if duration == 0:
            raise Incomparable('A zero-duration observation is not a run')
        metrics = run.get('metrics')
        if not isinstance(metrics, dict) or not metrics or set(metrics) - (LOWER | HIGHER):
            raise Incomparable('Missing or unsupported metric definitions')
        for key, value in metrics.items():
            numeric(value, key)
        signatures.add((duration, warmup, tuple(sorted(metrics))))
    if len(signatures) != 1:
        raise Incomparable('Runs must share duration, warmup and metric definitions')
    return next(iter(signatures))

def compare(baseline, candidate, tolerance=0.0):
    numeric(tolerance, 'tolerance')
    if tolerance > 0.05:
        raise Incomparable('A large regression must not be hidden inside a noise allowance')
    before = validate(baseline); after = validate(candidate)
    # Comparison is per device/role/workload, never pooled across incompatible machines.
    if baseline['conditions'] != candidate['conditions'] or before != after or len(baseline['runs']) != len(candidate['runs']):
        raise Incomparable('Different hardware, route, quality, workload, boundary or sampling conditions')
    rows=[]
    for metric in before[2]:
        old=statistics.median(run['metrics'][metric] for run in baseline['runs'])
        new=statistics.median(run['metrics'][metric] for run in candidate['runs'])
        delta=(new-old) if metric in LOWER else (old-new)
        # Explicitly handle a zero baseline; adding a copy cannot hide behind division by zero.
        regression=delta > abs(old)*tolerance
        rows.append({'metric':metric,'baseline_median':old,'candidate_median':new,
                     'direction':'lower_is_better' if metric in LOWER else 'higher_is_better',
                     'relative_regression':delta/abs(old) if old else None,'regression':regression})
    return {'schema':1,'status':'regression' if any(r['regression'] for r in rows) else 'no_reported_median_regression',
            'baseline_build':baseline['build_sha256'],'candidate_build':candidate['build_sha256'],
            'tolerance':tolerance,'metrics':rows,'claim_boundary':'supplied comparable reports only',
            'global_optimum_proven':False,'physical_measurements_executed_by_this_tool':False,
            'release_authorized':False}

def load(path):
    if path.stat().st_size > 8*1024*1024:
        raise Incomparable('Measurement report too large')
    return json.loads(path.read_text(encoding='utf-8'))

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('baseline',type=Path);parser.add_argument('candidate',type=Path)
    parser.add_argument('--tolerance',type=float,default=0.0,
                        help='Explicit relative measurement allowance, 0 by default, at most 0.05; not release approval')
    args=parser.parse_args()
    try:
        result=compare(load(args.baseline),load(args.candidate),args.tolerance)
    except (Incomparable,OSError,ValueError,TypeError,KeyError) as error:
        print(json.dumps({'status':'incomparable','error':str(error),'release_authorized':False}),file=sys.stderr)
        return 2
    print(json.dumps(result,indent=2,allow_nan=False))
    return int(result['status']=='regression')

if __name__=='__main__':
    sys.exit(main())
