#!/usr/bin/env python3
"""Validate explicitly listed phase 4 traces; missing acceptance stays open."""
from __future__ import annotations
import argparse
import csv
import json
import math
from pathlib import Path
import statistics

REQUIRED = ('automated', 'vulkan_rendering', 'input_integration', 'live', 'performance', 'transport_comparison')


def summary(intervals):
    if not intervals or any(type(n) is not int or n <= 0 for n in intervals):
        raise ValueError('empty/nonpositive frame intervals')
    return {'fps': len(intervals) * 1e9 / sum(intervals), 'p99_ms': sorted(intervals)[math.ceil(.99 * len(intervals)) - 1] / 1e6, 'frames': len(intervals)}


def mean(runs):
    if not runs:
        raise ValueError('no run summaries')
    return {'fps': statistics.mean(r['fps'] for r in runs), 'p99_ms': statistics.mean(r['p99_ms'] for r in runs), 'frames': sum(r['frames'] for r in runs)}


def compare(a, b):
    loss = (a['fps'] - b['fps']) / a['fps'] * 100
    increase = (b['p99_ms'] - a['p99_ms']) / a['p99_ms'] * 100
    return {'fps_loss_pct': loss, 'p99_increase_pct': increase, 'within_budget': loss <= 3 + 1e-9 and increase <= 3 + 1e-9}


def listed(root, name):
    if not isinstance(name, str) or not name:
        raise ValueError('missing explicit evidence path')
    return (root / name).resolve()


def read_trace(run, root):
    text = listed(root, run['trace']).read_text()
    meta = json.loads(listed(root, run['trace_meta']).read_text())
    if meta.get('pid') != run['process']['pid'] or meta.get('startTicks') != run['process']['start_ticks']:
        raise ValueError('trace belongs to a different process incarnation')
    if not text.endswith('\n') or meta['status'] != 'complete' or meta['dropped'] != 0:
        raise ValueError('truncated, unfinished or dropped trace')
    rows = list(csv.DictReader(text.splitlines()))
    if len(rows) != meta['samples'] or len(rows) < 2:
        raise ValueError('truncated trace sample count')
    previous = first = None
    kept = []
    total_submitted = kept_submitted = 0
    warmup, duration = run['warmup_seconds'] * 1e9, run['sample_seconds'] * 1e9
    for row in rows:
        at, interval, submitted = (int(row[k]) for k in ('present_ns', 'interval_ns', 'submitted'))
        if at < 0 or submitted not in (0, 1):
            raise ValueError('invalid trace row')
        total_submitted += submitted
        if previous is None:
            if interval != 0:
                raise ValueError('missing first trace row')
            first = at
        elif at <= previous or interval != at - previous:
            raise ValueError('nonmonotonic or inconsistent trace')
        elif previous >= first + warmup and at <= first + warmup + duration:
            kept.append(interval)
            kept_submitted += submitted
        previous = at
    if previous < first + warmup + duration or sum(kept) < duration - max(kept, default=0):
        raise ValueError('incomplete measurement duration')
    if meta.get('submitted') != total_submitted or meta.get('skipped') != len(rows) - total_submitted:
        raise ValueError('trace submission counts disagree')
    if run['arm'] == 'A' and total_submitted or run['arm'] == 'B' and not kept_submitted:
        raise ValueError('draw trace does not match the benchmark arm')
    return summary(kept)


