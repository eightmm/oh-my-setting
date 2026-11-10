"""Recorded input identities and replica grouping, with bounded shell fixtures."""
from __future__ import annotations

from copy import deepcopy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('run_ledger_metrics', ROOT / 'scripts/lib/run_ledger_metrics.py')
metrics = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(metrics)
ERROR = metrics.ContractError


def identity(cfg='a', data='b', expected=True):
    configs = [{'path': 'config.json', 'sha256': cfg * 32}] if cfg else []
    datasets = [{'name': 'dataset', 'manifest_sha256': data * 64}] if data else []
    return metrics.make_input_identity({'commit_full': 'c' * 40, 'dirty': 0, 'diff_sha256': ''},
                                       configs, datasets, ['config.json'] if expected else [])


def row(seed=1, inputs=None, contract='d', group='replicate'):
    value = {'cmd': ['train', '--seed', str(seed)], 'seed': seed, 'group': group,
             'git_sha': 'c' * 40, 'dirty': 0, 'dirty_hash': '', 'exit': 0,
             'metric_contract': {'loss': {'digest': contract * 64, 'direction': 'min'}}}
    if inputs is not None:
        value['input_identity'] = inputs
    return value


class IdentityTests(unittest.TestCase):
    def test_identity_is_deterministic_value_free_and_no_io(self):
        with patch('builtins.open', side_effect=AssertionError('no source reads')):
            result = identity()
            self.assertEqual(result, identity())
            self.assertEqual(metrics.validate_input_identity(result), result)
        encoded = json.dumps(result)
        self.assertNotIn('config.json', encoded)
        self.assertNotIn('dataset', encoded)
        self.assertEqual(result['config_status'], 'recorded')
        self.assertEqual(result['data_status'], 'recorded')
        self.assertEqual(len(result['digest']), 64)

    def test_input_order_is_irrelevant_and_missing_config_is_incomplete(self):
        git = {'commit_full': 'a' * 40, 'dirty': 1, 'diff_sha256': 'b' * 16}
        configs = [{'path': 'a', 'sha256': 'c' * 32}, {'path': 'b', 'sha256': 'd' * 64}]
        data = [{'name': 'a', 'manifest_sha256': 'e' * 64}, {'name': 'b', 'manifest_sha256': 'f' * 64}]
        first = metrics.make_input_identity(git, configs, data, ['a', 'b'])
        self.assertEqual(first, metrics.make_input_identity(git, configs[::-1], data[::-1], ['b', 'a']))
        incomplete = metrics.make_input_identity(git, configs[:1], data, ['a', 'b'])
        self.assertEqual(incomplete['config_status'], 'incomplete')
        self.assertIsNone(incomplete['config_sha256'])
        self.assertEqual(metrics.validate_input_identity(incomplete), incomplete)

    def test_config_data_and_git_all_contribute(self):
        first = identity()
        self.assertNotEqual(first['digest'], identity(cfg='d')['digest'])
        self.assertNotEqual(first['digest'], identity(data='d')['digest'])
        self.assertEqual(first['data_sha256'], identity(cfg='d')['data_sha256'])
        unknown = identity(cfg=None, data=None, expected=False)
        self.assertIsNone(unknown['config_sha256'])
        self.assertIsNone(unknown['data_sha256'])
        self.assertEqual(unknown['data_status'], 'unrecorded')

    def test_malformed_identity_is_not_a_matching_scope(self):
        variants = [None, [], {}, {'schema': 1}]
        for key, value in (('schema', True), ('data_status', []), ('data_sha256', 'bad'),
                           ('recorded_configs', True), ('capture', 'after-command'), ('digest', 'f' * 64)):
            bad = identity(); bad[key] = value; variants.append(bad)
        bad = identity(); bad['git']['commit'] = 'bad'; variants.append(bad)
        for value in variants:
            with self.subTest(value=value), self.assertRaises((ERROR, TypeError, ValueError)):
                metrics.validate_input_identity(value)

    def test_duplicate_conflict_invalid_source_and_bounds(self):
        git = {'commit_full': 'c' * 40, 'dirty': 0, 'diff_sha256': ''}
        for configs in ([{'path': 'x', 'sha256': 'a' * 32}, {'path': 'x', 'sha256': 'b' * 32}],
                        [{'path': 'x\x1b', 'sha256': 'a' * 32}], [{'path': 'x', 'sha256': 'bad'}],
                        [{}] * 129):
            with self.subTest(configs=configs), self.assertRaises(ERROR):
                metrics.make_input_identity(git, configs, [], ['x'])

    def test_data_comparability_is_separate_from_config_variation(self):
        rows = [(1, row(inputs=identity())), (2, row(2, identity(cfg='e')))]
        self.assertEqual(metrics.data_comparable(rows), (rows, {}))
        for other in (identity(data='f'), None, identity(cfg=None, data=None, expected=False)):
            candidate = [(1, row(inputs=identity())), (2, row(2, other))]
            kept, why = metrics.data_comparable(candidate)
            self.assertEqual(kept, [])
            self.assertTrue(why)
        legacy = [(1, row()), (2, row(2))]
        self.assertEqual(metrics.data_comparable(legacy), (legacy, {}))

    def test_data_digest_prefix_collision_is_not_equality(self):
        first, second = identity(), identity()
        for value, suffix in ((first, 'a'), (second, 'b')):
            value['data_sha256'] = '1' * 12 + suffix * 52
            value['digest'] = metrics._canonical_digest({key: item for key, item in value.items() if key != 'digest'})
        self.assertEqual(metrics.data_comparable([(1, row(inputs=first)), (2, row(2, second))])[0], [])

    def test_invalid_identity_is_counted_not_accepted(self):
        valid = (1, row(inputs=identity()))
        invalid = row(2); invalid['input_identity'] = None
        kept, excluded = metrics.data_comparable([valid, (2, invalid)])
        self.assertEqual(kept, [valid])
        self.assertEqual(excluded, {'invalid input identity': 1})

    def test_capture_bridge_is_bounded_and_pins_precommand_contract(self):
        contract = {'loss': {'direction': 'min', 'digest': 'a' * 64}}
        captured = metrics.capture_for_ledger({'commit_full': 'c' * 40, 'dirty': 0, 'diff_sha256': ''},
                                             [], [], [], contract, ['7'], 'capture-1')
        self.assertEqual(metrics.decode_capture(json.dumps(captured)), captured)
        before = deepcopy(captured)
        output = metrics.attach_capture(row(seed=7, contract='b'), captured)
        self.assertEqual(output['metric_contract'], contract)
        self.assertEqual(output['seed'], 7)
        self.assertEqual(output['seed_status'], 'declared')
        self.assertEqual(output['capsule_id'], 'capture-1')
        self.assertEqual(captured, before)
        captured['metric_contract'] = None
        self.assertNotIn('metric_contract', metrics.attach_capture(row(seed=7), captured))

    def test_ambiguous_seed_cannot_be_used_as_one_replicate(self):
        captured = {'inputs': identity(), 'capsule_id': 'capture', 'metric_contract': None}
        for seeds, commandseed, status in ((['1', '2'], 1, 'multiple-or-unrecognized'),
                                           (['bad'], 1, 'multiple-or-unrecognized'),
                                           (['2'], 1, 'conflicting-declarations')):
            with self.subTest(seeds=seeds):
                output = metrics.attach_capture(row(seed=commandseed), dict(captured, seeds=seeds))
                self.assertNotIn('seed', output)
                self.assertNotIn('group', output)
                self.assertEqual(output['seed_status'], status)
        output = metrics.attach_capture(row(seed=1), dict(captured, seeds=[]))
        self.assertEqual(output['seed_status'], 'command')

    def test_bad_bridge_and_contract_fail_closed(self):
        for raw in ('{}', '{"schema":1,"schema":1}', '{"schema":NaN}', '[' * 2000, 'x' * 65537):
            with self.subTest(size=len(raw)), self.assertRaises(ERROR):
                metrics.decode_capture(raw)
        for value in ([], {'loss': {'digest': 'a' * 64, 'direction': 'sideways'}},
                      {'loss': {'digest': 'a' * 64, 'direction': 'min', 'eval_sha256': 'bad'}},
                      {'loss': {'digest': 'a' * 64, 'direction': 'min', 'command': 'never copied'}}):
            with self.subTest(value=value), self.assertRaises(ERROR):
                metrics.captured_contract(value)


