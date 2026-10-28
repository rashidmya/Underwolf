"""Breaks caught: invalid measurements passing, swapped identity/settings, false completion."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('render_report', Path(__file__).resolve().parents[1] / 'render-4-report.py')
report = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(report)


class ReportTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.manifest = {'schema_version': 1, 'implementation_head': 'head', 'artifacts': {'layer': 'abc', 'input': 'def'}, 'scenarios': ['normal', 'max'], 'runs': []}
        # 65 seconds, 100 FPS, five seconds of warmup then a full minute.
        trace = self.root / 'trace.csv'
        trace.write_text('present_ns,interval_ns,submitted\n' + '\n'.join(f'{i*10000000},{10000000 if i else 0},0' for i in range(6502)) + '\n')
        self.meta = self.root / 'trace.json'
        self.meta.write_text(json.dumps({'status': 'complete', 'samples': 6502, 'dropped': 0, 'submitted': 0, 'skipped': 6502}))
        for scenario in ['normal', 'max']:
            for pair in range(1, 4):
                order = ['A', 'B'] if pair != 2 else ['B', 'A']
                for arm in order:
                    index = len(self.manifest['runs'])
                    own_trace, own_meta = self.root / f'{index}.csv', self.root / f'{index}.json'
                    own_trace.write_text(trace.read_text() if arm == 'A' else trace.read_text().replace(',0\n', ',1\n'))
                    own_meta.write_text(json.dumps({'status': 'complete', 'samples': 6502, 'dropped': 0, 'pid': index + 100, 'startTicks': '123', 'submitted':6502 if arm == 'B' else 0, 'skipped':0 if arm == 'B' else 6502}))
                    self.manifest['runs'].append({'scenario': scenario, 'pair': pair, 'arm': arm,
                        'process': {'pid': len(self.manifest['runs']) + 100, 'start_ticks': '123'}, 'source_head': 'head',
                        'artifacts': {'layer': 'abc', 'input': 'def'}, 'settings': {'preset': scenario, 'resolution': [1920,1080], 'scale': 1},
                        'trace': str(own_trace), 'trace_meta': str(own_meta), 'workload_ready_seconds': 0, 'warmup_seconds': 5, 'sample_seconds': 60,
                        'focused': True, 'uncapped': True, 'disturbed': False, 'previously_injected': False,
                        'input_loaded': arm == 'B', 'bootstrap_loaded': arm == 'B', 'host_active': arm == 'B', 'helper_active': arm == 'B',
                        'production_layer_loaded': True, 'spike_loaded': False, 'capture': False, 'widgets': 3 if arm == 'B' else 0, 'animation_fps': 60})

    def test_frame_rate_and_nearest_rank_match_native_stats(self):
        # total time 40ms / two frames = 20ms mean, not the mean of reciprocal intervals.
        self.assertEqual(report.summary([10_000_000, 30_000_000]), {'fps': 50.0, 'p99_ms': 30.0, 'frames': 2})
        self.assertEqual(report.summary(list(range(1, 101)))['p99_ms'], 99 / 1e6)
        self.assertRaises(ValueError, report.summary, [0])

    def test_budget_math_and_inclusive_tolerance(self):
        c = report.compare({'fps': 100, 'p99_ms': 10}, {'fps': 97, 'p99_ms': 10.3})
        self.assertAlmostEqual(c['fps_loss_pct'], 3)
        self.assertAlmostEqual(c['p99_increase_pct'], 3)
        self.assertTrue(c['within_budget'])
        self.assertFalse(report.compare({'fps':100,'p99_ms':10}, {'fps':96.9,'p99_ms':10})['within_budget'])
        self.assertFalse(report.compare({'fps':100,'p99_ms':10}, {'fps':100,'p99_ms':10.31})['within_budget'])

    def test_three_matched_pairs_and_scenario_means(self):
        result = report.evaluate(self.manifest, self.root)
        self.assertEqual(result['gates']['performance']['status'], 'pass')
        normal = result['gates']['performance']['scenarios']['normal']
        self.assertEqual(len(normal['pairs']), 3)
        self.assertEqual(normal['A']['fps'], 100)
        self.assertEqual(normal['B']['p99_ms'], 10)
        self.assertEqual(result['gates']['live']['status'], 'pending')
        self.assertEqual(result['gates']['amd']['status'], 'unavailable')
        self.assertEqual(result['status'], 'implemented_open_acceptance')
        self.assertEqual(report.mean([{'fps': 100, 'p99_ms': 5, 'frames': 2}, {'fps': 80, 'p99_ms': 9, 'frames': 4}, {'fps': 90, 'p99_ms': 7, 'frames': 3}]), {'fps': 90, 'p99_ms': 7, 'frames': 9})

    def test_bad_pairing_artifacts_settings_and_process_reuse_are_invalid(self):
        mutations = [lambda m: m['runs'].pop(), lambda m: m['runs'][0].update(scenario='other'),
            lambda m: m['runs'][0]['artifacts'].update(layer='wrong'), lambda m: m['runs'][0]['settings'].update(scale=1.5),
            lambda m: m['runs'][1].update(process=m['runs'][0]['process']), lambda m: m['runs'][0].update(previously_injected=True),
            lambda m: m['runs'][0].update(input_loaded=True), lambda m: m['runs'][0].update(focused=False),
            lambda m: m['runs'][0].update(uncapped=False), lambda m: m['runs'][0].update(disturbed=True),
            lambda m: m['runs'][1].update(animation_fps=30), lambda m: m['runs'][1].update(capture=True),
            lambda m: m['runs'][0].update(warmup_seconds=4), lambda m: m['runs'][0].update(sample_seconds=59),
            lambda m: m['runs'][1].update(trace=m['runs'][0]['trace']),
            lambda m: m['runs'][1].update(workload_ready_seconds=1)]
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                manifest = copy.deepcopy(self.manifest); mutate(manifest)
                self.assertEqual(report.evaluate(manifest, self.root)['gates']['performance']['status'], 'invalid')

    def test_nonmonotonic_truncated_or_dropped_trace_is_invalid(self):
        trace = Path(self.manifest['runs'][0]['trace'])
        self.meta = Path(self.manifest['runs'][0]['trace_meta'])
        original = trace.read_text()
        for text in [original[:-1], original.replace('10000000,10000000,0', '0,10000000,0', 1), original.replace('20000000,10000000,0', '20000000,9,0', 1)]:
            trace.write_text(text)
            self.assertEqual(report.evaluate(self.manifest, self.root)['gates']['performance']['status'], 'invalid')

        trace.write_text(original)
        base_meta = json.loads(self.meta.read_text())
        for change in [{'status':'running'}, {'samples':6501}, {'dropped':1}]:
            meta = dict(base_meta); meta.update(change)
            self.meta.write_text(json.dumps(meta))
            self.assertEqual(report.evaluate(self.manifest, self.root)['gates']['performance']['status'], 'invalid')
            self.meta.write_text(json.dumps(base_meta))

    def test_trace_metadata_must_match_the_measured_process_incarnation(self):
        path = Path(self.manifest['runs'][0]['trace_meta'])
        meta = json.loads(path.read_text()); meta['startTicks'] = '124'; path.write_text(json.dumps(meta))
        self.assertEqual(report.evaluate(self.manifest, self.root)['gates']['performance']['status'], 'invalid')

    def test_draw_trace_must_match_the_claimed_benchmark_arm(self):
        run = self.manifest['runs'][1]
        path = Path(run['trace']); path.write_text(path.read_text().replace(',1\n', ',0\n'))
        meta_path = Path(run['trace_meta']); meta = json.loads(meta_path.read_text())
        meta.update(submitted=0, skipped=6502); meta_path.write_text(json.dumps(meta))
        self.assertEqual(report.evaluate(self.manifest, self.root)['gates']['performance']['status'], 'invalid')

    def test_portable_passes_never_fill_missing_live_or_hardware_evidence(self):
        self.manifest['runs'] = []
        self.manifest['gates'] = {k: {'status':'pass'} for k in ['automated', 'live', 'vulkan_rendering', 'input_integration', 'transport_comparison']}
        result = report.evaluate(self.manifest, self.root)
        self.assertEqual(result['gates']['performance']['status'], 'pending')
        self.assertEqual(result['gates']['live']['status'], 'pending')
        self.assertEqual(result['gates']['amd']['status'], 'unavailable')
        self.assertNotEqual(result['status'], 'complete')

if __name__ == '__main__':
    unittest.main()
