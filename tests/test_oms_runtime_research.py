from __future__ import annotations
import json
import os
import stat
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
from oms_runtime import capsule, context, evidence, experiment, failures, profiles, release
from oms_runtime.benchmark import record_outcome as record_benchmark_outcome
from oms_runtime.benchmark import snapshot as benchmark_snapshot
from oms_runtime.common import CoreError, append_jsonl, atomic_write_bytes, atomic_write_json, canonical_json, git_head, parse_path_list, read_json, read_jsonl, sensitive_text, sha256_bytes, sha256_file, sha256_text
from oms_runtime.execution import check as check_backend
from oms_runtime.execution import run as run_backend

from runtime_test_base import RuntimeFixtureBase

class RuntimeFixture(RuntimeFixtureBase):

    def test_evidence_revocation_is_append_only(self) -> None:
        bound = evidence.bind(self.repo, 'project-safe', 'evt-api', 'verified')
        self.assertEqual({item['id']: item['status'] for item in evidence.build_coverage(self.repo)['criteria']}['project-safe'], 'verified')
        revoked = evidence.revoke(self.repo, bound['binding_id'], reason='superseded by a stronger proof')
        self.assertEqual(revoked['action'], 'revoke')
        self.assertEqual({item['id']: item['status'] for item in evidence.build_coverage(self.repo)['criteria']}['project-safe'], 'missing')
        ledger = (self.repo / '.oms' / 'evidence' / 'bindings.jsonl').read_text(encoding='utf-8').splitlines()
        self.assertEqual(len(ledger), 2)

    def test_context_phase_and_verify_heading_are_exact(self) -> None:
        task = self.repo / '.oms' / 'task' / 'current.md'
        task.write_text(task.read_text(encoding='utf-8') + '\n## Verification\n\nold prose receipt\n', encoding='utf-8')
        envelope = evidence.build_envelope(self.repo)
        self.assertEqual(envelope['task']['verify_digest'], sha256_text('python3 -m unittest'))
        manifest = context.plan_context(self.repo, target='scripts/sample.py', phase='review', max_bytes=32768)
        self.assertEqual(manifest['phase'], 'review')

    def test_manual_effectiveness_outcomes_are_aggregated(self) -> None:
        record_benchmark_outcome(self.repo, task_id='task-fixture', status='verified', human_corrections=1, escaped_defects=2, reverted_lines=3, false_refusals=4, duplicate_work=5, note='post-merge audit')
        stored = read_jsonl(self.repo / '.oms' / 'runtime' / 'outcomes.jsonl')[-1]
        self.assertNotIn('note', stored)
        self.assertTrue(stored['note_digest'])
        row = benchmark_snapshot(self.repo)
        self.assertEqual(row['manual_outcomes']['count'], 1)
        self.assertEqual(row['manual_outcomes']['totals']['duplicate_work'], 5)
        self.assertEqual(row['unknown_metrics'], [])

    def test_install_plan_points_to_selective_installer(self) -> None:
        plan = profiles.install_plan(['core', 'notion'], 'claude')
        self.assertEqual(plan['selective_installer'], 'scripts/install-profile.sh')
        self.assertIn('ntn', plan['managed_tools'])
        self.assertEqual(plan['command'][:2], ['oms', 'install-profile'])

    def test_latest_fresh_evidence_supersedes_an_older_failure(self) -> None:
        index = self.repo / '.oms' / 'artifacts' / 'index.jsonl'
        append_jsonl(index, {'schema': 1, 'event_id': 'evt-safe-failed', 'kind': 'verify', 'status': 'failed', 'ts': '2026-08-15T00:00:00Z', 'covers': ['project-safe']})
        append_jsonl(index, {'schema': 1, 'event_id': 'evt-safe-verified', 'kind': 'verify', 'status': 'verified', 'ts': '2026-08-15T00:01:00Z', 'covers': ['project-safe']})
        criteria = {item['id']: item for item in evidence.build_coverage(self.repo)['criteria']}
        self.assertEqual(criteria['project-safe']['status'], 'verified')
        self.assertEqual(criteria['project-safe']['evidence'][-1]['evidence_ref'], 'evt-safe-verified')

    def test_zero_exit_without_fresh_metrics_is_not_a_success(self) -> None:
        experiment.register(self.repo, self._experiment(), 'exp-missing-metrics')
        metrics = self.repo / 'missing-metrics.json'
        row, rc = experiment.record_run(self.repo, 'exp-missing-metrics', 'baseline', 0, metrics, [sys.executable, '-c', "print('completed without metrics')"], timeout_seconds=10)
        self.assertEqual(row['command_exit'], 0)
        self.assertEqual(rc, 3)
        self.assertFalse(row['metrics_valid'])
        self.assertIn('missing', row['metric_error'])

    def test_required_invariant_results_control_experiment_verdict(self) -> None:
        contract = self._experiment()
        contract['invariant_pack'] = [{'name': 'equivariance', 'command': [sys.executable, '-c', 'raise SystemExit(0)'], 'required': True}]
        results = {'baseline': {'score': [1.0], 'memory': [10.0]}, 'treatment': {'score': [1.2], 'memory': [10.3]}}
        self.assertEqual(experiment.evaluate(contract, results)['verdict'], 'inconclusive')
        results['invariants'] = {'equivariance': False}
        self.assertEqual(experiment.evaluate(contract, results)['verdict'], 'not_supported')
        results['invariants'] = {'equivariance': True}
        self.assertEqual(experiment.evaluate(contract, results)['verdict'], 'supported')

    def test_remote_adapter_must_bind_the_operation_id(self) -> None:
        adapter = Path(self.tmp.name) / 'bad-adapter.py'
        adapter.write_text("#!/usr/bin/env python3\nimport json,sys\nrequest=json.load(sys.stdin)\nprint(json.dumps({'schema': 1, 'operation_id': 'wrong-operation', 'accepted': True}))\n", encoding='utf-8')
        adapter.chmod(493)
        receipt, rc = run_backend('remote', self.repo, ['echo', 'remote'], adapter=str(adapter), timeout_seconds=10)
        self.assertEqual(rc, 125)
        self.assertFalse(receipt['remote_response_valid'])
        self.assertIsNone(receipt['remote_accepted'])

    def test_remote_adapter_rejects_false_isolation_attestation(self) -> None:
        adapter = Path(self.tmp.name) / 'false-attestation-adapter.py'
        adapter.write_text("#!/usr/bin/env python3\nimport json,sys\nrequest=json.load(sys.stdin)\nprint(json.dumps({'schema': 1, 'operation_id': request['operation_id'], 'accepted': True, 'exit': 0, 'attestation': {'transport_authenticated': True, 'filesystem_isolated': False, 'cleanup_confirmed': True}}))\n", encoding='utf-8')
        adapter.chmod(493)
        receipt, rc = run_backend('remote', self.repo, ['echo', 'remote'], adapter=str(adapter), timeout_seconds=10)
        self.assertEqual(rc, 125)
        self.assertFalse(receipt['remote_response_valid'])
        self.assertFalse(receipt['adapter_attestation_valid'])

    def test_capsule_import_is_idempotent_and_refuses_corrupt_existing_state(self) -> None:
        exported = capsule.export(self.repo, self.repo / 'portable.json')
        first = capsule.import_capsule(self.repo, self.repo / 'portable.json')
        second = capsule.import_capsule(self.repo, self.repo / 'portable.json')
        self.assertTrue(first['imported'])
        self.assertFalse(first['idempotent'])
        self.assertFalse(second['imported'])
        self.assertTrue(second['idempotent'])
        destination = self.repo / second['path']
        destination.write_text('{"schema":1,"capsule":{}}\n', encoding='utf-8')
        with self.assertRaises(CoreError) as raised:
            capsule.import_capsule(self.repo, self.repo / 'portable.json')
        self.assertEqual(raised.exception.exit_code, 75)
        self.assertEqual(exported['capsule_id'], first['capsule_id'])

    def test_state_writer_rejects_symlinked_oms_directory(self) -> None:
        external = Path(self.tmp.name) / 'external-state'
        external.mkdir()
        oms = self.repo / '.oms'
        shutil.rmtree(oms)
        oms.symlink_to(external, target_is_directory=True)
        with self.assertRaises(CoreError):
            atomic_write_json(oms / 'runtime' / 'unsafe.json', {'unsafe': True})
        self.assertEqual(list(external.iterdir()), [])

    def test_release_promotion_requires_an_explicit_manifest_digest(self) -> None:
        with self.assertRaises(CoreError):
            release.promote(self.repo, git_head(self.repo), '1.0.1', expected_manifest_digest='')

    def test_python_sources_parse_with_python39_grammar(self) -> None:
        import ast
        roots = [ROOT / 'scripts' / 'lib' / 'oms_runtime', ROOT / 'scripts' / 'lib']
        checked = 0
        for root in roots:
            for path in sorted(root.glob('*.py')):
                checked += 1
                ast.parse(path.read_text(encoding='utf-8'), filename=str(path), feature_version=9)
        self.assertGreater(checked, 10)

    def test_plan_acceptance_receipt_is_linked_and_becomes_stale_after_head_moves(self) -> None:
        plan = read_json(self.repo / '.oms' / 'plan' / 'tasks.json')
        accept_digest = sha256_text(plan['accept'])
        append_jsonl(self.repo / '.oms' / 'plan' / 'progress.jsonl', {'schema': 1, 'kind': 'acceptance', 'status': 'pass', 'exit': 0, 'ts': '2026-08-15T00:02:00Z', 'base_sha': git_head(self.repo), 'accept_sha256': accept_digest[:16]})
        criteria = {item['id']: item for item in evidence.build_coverage(self.repo)['criteria']}
        plan_id = next((item_id for item_id, item in criteria.items() if item['source'] == 'plan'))
        self.assertEqual(criteria[plan_id]['status'], 'verified')
        (self.repo / 'new.txt').write_text('move head\n', encoding='utf-8')
        subprocess.run(['git', '-C', str(self.repo), 'add', 'new.txt'], check=True)
        subprocess.run(['git', '-C', str(self.repo), 'commit', '-qm', 'move head'], check=True)
        criteria = {item['id']: item for item in evidence.build_coverage(self.repo)['criteria']}
        self.assertEqual(criteria[plan_id]['status'], 'stale')

    def test_target_is_required_context_and_truncation_creates_debt(self) -> None:
        target = self.repo / 'scripts' / 'large_target.py'
        target.write_text("VALUE = '" + 'x' * 20000 + "'\n", encoding='utf-8')
        manifest = context.plan_context(self.repo, target='scripts/large_target.py', max_bytes=8192)
        self.assertIn('scripts/large_target.py', manifest['truncated_required'])
        self.assertFalse(manifest['sufficient'])

    def test_council_profile_includes_core_and_remote_uses_environment_adapter(self) -> None:
        plan = profiles.install_plan(['council'], 'codex')
        self.assertIn('core', plan['expanded'])
        self.assertEqual(len([name for name in plan['managed_tools'] if name in ('codex', 'claude', 'agy')]), 2)
        adapter = Path(self.tmp.name) / 'env-adapter.py'
        adapter.write_text("#!/usr/bin/env python3\nimport json,sys\nrequest=json.load(sys.stdin)\nprint(json.dumps({'schema':1,'operation_id':request['operation_id'],'accepted':True,'exit':0,'attestation':{'transport_authenticated':True,'filesystem_isolated':True,'cleanup_confirmed':True}}))\n", encoding='utf-8')
        adapter.chmod(493)
        previous = os.environ.get('OMS_REMOTE_ADAPTER')
        os.environ['OMS_REMOTE_ADAPTER'] = str(adapter)
        try:
            self.assertTrue(check_backend('remote')['ready'])
            receipt, rc = run_backend('remote', self.repo, ['echo', 'remote'], timeout_seconds=10)
            self.assertEqual(rc, 0)
            self.assertTrue(receipt['adapter_attestation_valid'])
        finally:
            if previous is None:
                os.environ.pop('OMS_REMOTE_ADAPTER', None)
            else:
                os.environ['OMS_REMOTE_ADAPTER'] = previous

    def test_experiment_id_cannot_escape_contract_directory(self) -> None:
        with self.assertRaises(CoreError):
            experiment.load_contract(self.repo, '../../outside')
        with self.assertRaises(CoreError):
            experiment.register(self.repo, self._experiment(), 'bad:id')

    def test_effective_scope_uses_the_most_specific_allowed_layer(self) -> None:
        plan = read_json(self.repo / '.oms' / 'plan' / 'tasks.json')
        plan['tasks'][0]['allowed'] = ['scripts/sample.py']
        atomic_write_json(self.repo / '.oms' / 'plan' / 'tasks.json', plan)
        envelope = evidence.build_envelope(self.repo)
        self.assertEqual(envelope['scope']['allowed'], ['scripts/sample.py'])
        self.assertEqual(envelope['scope']['allowed_source'], 'plan')
        self.assertFalse(envelope['scope']['authoritative'])