class GroupingTests(unittest.TestCase):
    def setUp(self):
        # Bootstrap implementation is unchanged; assert which values it receives.
        self.values = []
        package = types.ModuleType('oms_runtime')
        run = types.ModuleType('oms_runtime.experiment_run')
        def capture(values, seed):
            self.values.append(list(values))
            return min(values), max(values)
        run._bootstrap_ci = capture
        self.stub = patch.dict(sys.modules, {'oms_runtime': package, 'oms_runtime.experiment_run': run})
        self.stub.start(); self.addCleanup(self.stub.stop)

    def test_latest_append_wins_not_best_score(self):
        summaries, loose = metrics.group_summaries([(0.9, row(1)), (0.1, row(2)), (0.5, row(1))], 'loss')
        self.assertEqual(loose, 0)
        self.assertEqual(summaries[0][1:3], (2, 0.3))
        self.assertEqual(self.values, [[0.5, 0.1]])

    def test_same_label_does_not_pool_different_configs_data_or_contracts(self):
        for other, contract in ((identity(cfg='e'), 'd'), (identity(data='e'), 'd'), (identity(), 'e'), (None, 'd')):
            with self.subTest(other=other, contract=contract):
                summaries, _ = metrics.group_summaries([(1, row(1, identity())), (2, row(2, other, contract))], 'loss')
                self.assertEqual(len(summaries), 2)
                self.assertTrue(all(item[1] == 1 and '@' in item[0] for item in summaries))

    def test_incomplete_unknown_git_invalid_and_nonfinite_are_not_replicates(self):
        bad = row(); bad['input_identity'] = None
        incomplete = identity(cfg=None, expected=True)
        no_git = metrics.make_input_identity({'commit_full': 'none', 'dirty': 0, 'diff_sha256': ''}, [], [], [])
        rows = [(1, bad), (2, row(2, incomplete)), (3, row(3, no_git)), (float('nan'), row(4))]
        self.assertEqual(metrics.group_summaries(rows, 'loss'), ([], 4))