def performance(manifest, root):
    runs = manifest.get('runs', [])
    if not runs:
        return {'status': 'pending', 'scope': 'no measured whole-overlay traces supplied'}
    scenarios = manifest.get('scenarios', ['normal', 'max'])
    if set(scenarios) != {'normal', 'max'} or len(scenarios) != 2 or len(runs) != 12:
        raise ValueError('normal/max each require three complete pairs')
    seen_processes = set()
    seen_traces = set()
    grouped = {s: {i: [] for i in range(1, 4)} for s in scenarios}
    for run in runs:
        scenario, pair, arm = run['scenario'], run['pair'], run['arm']
        if scenario not in grouped or pair not in grouped[scenario] or arm not in ('A', 'B'):
            raise ValueError('mismatched scenario or pair')
        identity = (run['process']['pid'], run['process']['start_ticks'])
        if type(identity[0]) is not int or identity[0] <= 0 or not isinstance(identity[1], str) or not identity[1].isdigit() or str(int(identity[1])) != identity[1] or int(identity[1]) <= 0:
            raise ValueError('invalid process identity')
        if identity in seen_processes:
            raise ValueError('process reused across benchmark arms')
        seen_processes.add(identity)
        trace = listed(root, run['trace'])
        if trace in seen_traces:
            raise ValueError('trace reused across benchmark arms')
        seen_traces.add(trace)
        if run['source_head'] != manifest['implementation_head'] or run['artifacts'] != manifest['artifacts'] or not run['artifacts']:
            raise ValueError('mismatched source/artifacts')
        if run['focused'] is not True or run['uncapped'] is not True or run['disturbed'] is not False or run['previously_injected'] is not False or run['spike_loaded'] is not False or run['production_layer_loaded'] is not True or run['capture'] is not False:
            raise ValueError('focus/cap/module/capture violation')
        if not math.isfinite(run['warmup_seconds']) or run['warmup_seconds'] < 5 or run['sample_seconds'] != 60:
            raise ValueError('need five seconds warmup and sixty seconds sampling')
        ready = run['workload_ready_seconds']
        if not math.isfinite(ready) or ready < 0 or run['warmup_seconds'] - ready < 5:
            raise ValueError('full workload must be ready for five seconds before sampling')
        for flag in ('input_loaded', 'bootstrap_loaded', 'host_active', 'helper_active'):
            if run[flag] is not (arm == 'B'):
                raise ValueError('baseline must be uninjected; B requires the whole overlay')
        if run['widgets'] != (3 if arm == 'B' else 0) or run['animation_fps'] != 60:
            raise ValueError('incorrect benchmark workload')
        settings = run['settings']
        if not settings.get('preset') or len(settings.get('resolution', [])) != 2 or any(type(n) is not int or n <= 0 for n in settings['resolution']) or not math.isfinite(settings.get('scale', 0)) or settings.get('scale', 0) <= 0:
            raise ValueError('incomplete settings inventory')
        grouped[scenario][pair].append((arm, run, read_trace(run, root)))
    results = {}
    for scenario, pairs in grouped.items():
        collected = []
        for pair, values in pairs.items():
            if [v[0] for v in values] != (['B', 'A'] if pair == 2 else ['A', 'B']):
                raise ValueError('need alternating AB/BA/AB collection order')
            by_arm = {v[0]: v for v in values}
            if by_arm['A'][1]['settings'] != by_arm['B'][1]['settings'] or by_arm['A'][1]['settings'] != pairs[1][0][1]['settings']:
                raise ValueError('settings changed within scenario')
            a, b = by_arm['A'][2], by_arm['B'][2]
            collected.append({'pair': pair, 'A': a, 'B': b, **compare(a, b)})
        a, b = mean([p['A'] for p in collected]), mean([p['B'] for p in collected])
        results[scenario] = {'A': a, 'B': b, 'pairs': collected, **compare(a, b)}
    return {'status': 'pass' if all(s['within_budget'] for s in results.values()) else 'fail', 'scenarios': results,
            'scope': 'listed traces and explicit operator focus/settings/module attestations; not a substitute for owner visual/input observation'}


def evidence_gate(value, root, head):
    if not isinstance(value, dict):
        return {'status': 'pending'}
    gate = dict(value)
    if gate.get('status') not in ('pending', 'pass', 'fail', 'invalid', 'unavailable'):
        return {'status': 'invalid', 'reason': 'unknown gate status'}
    if gate.get('status') == 'pass' and (gate.get('source_head') != head or not gate.get('raw') or not all(listed(root, p).is_file() for p in gate['raw'])):
        return {'status': 'invalid', 'reason': 'pass lacks matching source head and existing raw evidence'}
    return gate


def evaluate(manifest, root):
    root = Path(root)
    head = manifest.get('implementation_head')
    gates = {name: evidence_gate(manifest.get('gates', {}).get(name), root, head) for name in REQUIRED}
    # A generic portable/pass flag cannot manufacture an owner observation.
    gates['live'] = evidence_gate(manifest.get('owner_observation'), root, head)
    gates['amd'] = evidence_gate(manifest.get('amd', {'status':'unavailable', 'scope':'no AMD hardware evidence supplied; before-release gate'}), root, head)
    gates['gpu_bound'] = evidence_gate(manifest.get('gpu_bound', {'status':'unavailable', 'scope':'no verified GPU-bound workload supplied'}), root, head)
    try:
        if manifest.get('schema_version') != 1 or not isinstance(head, str) or not head:
            raise ValueError('invalid manifest schema/source head')
        gates['performance'] = performance(manifest, root)
    except (ValueError, KeyError, TypeError, OSError, ZeroDivisionError) as error:
        gates['performance'] = {'status':'invalid', 'reason':str(error)}
    gates['performance'].update(source_head=head, raw=[r.get('trace') for r in manifest.get('runs', [])])
    return {'schema_version':1, 'phase':'rendering 4', 'status':'complete' if all(gates[n]['status'] == 'pass' for n in REQUIRED) else 'implemented_open_acceptance',
            'implementation_head':head, 'artifacts':manifest.get('artifacts', {}), 'environment':manifest.get('environment', {}), 'gates':gates,
            'deferred_items':manifest.get('deferred_items', ['GameInput adapter remains deferred under 3b', 'AMD coverage required before release']),
            'integration':manifest.get('integration', {'merged':False,'pushed':False})}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = evaluate(json.loads(args.manifest.read_text()), args.manifest.resolve().parent)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, allow_nan=False) + '\n')
    print(f"{result['status']}: {args.output}")
    return int(any(g['status'] in ('invalid','fail') for g in result['gates'].values()))

if __name__ == '__main__':
    raise SystemExit(main())
