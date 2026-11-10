"""Metric diagnostics and the actual job-digest front door, without training."""
from copy import deepcopy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/lib'))
from oms_runtime import training_metrics as metrics
from oms_runtime.inspection_io import ReadError

NOW = 1794268800


class TrainingMetricsTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='oms-metric-history-')
        self.addCleanup(temp.cleanup)
        self.repo = Path(temp.name)
        self.path = self.repo / 'metrics.jsonl'

    def write(self, rows):
        self.path.write_text(''.join(json.dumps(row) + '\n' for row in rows), encoding='utf-8')

    def row(self, step=0, value=1.0, **extra):
        return dict(step=step, name='train/loss', value=value, ts=NOW - 10, **extra)

    def report(self, **options):
        return metrics.inspect_metrics(self.repo, 'metrics.jsonl', now=NOW, **options)

    def test_minimize_exact_metric_and_endpoint_trend(self):
        self.write([self.row(0, 3), self.row(10, 1), self.row(20, 2)])
        item = self.report(metric='train/loss', direction='minimize')['series'][0]
        self.assertEqual(item['best'], {'step': 10, 'value': 1.0})
        self.assertEqual(item['latest']['value'], 2)
        self.assertEqual(item['trend']['value_per_step'], -0.05)
        self.assertEqual(item['freshness']['state'], 'recent-record')

    def test_observe_never_infers_best_direction(self):
        self.write([self.row(0, 1), self.row(1, 2)])
        report = self.report()
        self.assertIsNone(report['series'][0]['best'])
        self.assertNotIn('success', report)
        self.assertNotIn('winner', report)
        with self.assertRaises(metrics.MetricsError):
            self.report(direction='minimize')

    def test_maximize_and_exact_filter(self):
        self.write([self.row(0, 1), {'step': 1, 'name': 'val/auc', 'value': 0.9}, self.row(1, 4)])
        report = self.report(metric='train/loss', direction='maximize')
        self.assertEqual(len(report['series']), 1)
        self.assertEqual(report['series'][0]['best']['value'], 4)
        self.assertEqual(report['coverage']['diagnostics']['filtered-row'], 1)

    def test_run_ids_do_not_mix(self):
        self.write([self.row(0, 1, run_id='run-a'), self.row(0, 100, run_id='run-b')])
        self.assertEqual([s['run_id'] for s in self.report()['series']], ['run-a', 'run-b'])

    def test_step_reversal_starts_an_ambiguous_segment(self):
        self.write([self.row(10, 0.01), self.row(20, 0.02), self.row(0, 3), self.row(1, 2)])
        result = self.report(metric='train/loss', direction='minimize')
        item = result['series'][0]
        self.assertEqual(item['step_regressions'], 1)
        self.assertEqual(item['earlier_segment_points'], 2)
        self.assertEqual(item['best']['value'], 2)
        self.assertFalse(result['coverage']['history_complete'])

    def test_duplicate_step_correction_replaces_stale_extreme(self):
        self.write([self.row(0, 0.01), self.row(0, 2), self.row(1, 1)])
        item = self.report(metric='train/loss', direction='minimize')['series'][0]
        self.assertEqual(item['duplicate_steps'], 1)
        self.assertEqual(item['retained_points'], 2)
        self.assertEqual(item['minimum']['value'], 1)

    def test_nonfinite_values_are_reported_not_json_nan(self):
        for value in ('NaN', float('nan'), 'Infinity', float('-inf')):
            with self.subTest(value=value):
                self.write([self.row(0, 1), self.row(1, value)])
                result = self.report()
                self.assertIsNone(result['series'][0]['latest']['value'])
                self.assertEqual(result['series'][0]['nonfinite_observations'], 1)
                self.assertEqual(result['series'][0]['trend']['reason'], 'non-finite-in-window')
                json.dumps(result, allow_nan=False)

    def test_missing_bad_future_and_old_timestamps(self):
        for ts, expected in ((None, 'unknown'), ('2026-11-10T00:00:00', 'unknown'),
                             (NOW + 1, 'clock-skew'), (NOW - 700, 'late-record')):
            row = self.row(); row['ts'] = ts; self.write([row])
            with self.subTest(ts=ts):
                self.assertEqual(self.report()['series'][0]['freshness']['state'], expected)
        row = self.row(); row['ts'] = '2026-11-10T08:59:50+09:00'; self.write([row])
        self.assertEqual(self.report()['series'][0]['freshness']['age_seconds'], 10)

    def test_timestamp_reversal_does_not_claim_freshness(self):
        a, b = self.row(0), self.row(1)
        a['ts'] = NOW-10; b['ts'] = NOW-20; self.write([a, b])
        item = self.report()['series'][0]
        self.assertEqual(item['timestamp_regressions'], 1)
        self.assertEqual(item['freshness']['state'], 'unknown')

    def test_invalid_rows_do_not_become_zero_values(self):
        rows = [self.row(value=True), self.row(step=-1), self.row(value='password'),
                self.row(step=True), {'step': 1, 'name': '\x1b[31m', 'value': 1}, [1, 2]]
        self.write(rows)
        report = self.report()
        self.assertEqual(report['series'], [])
        self.assertEqual(report['coverage']['diagnostics']['invalid-row'], 6)
        self.assertFalse(report['coverage']['history_complete'])
        self.assertNotIn('password', json.dumps(report))

    def test_partial_final_line_and_duplicate_json_key(self):
        self.write([self.row()])
        with self.path.open('ab') as handle:
            handle.write(b'{"step":1,"step":2,"name":"train/loss","value":1}\n{"step":')
        result = self.report()
        self.assertEqual(result['coverage']['matched_rows'], 1)
        self.assertEqual(result['coverage']['unterminated_tail_bytes'], 8)
        self.assertEqual(result['coverage']['diagnostics']['invalid-row'], 1)

    def test_dvclive_tsv_with_and_without_millisecond_time(self):
        self.path.write_text('timestamp\tstep\tloss\n'+str((NOW-5)*1000)+'\t0\t0.4\n')
        result = self.report(input_format='dvclive', metric='loss', direction='minimize')
        self.assertEqual(result['series'][0]['freshness']['age_seconds'], 5)
        self.path.write_text('step\tloss\n0\t0.4\n1\tNaN\n')
        result = self.report(input_format='dvclive')
        self.assertEqual(result['series'][0]['latest']['value_state'], 'nan')
        self.assertEqual(result['series'][0]['freshness']['state'], 'unknown')

    def test_unsupported_tsv_header_is_not_silently_interpreted(self):
        for header in ('step\tloss\tloss', 'epoch\tloss', 'step\tloss\tauc'):
            self.path.write_text(header + '\n0\t0.1\n')
            with self.subTest(header=header), self.assertRaises(metrics.MetricsError):
                self.report(input_format='dvclive')

    def test_large_file_uses_only_contiguous_suffix(self):
        self.write([self.row(i, float(i)) for i in range(30)])
        with patch.object(metrics, 'MAX_BYTES', 700):
            report = self.report()
        self.assertIsNone(report['source']['sha256'])
        self.assertFalse(report['coverage']['history_complete'])
        self.assertEqual(report['series'][0]['latest']['step'], 29)
        self.assertGreater(report['series'][0]['minimum']['step'], 20)

    def test_large_tsv_retains_header_but_not_fake_continuity(self):
        self.path.write_text('step\tloss\n'+''.join(f'{i}\t{i/100}\n' for i in range(100)))
        with patch.object(metrics, 'MAX_BYTES', 200):
            report = self.report(input_format='dvclive')
        self.assertEqual(report['series'][0]['latest']['step'], 99)
        self.assertGreater(report['series'][0]['minimum']['step'], 80)

    def test_points_rows_series_and_line_budgets_are_visible(self):
        self.write([self.row(i, i) for i in range(6)])
        with patch.object(metrics, 'MAX_POINTS', 3):
            report = self.report()
        self.assertEqual(report['series'][0]['omitted_earlier_points'], 3)
        self.assertFalse(report['coverage']['history_complete'])
        with patch.object(metrics, 'MAX_ROWS', 2):
            report = self.report()
        self.assertGreater(report['coverage']['unprocessed_window_bytes'], 0)
        self.write([self.row(0, 1, run_id=f'r{i}') for i in range(5)])
        with patch.object(metrics, 'MAX_SERIES', 2):
            report = self.report()
        self.assertEqual(report['coverage']['diagnostics']['series-budget-exhausted-rows'], 3)
        with patch.object(metrics, 'MAX_LINE_BYTES', 10):
            report = self.report()
        self.assertEqual(report['coverage']['diagnostics']['oversize-row'], 5)

    def test_empty_file_is_not_a_training_success(self):
        self.write([])
        report = self.report()
        self.assertEqual(report['series'], [])
        self.assertIn('not a success verdict', metrics.render(report))

    def test_explicit_file_is_never_executed_or_modified(self):
        self.write([self.row()]); before = self.path.read_bytes()
        with patch('subprocess.run', side_effect=AssertionError('unexpected process')):
            report = self.report()
        self.assertEqual(self.path.read_bytes(), before)
        self.assertEqual([p.name for p in self.repo.iterdir()], ['metrics.jsonl'])
        self.assertNotIn('sbatch', report)

    def test_file_safety_is_shared_with_inspection_reader(self):
        self.write([self.row()]); (self.repo/'linked').symlink_to(self.path)
        for path in ('linked', '../metrics.jsonl'):
            with self.subTest(path=path), self.assertRaises(ReadError):
                metrics.inspect_metrics(self.repo, path)

    def test_option_errors_do_not_trigger_file_read(self):
        for options in ({'stale_after': 0}, {'window': 201}, {'direction': 'best'},
                        {'metric': '\x1b'}, {'stale_after': True}):
            with patch.object(metrics, 'read_segments', side_effect=AssertionError('read')), self.assertRaises(metrics.MetricsError):
                self.report(**options)

    def test_invalid_clock_and_dense_blank_file_are_bounded(self):
        self.write([self.row()])
        for clock in (-1, True, 10**1000, float('inf'), '2026-11-10T00:00:00Z'):
            with self.subTest(clock_type=type(clock).__name__), self.assertRaises(metrics.MetricsError):
                metrics.inspect_metrics(self.repo, self.path, now=clock)
        self.path.write_bytes(b'\n' * 100000)
        with patch.object(metrics, 'MAX_ROWS', 3):
            report = self.report()
        self.assertEqual(report['coverage']['rows_seen'], 3)
        self.assertEqual(report['coverage']['unprocessed_window_bytes'], 99997)

    def shell(self, *args):
        return subprocess.run(['bash', str(ROOT/'scripts/job-digest.sh'), *args], cwd=self.repo,
                              env=dict(os.environ, PYTHONDONTWRITEBYTECODE='1'),
                              text=True, capture_output=True, timeout=15)

    def test_real_job_digest_metrics_only_json_and_text(self):
        self.write([self.row(0, 2), self.row(1, 1)])
        before = self.path.read_bytes()
        result = self.shell('--metrics', 'metrics.jsonl', '--metric', 'train/loss',
                            '--metric-direction', 'minimize', '--metrics-json')
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report['series'][0]['best']['value'], 1)
        result = self.shell('--metrics', 'metrics.jsonl')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Recorded training metrics', result.stdout)
        self.assertEqual(self.path.read_bytes(), before)
        self.assertFalse((self.repo/'.oms').exists())

    def test_real_shell_combined_log_and_metrics_preserves_sections(self):
        self.write([self.row()]); (self.repo/'train.log').write_text('ERROR fixture\n')
        result = self.shell('train.log', '--metrics', 'metrics.jsonl')
        self.assertEqual(result.returncode, 0, result.stderr)
        for section in ('Error patterns', 'Last traceback', 'Tail', 'Recorded training metrics'):
            self.assertIn(section, result.stdout)
        result = self.shell('--metrics', 'metrics.jsonl', '--max-bytes', '512')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertLessEqual(len(result.stdout.encode()), 512)

    def test_real_shell_rejects_ambiguous_and_missing_options(self):
        self.write([self.row()]); (self.repo/'train.log').write_text('hello\n')
        for args in (('--metric', 'loss'), ('--metrics',), ('--metrics', 'metrics.jsonl', '--wait'),
                     ('--metrics', 'metrics.jsonl', '--metric-direction', 'minimize'),
                     ('train.log', '--metrics', 'metrics.jsonl', '--metrics-json'),
                     ('--metrics', 'metrics.jsonl', '--stale-after', '0'),
                     ('--metrics', 'metrics.jsonl', '--metrics-json', '--max-bytes', '512')):
            with self.subTest(args=args):
                self.assertEqual(self.shell(*args).returncode, 2)


if __name__ == '__main__':
    unittest.main(verbosity=2)