class ShellIntegration(unittest.TestCase):
    """Actual capsule and ledger scripts; environment probes/library adapters are fixture stand-ins."""
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='oms-run-identity-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.install = self.root / 'install'
        self.scripts = self.install / 'scripts'
        lib = self.scripts / 'lib'
        lib.mkdir(parents=True)
        for path in ('run-capsule.sh', 'run-ledger.sh'):
            shutil.copyfile(ROOT / 'scripts' / path, self.scripts / path)
            (self.scripts / path).chmod(0o700)
        shutil.copyfile(ROOT / 'scripts/lib/run_ledger_metrics.py', lib / 'run_ledger_metrics.py')
        (lib / 'agent-memory-common.sh').write_text('''
oms_repo_root() { git rev-parse --show-toplevel; }
oms_sha256_file() { python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$1"; }
oms_sha256_stream() { python3 -c 'import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())'; }
oms_with_file_lock() { shift; "$@"; }
oms_effective_run_id() { return 1; }
oms_detect_agent() { printf 'fixture\\n'; }
agent_memory_ensure_oms_ignore_for_path() { :; }
agent_memory_file_has_sensitive_content() { return 1; }
''')
        (lib / 'oms-common.sh').write_text(':\n')
        (lib / 'work-journal.sh').write_text('work_journal_observe() { :; }\n')
        package = lib / 'oms_runtime'; package.mkdir()
        (package / '__init__.py').write_text('')
        (package / 'experiment_run.py').write_text('def _bootstrap_ci(values, seed):\n    return min(values), max(values)\n')
        self.repo = self.root / 'repo'; self.repo.mkdir()
        subprocess.run(['git', 'init', '-q', str(self.repo)], check=True)
        (self.repo / '.gitignore').write_text('.oms/\n.venv/\nmetrics.json\n')
        (self.repo / 'config.json').write_text('{"lr": 0.01}')
        (self.repo / 'docs').mkdir()
        self.contract = self.repo / 'docs/metric-contract.json'
        self.contract.write_text(json.dumps({'schema': 1, 'metrics': {'loss': {'direction': 'min', 'split': 'val'}}}))
        subprocess.run(['git', '-C', str(self.repo), 'add', '.'], check=True)
        subprocess.run(['git', '-C', str(self.repo), '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                        'commit', '-qm', 'fixture'], check=True)
        manifests = self.repo / '.oms/manifests'; manifests.mkdir(parents=True)
        (manifests / 'ds.json').write_text(json.dumps({'schema': 1, 'splits': [{'label': 'train', 'rows': 4}]}))
        py = self.repo / '.venv/bin/python'; py.parent.mkdir(parents=True)
        # capture_env_json is unrelated to input comparison: no torch/pip/GPU/environment probes.
        py.write_text('#!/bin/sh\ncat >/dev/null\nprintf \'{"python":"fixture"}\\n\'\n'); py.chmod(0o700)
        bindir = self.root / 'bin'; bindir.mkdir()
        uv = bindir / 'uv'; uv.write_text('#!/bin/sh\nprintf "fixture==1\\n"\n'); uv.chmod(0o700)
        self.env = dict(os.environ, PATH=str(bindir) + os.pathsep + os.environ['PATH'], PYTHONDONTWRITEBYTECODE='1',
                        OMS_WORK_JOURNAL_SUPPRESS='1', CONDA_DEFAULT_ENV='')
        for key in list(self.env):
            if key.startswith(('GIT_CONFIG_', 'OMS_RUN_', 'OMS_RL_', 'OMS_CAPTURE', 'OMS_RESEARCH_')) or key == 'BASH_ENV':
                self.env.pop(key, None)
        self.ledger = self.repo / 'docs/EXPERIMENTS.jsonl'

    def run_script(self, name, *args, env=None):
        return subprocess.run(['bash', str(self.scripts / name), *args], cwd=self.repo,
                              env=dict(self.env, **(env or {})), capture_output=True, text=True, timeout=15)

    def test_capsule_shares_precommand_inputs_seed_and_contract_with_ledger(self):
        old = metrics.row_contract(str(self.ledger), str(self.repo), lambda _: None)
        before = subprocess.check_output(['git', '-C', str(self.repo), 'rev-parse', 'HEAD'], text=True).strip()
        command = ('from pathlib import Path; import json; '
                   'Path("config.json").write_text("changed during run"); '
                   'Path("docs/metric-contract.json").write_text(json.dumps({"schema":1,"metrics":{"loss":{"direction":"max","split":"test"}}})); '
                   'Path("metrics.json").write_text("{\\"loss\\": 0.4}"); Path("executed").write_text("once")')
        done = self.run_script('run-capsule.sh', 'run', '--config', 'config.json', '--data', 'ds', '--seed', '7',
                               '--metrics', 'metrics.json', '--', sys.executable, '-c', command)
        self.assertEqual(done.returncode, 0, done.stderr)
        cap_id = done.stdout.strip()
        cap = json.loads((self.repo / '.oms/runs' / cap_id / 'capsule.json').read_text())
        recorded = json.loads(self.ledger.read_text().strip())
        self.assertEqual(cap['input_identity'], recorded['input_identity'])
        self.assertEqual(cap['metric_contract'], old)
        self.assertEqual(recorded['metric_contract'], old)
        self.assertEqual(recorded['git_sha'], before)
        self.assertEqual(recorded['seed'], 7)
        self.assertEqual(recorded['capsule_id'], cap_id)
        self.assertNotIn('config.json', json.dumps(recorded['input_identity']))
        self.assertEqual((self.repo / 'executed').read_text(), 'once')

    def test_multiple_declared_seeds_do_not_create_fake_scalar_replication(self):
        result = self.run_script('run-capsule.sh', 'run', '--seed', '1', '--seed', '2', '--', 'true', '--seed', '1')
        self.assertEqual(result.returncode, 0, result.stderr)
        record = json.loads(self.ledger.read_text())
        self.assertEqual(record['seed_status'], 'multiple-or-unrecognized')
        self.assertNotIn('seed', record)
        self.assertNotIn('group', record)

    def test_bad_capture_cannot_run_command_or_append_row(self):
        command = [sys.executable, '-c', 'from pathlib import Path;Path("must-not-exist").touch()']
        for env in ({'OMS_RUN_LEDGER_CAPTURED': '{}'},
                    {'OMS_RUN_LEDGER_CAPTURED': '{}', 'OMS_RUN_LEDGER_STATUS_OVERRIDE': '0'}):
            with self.subTest(env=env):
                done = self.run_script('run-ledger.sh', '--', *command, env=env)
                self.assertEqual(done.returncode, 2)
                self.assertFalse((self.repo / 'must-not-exist').exists())
                self.assertFalse(self.ledger.exists())

    def write_rows(self, values):
        records = []
        for value, record in values:
            item = dict(record, metrics={'loss': value})
            item.pop('metric_contract', None)
            records.append(item)
        self.contract.unlink(missing_ok=True)
        self.ledger.write_text(''.join(json.dumps(item) + '\n' for item in records))

    def test_real_top_reconciles_append_order_before_sort_for_both_directions(self):
        self.write_rows([(0.2, row(1)), (0.8, row(2)), (0.6, row(1))])
        for direction in ('--min', '--max'):
            with self.subTest(direction=direction):
                done = self.run_script('run-ledger.sh', 'top', '--metric', 'loss', '--by-group', direction)
                self.assertEqual(done.returncode, 0, done.stderr)
                self.assertIn('mean=0.7', done.stdout)
                self.assertIn('n=2', done.stdout)

    def test_real_top_refuses_data_mix_and_mixed_does_not_pool_replicates(self):
        self.write_rows([(0.2, row(1, identity())), (0.8, row(2, identity(data='f')))])
        denied = self.run_script('run-ledger.sh', 'top', '--metric', 'loss', '--by-group')
        self.assertEqual(denied.returncode, 4, denied.stderr)
        self.assertIn('data identities', denied.stderr)
        mixed = self.run_script('run-ledger.sh', 'top', '--metric', 'loss', '--by-group', '--mixed')
        self.assertEqual(mixed.returncode, 0, mixed.stderr)
        self.assertEqual(mixed.stdout.count('n=1'), 2)
        self.assertNotIn('n=2', mixed.stdout)
        self.assertIn('--mixed', mixed.stderr)

    def test_legacy_direct_run_still_records_without_capsule_fields(self):
        done = self.run_script('run-ledger.sh', '--seed', '3', '--', 'true')
        self.assertEqual(done.returncode, 0, done.stderr)
        record = json.loads(self.ledger.read_text())
        self.assertEqual(record['seed'], 3)
        self.assertNotIn('input_identity', record)
        self.assertNotIn('capsule_id', record)
        self.assertIn('metric_contract', record)


if __name__ == '__main__':
    unittest.main(verbosity=2)
