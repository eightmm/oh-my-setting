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
from oms_runtime import capsule, context, evidence, experiment, failures, profiles, projection, release
from oms_runtime.benchmark import record_outcome as record_benchmark_outcome
from oms_runtime.benchmark import snapshot as benchmark_snapshot
from oms_runtime.common import CoreError, append_jsonl, atomic_write_bytes, atomic_write_json, canonical_json, git_head, parse_path_list, read_json, read_jsonl, sensitive_text, sha256_bytes, sha256_file, sha256_text
from oms_runtime.execution import check as check_backend
from oms_runtime.execution import run as run_backend

from runtime_test_base import RuntimeFixtureBase


class ChildRuntimePolicyTest(unittest.TestCase):

    def test_policy_classifies_every_runtime_action_fail_closed(self) -> None:
        from oms_runtime.child_policy import child_action_is_read_only
        from oms_runtime.cli_parser import build_parser

        allowed = (
            ['envelope', 'show'],
            ['evidence', 'show'],
            ['evidence', 'unbound'],
            ['next'],
            ['failure', 'classify', 'verification failed'],
            ['failure', 'catalog'],
            ['profile', 'list'],
            ['profile', 'current'],
            ['profile', 'show', 'core'],
            ['profile', 'check', 'core'],
            ['profile', 'install-plan', 'core'],
            ['profile', 'install', 'core', '--dry-run'],
            ['release', 'status'],
            ['release', 'resolve', 'stable'],
            ['capsule', 'verify', 'capsule.json'],
            ['capsule', 'diff', 'left.json', 'right.json'],
            ['backend', 'describe', 'trusted-local'],
            ['backend', 'check', 'isolated'],
            ['experiment', 'template'],
            ['experiment', 'validate', '{}'],
            ['experiment', 'compare', '{}', '{}'],
            ['experiment', 'show', 'experiment-id'],
            ['experiment', 'summarize', '--id', 'experiment-id'],
            ['experiment', 'evaluate', '{}', '{}'],
            ['benchmark', 'show'],
            ['benchmark', 'compare', 'left.json', 'right.json'],
            ['doctor'],
            ['doctor', '--strict'],
        )
        denied = (
            ['envelope', 'write'],
            ['evidence', 'bind', '--criterion', 'criterion', '--ref', 'event', '--status', 'verified'],
            ['evidence', 'revoke', '--binding', 'binding'],
            ['context'],
            ['profile', 'apply', 'core'],
            ['profile', 'install', 'core'],
            ['release', 'resolve', 'stable', '--fetch'],
            ['release', 'apply', 'stable'],
            ['release', 'promote', '--commit', 'a' * 40, '--version', '1.0.0', '--expected-manifest-digest', 'b' * 64],
            ['capsule', 'export'],
            ['capsule', 'import', 'capsule.json'],
            ['backend', 'run', 'trusted-local', '--', 'true'],
            ['experiment', 'template', '--output', 'experiment.json'],
            ['experiment', 'register', '{}'],
            ['experiment', 'run', '--id', 'experiment-id', '--arm', 'baseline', '--seed', '1', '--metrics', 'metrics.json', '--', 'true'],
            ['experiment', 'invariants', '{}'],
            ['benchmark', 'snapshot'],
            ['benchmark', 'record', '--task-id', 'task', '--status', 'verified'],
        )
        parser = build_parser()
        for argv in allowed:
            with self.subTest(kind='allowed', argv=argv):
                self.assertTrue(child_action_is_read_only(parser.parse_args(argv)))
        for argv in denied:
            with self.subTest(kind='denied', argv=argv):
                self.assertFalse(child_action_is_read_only(parser.parse_args(argv)))


class RuntimeFixture(RuntimeFixtureBase):

    def test_task_status_is_read_by_the_installed_verb(self) -> None:
        """A repository cannot answer the harness's question about itself.

        The status verb was resolved as REPO/scripts/agent-task.sh, so every
        project without that file got {} -- verification read 'unknown' and a
        stale packet never reported stale -- while any repository that did have
        one could dictate its own task status.
        """
        planted = self.repo / 'scripts' / 'agent-task.sh'
        planted.write_text(
            '#!/usr/bin/env bash\nprintf \'%s\\n\' '
            '\'{"schema":1,"present":true,"task_id":"planted","status":"blocked",'
            '"verification":"fresh","stale":true}\'\n', encoding='utf-8')
        planted.chmod(0o755)
        row = evidence.build_envelope(self.repo)
        task = row['task']
        self.assertEqual(task['task_id'], 'task-fixture')
        self.assertEqual(task['status'], 'verified')
        self.assertNotEqual(task['status'], 'blocked')

    def test_a_resolved_failure_leaves_the_envelope(self) -> None:
        """Resolution lives in its own ledger row, not in the failing one.

        The projection read each row's own status field, so nothing in the
        canonical ledger could ever close: resolved failures stayed listed and
        resolve_blocker outranked every other next action forever.
        """
        ledger = self.repo / '.oms' / 'failures.jsonl'
        append_jsonl(ledger, {
            'schema': 2, 'event': 'fail', 'ts': '2026-08-20T00:00:00Z', 'agent': 'claude',
            'fingerprint': 'aaaabbbbccccdddd', 'kind': 'cmd', 'cmd': 'make test',
            'exit': 1, 'summary': 'make test failed'})
        open_row = evidence.build_envelope(self.repo)
        self.assertEqual([item['id'] for item in open_row['failures']], ['aaaabbbbccccdddd'])
        self.assertIn('resolve_blocker', [item['id'] for item in open_row['next_actions']])
        append_jsonl(ledger, {
            'schema': 2, 'event': 'resolved', 'ts': '2026-08-20T01:00:00Z', 'agent': 'claude',
            'fingerprint': 'aaaabbbbccccdddd', 'how': 'fixed the build'})
        closed = evidence.build_envelope(self.repo)
        self.assertEqual(closed['failures'], [])
        self.assertNotIn('resolve_blocker', [item['id'] for item in closed['next_actions']])

    def test_canonical_projection_errors_never_become_absent_state(self) -> None:
        plan = self.repo / '.oms' / 'plan' / 'tasks.json'
        plan.write_text(json.dumps({
            'goal': 'invalid canonical task shape', 'accept': 'true', 'tasks': [],
        }), encoding='utf-8')
        with self.assertRaisesRegex(CoreError, 'canonical plan status'):
            evidence.build_envelope(self.repo)

        plan.write_text(json.dumps({
            'goal': 'valid again', 'accept': 'true',
            'tasks': {'t1': {'id': 't1', 'title': 'valid', 'state': 'ready'}},
        }), encoding='utf-8')
        # A corrupt ledger row is quarantined by the canonical projection, not
        # escalated: the envelope must survive (the remediation the inbox
        # names is the projection command itself), while state surfaces the
        # corruption through the failures projection's invalid_rows count.
        ledger = self.repo / '.oms' / 'failures.jsonl'
        ledger.write_text('{}\n', encoding='utf-8')
        envelope = evidence.build_envelope(self.repo)
        self.assertIsInstance(envelope, dict)

    def test_canonical_failure_ledger_symlink_is_not_absence(self) -> None:
        ledger = self.repo / '.oms' / 'failures.jsonl'
        outside = Path(self.tmp.name) / 'outside-failures.jsonl'
        outside.write_text('', encoding='utf-8')
        try:
            ledger.symlink_to(outside)
        except OSError as exc:
            self.skipTest('symlink unavailable: %s' % exc)
        with self.assertRaisesRegex(CoreError, 'regular non-symlink'):
            evidence.build_envelope(self.repo)

    def test_one_criterion_id_cannot_name_two_criteria(self) -> None:
        """The id is the key evidence binds to, so it cannot name two things.

        Deduplication was keyed by (id, text), which kept both rows and hid the
        collision: coverage collapses ids to a set, so one receipt marked both
        statements verified and the envelope reported full coverage for a
        criterion nobody had checked.
        """
        project = self.repo / 'PROJECT.md'
        original = project.read_text(encoding='utf-8')
        project.write_text(
            '# Demo\n\n## Goal\n\nShip it.\n\n## Acceptance Criteria\n\n'
            '- [id:safe] The public API stays compatible.\n'
            '- [id:safe] No data is lost.\n', encoding='utf-8')
        with self.assertRaises(CoreError) as caught:
            evidence.build_envelope(self.repo)
        self.assertIn('safe', str(caught.exception))
        # The same statement repeated is one criterion, not a collision.
        project.write_text(
            '# Demo\n\n## Goal\n\nShip it.\n\n## Acceptance Criteria\n\n'
            '- [id:safe] The public API stays compatible.\n'
            '- [id:safe] The public API stays compatible.\n', encoding='utf-8')
        row = evidence.build_envelope(self.repo)
        self.assertEqual([item['id'] for item in row['criteria'] if item['source'] == 'project'], ['safe'])
        project.write_text(original, encoding='utf-8')

    def test_envelope_and_coverage_are_conservative(self) -> None:
        row = evidence.build_envelope(self.repo)
        statuses = {item['id']: item['status'] for item in row['criteria']}
        self.assertEqual(statuses['project-api'], 'verified')
        self.assertEqual(statuses['project-safe'], 'missing')
        self.assertEqual(statuses['task-tests'], 'verified')
        plan_id = next((item['id'] for item in row['criteria'] if item['source'] == 'plan'))
        self.assertEqual(statuses[plan_id], 'missing')
        self.assertEqual(row['scope']['forbidden'], ['secrets/'])
        self.assertEqual(row['next_actions'][0]['id'], 'execute_ready_task')

    def test_completed_plan_without_active_task_routes_to_retirement_check(self) -> None:
        actions = projection._actions(
            {'present': True},
            {'present': False},
            {
                'present': True,
                'nonempty': True,
                'all_done': True,
                'tasks': [{'id': 'finished', 'state': 'done'}],
            },
            {'complete': True},
            [],
        )
        by_id = {item['id']: item for item in actions}
        self.assertNotIn('record_verified_completion', by_id)
        self.assertEqual(
            by_id['inspect_completed_plan_retirement'],
            {
                'id': 'inspect_completed_plan_retirement',
                'priority': 50,
                'authority': 'read',
                'reason': 'Every declared criterion has current evidence and the completed plan is still active.',
                'command': 'oms agent-plan --repo . retire --check',
            },
        )

    def test_contract_blocker_replaces_ready_execution_with_inspection(self) -> None:
        actions = projection._actions(
            {'present': True},
            {'present': False},
            {
                'present': True,
                'nonempty': True,
                'all_done': False,
                'actionable': [],
                'contract': {
                    'bound': True,
                    'satisfied': False,
                    'project_state': 'confirmed',
                    'blocker': 'project-drift',
                },
                'tasks': [{'id': 'ready-but-blocked', 'state': 'ready'}],
            },
            {'complete': False},
            [],
        )
        by_id = {item['id']: item for item in actions}
        self.assertNotIn('execute_ready_task', by_id)
        self.assertEqual(
            by_id['inspect_plan_contract'],
            {
                'id': 'inspect_plan_contract',
                'priority': 89,
                'authority': 'read',
                'reason': 'The reviewed plan is blocked by PROJECT.md contract state: project-drift.',
                'command': 'oms agent-plan --repo . status --json',
            },
        )

    def test_plan_actions_use_canonical_dependency_and_expiry_verdict(self) -> None:
        base = {
            'present': True,
            'nonempty': True,
            'all_done': False,
            'contract': {'bound': False, 'satisfied': True},
        }
        dependency_blocked = dict(base, actionable=[], tasks=[
            {'id': 'blocked-dependency', 'state': 'blocked'},
            {'id': 'stored-ready', 'state': 'ready'},
        ])
        actions = projection._actions(
            {'present': True}, {'present': False}, dependency_blocked,
            {'complete': False}, [],
        )
        self.assertNotIn('execute_ready_task', [item['id'] for item in actions])

        expired_claim = dict(base, actionable=['expired'], tasks=[
            {'id': 'expired', 'state': 'claimed'},
        ])
        actions = projection._actions(
            {'present': True}, {'present': False}, expired_claim,
            {'complete': False}, [],
        )
        by_id = {item['id']: item for item in actions}
        self.assertIn('execute_ready_task', by_id)
        self.assertNotIn('inspect_active_attempt', by_id)

    def test_explicit_binding_and_dependency_staleness(self) -> None:
        dependency = self.repo / 'scripts' / 'sample.py'
        bound = evidence.bind(self.repo, 'project-safe', 'evt-api', 'verified', dependencies=['scripts/sample.py'])
        self.assertEqual(bound['criterion_id'], 'project-safe')
        statuses = {item['id']: item['status'] for item in evidence.build_coverage(self.repo)['criteria']}
        self.assertEqual(statuses['project-safe'], 'verified')
        unrelated = self.repo / 'unrelated.txt'
        unrelated.write_text('unrelated change\n', encoding='utf-8')
        subprocess.run(['git', '-C', str(self.repo), 'add', 'unrelated.txt'], check=True)
        subprocess.run(['git', '-C', str(self.repo), 'commit', '-qm', 'unrelated'], check=True)
        statuses = {item['id']: item['status'] for item in evidence.build_coverage(self.repo)['criteria']}
        self.assertEqual(statuses['project-safe'], 'verified')
        dependency.write_text(dependency.read_text(encoding='utf-8') + '\n# changed\n', encoding='utf-8')
        statuses = {item['id']: item['status'] for item in evidence.build_coverage(self.repo)['criteria']}
        self.assertEqual(statuses['project-safe'], 'stale')

    def test_context_manifest_selects_target_imports_tests_and_reports_debt(self) -> None:
        manifest = context.plan_context(self.repo, target='scripts/sample.py', required=['scripts/helper.py', 'missing-required.py'], max_bytes=32768)
        selected = {item['path'] for item in manifest['selected']}
        self.assertIn('scripts/sample.py', selected)
        self.assertIn('scripts/helper.py', selected)
        self.assertIn('tests/test_sample.py', selected)
        self.assertFalse(manifest['sufficient'])
        self.assertIn('missing-required.py', manifest['missing_required'])
        self.assertTrue((self.repo / manifest['manifest_path']).is_file())
        self.assertTrue((self.repo / manifest['bundle_path']).is_file())

    def test_path_lists_reject_parent_traversal_and_jsonl_fails_closed_on_truncation(self) -> None:
        self.assertEqual(parse_path_list(['./src', '../outside', 'tests/../secret', 'safe/**']), ['safe/**', 'src'])
        ledger = self.repo / '.oms' / 'runtime' / 'stream.jsonl'
        ledger.parent.mkdir(parents=True, exist_ok=True)
        ledger.write_text(''.join(('{"i":%d}\n' % i for i in range(100))), encoding='utf-8')
        with self.assertRaises(CoreError):
            read_jsonl(ledger, limit_rows=3)
        rows = read_jsonl(ledger, limit_rows=100)
        self.assertEqual([row['i'] for row in rows[-3:]], [97, 98, 99])
        self.assertTrue(sensitive_text('machine file: /mnt/private/checkpoint.bin'))
        self.assertFalse(sensitive_text('public docs: https://example.com/path/to/page'))

    def test_context_discovers_relative_import_and_truncated_required_is_debt(self) -> None:
        package = self.repo / 'pkg'
        package.mkdir()
        (package / '__init__.py').write_text('', encoding='utf-8')
        (package / 'helper.py').write_text('VALUE = 1\n', encoding='utf-8')
        (package / 'target.py').write_text('from .helper import VALUE\n', encoding='utf-8')
        large = self.repo / 'required-large.txt'
        large.write_text('x' * 20000, encoding='utf-8')
        manifest = context.plan_context(self.repo, target='pkg/target.py', required=['required-large.txt'], max_bytes=8192)
        selected = {item['path'] for item in manifest['selected']}
        self.assertIn('pkg/helper.py', selected)
        self.assertIn('required-large.txt', manifest['truncated_required'])
        self.assertFalse(manifest['sufficient'])

    def test_capsule_digest_and_import_do_not_transfer_authority(self) -> None:
        # Keep the transport file outside the checkout: placing it in the
        # worktree legitimately changes the local state fingerprint and should
        # make the imported capsule state-diverged.
        output = Path(self.tmp.name) / 'capsule.json'
        exported = capsule.export(self.repo, output)
        self.assertFalse(exported['authority_transfer'])
        verified = capsule.verify(output)
        self.assertTrue(verified['valid'])
        task_before = sha256_file(self.repo / '.oms' / 'task' / 'current.md')
        imported = capsule.import_capsule(self.repo, output)
        self.assertFalse(imported['authority_transfer'])
        self.assertEqual(task_before, sha256_file(self.repo / '.oms' / 'task' / 'current.md'))
        continuity = evidence.build_envelope(self.repo)['continuity']['latest_import']
        self.assertTrue(continuity['advisory'])
        self.assertFalse(continuity['authority_transfer'])
        self.assertEqual(continuity['status'], 'current')
        raw = read_json(output)
        raw['payload']['contract']['complete'] = True
        atomic_write_json(output, raw)
        with self.assertRaises(CoreError):
            capsule.verify(output)
        self.assertEqual(task_before, sha256_file(self.repo / '.oms' / 'task' / 'current.md'))

    def test_invalid_latest_capsule_import_is_contained_as_advisory_state(self) -> None:
        output = self.repo / 'capsule.json'
        exported = capsule.export(self.repo, output)
        capsule.import_capsule(self.repo, output)
        pointer = self.repo / '.oms' / 'portable' / 'imports' / 'LATEST'
        outside = Path(self.tmp.name) / 'outside-latest'
        outside.write_text(exported['capsule_id'] + '\n', encoding='ascii')
        pointer.unlink()
        try:
            pointer.symlink_to(outside)
        except OSError as exc:
            self.skipTest('symlink unavailable: %s' % exc)
        projected = evidence.build_envelope(self.repo)
        latest = projected['continuity']['latest_import']
        self.assertEqual(latest['status'], 'invalid')
        self.assertFalse(latest['authority_transfer'])
        self.assertIn('project-api', [item['id'] for item in projected['criteria']])

    def test_near_limit_capsule_import_remains_readable(self) -> None:
        row = capsule.build(self.repo)
        target_size = capsule.MAX_CAPSULE_BYTES - 256
        row['payload']['padding'] = 'x' * max(
            0, target_size - len(canonical_json(row)) - 128)
        row['digest'] = sha256_bytes(canonical_json(row['payload']))
        row['capsule_id'] = 'capsule-' + row['digest'][:32]
        encoded = canonical_json(row)
        delta = capsule.MAX_CAPSULE_BYTES - 1 - len(encoded)
        row['payload']['padding'] += 'x' * delta
        row['digest'] = sha256_bytes(canonical_json(row['payload']))
        row['capsule_id'] = 'capsule-' + row['digest'][:32]
        encoded = canonical_json(row)
        self.assertLessEqual(len(encoded), capsule.MAX_CAPSULE_BYTES)
        self.assertGreater(len(encoded), capsule.MAX_CAPSULE_BYTES - 1024)
        output = Path(self.tmp.name) / 'near-limit-capsule.json'
        atomic_write_bytes(output, encoded)
        capsule.import_capsule(self.repo, output)
        stored = (self.repo / '.oms' / 'portable' / 'imports' /
                  (row['capsule_id'] + '.json'))
        self.assertGreater(stored.stat().st_size, capsule.MAX_CAPSULE_BYTES)
        self.assertLessEqual(stored.stat().st_size, capsule.MAX_IMPORT_BYTES)
        latest = evidence.build_envelope(self.repo)['continuity']['latest_import']
        self.assertNotEqual(latest['status'], 'invalid')
        self.assertEqual(latest['capsule_id'], row['capsule_id'])

    def test_producer_covers_and_completion_state_are_evidence_driven(self) -> None:
        # A review-verify row whose covers digest matches the plan acceptance
        # command links automatically; a mismatched digest stays inert.
        import hashlib
        accept_digest = hashlib.sha256('python3 -m unittest'.encode('utf-8')).hexdigest()
        append_jsonl(self.repo / '.oms' / 'artifacts' / 'index.jsonl', {
            'schema': 1, 'event_id': 'evt-gate', 'kind': 'review-verify',
            'exit': 0, 'status': 'verified',
            'covers': ['criterion-plan-acceptance-' + accept_digest[:10]],
        })
        row = evidence.build_envelope(self.repo)
        statuses = {item['id']: item['status'] for item in row['criteria']}
        plan_id = 'criterion-plan-acceptance-' + accept_digest[:10]
        self.assertEqual(statuses[plan_id], 'verified')
        # patch_sha256 is an accepted scope digest spelling: an existing
        # patch-admit row proves scope without being rewritten to a new field.
        supports = {item['id']: [e.get('support') for e in item.get('evidence', [])] for item in row['criteria']}
        self.assertIn('explicit-artifact', supports[plan_id])
        # Completion is derived from evidence, never from model confidence:
        # the fixture task is marked verified while project-safe still lacks
        # evidence, so the completion judgment keeps its unverified qualifier.
        self.assertEqual(row['task']['completion'], 'completed_with_unverified_items')
        from oms_runtime.projection import _completion_state
        complete = {'complete': True, 'counts': {'verified': 3}}
        incomplete = {'complete': False, 'counts': {'verified': 1, 'missing': 2}}
        failing = {'complete': False, 'counts': {'failed': 1}}
        self.assertEqual(_completion_state({'present': True, 'status': 'closed'}, complete), 'completed_verified')
        self.assertEqual(_completion_state({'present': True, 'status': 'closed'}, failing), 'failed')
        self.assertEqual(_completion_state({'present': True, 'status': 'blocked'}, complete), 'blocked')
        self.assertEqual(_completion_state({'present': True, 'status': 'cancelled'}, complete), 'cancelled')
        self.assertEqual(_completion_state({'present': True, 'status': 'active'}, incomplete), 'active')
        self.assertEqual(_completion_state({'present': False, 'status': ''}, incomplete), 'none')

    def test_plan_tasks_are_criteria_and_admission_is_their_evidence(self) -> None:
        # Every plan task projects as a criterion; the admission receipt that
        # carries its task lineage is the automatic evidence — verified on
        # ADMIT (exit 0), failed on REJECT, and a receipt naming no current
        # plan task stays inert instead of guessing.
        plan_path = self.repo / '.oms' / 'plan' / 'tasks.json'
        plan = read_json(plan_path)
        plan['schema'] = 3
        plan['plan_id'] = 'plan_' + '1' * 32
        atomic_write_json(plan_path, plan)
        row = evidence.build_envelope(self.repo)
        statuses = {item['id']: item['status'] for item in row['criteria']}
        self.assertEqual(statuses['plan-task-t1'], 'missing')
        append_jsonl(self.repo / '.oms' / 'artifacts' / 'index.jsonl', {
            'schema': 1, 'event_id': 'evt-admit-t1', 'kind': 'patch-admit',
            'exit': 0, 'task_id': 't1', 'plan_id': plan['plan_id'],
        })
        append_jsonl(self.repo / '.oms' / 'artifacts' / 'index.jsonl', {
            'schema': 1, 'event_id': 'evt-admit-ghost', 'kind': 'patch-admit',
            'exit': 0, 'task_id': 'no-such-task',
        })
        linked = evidence.build_envelope(self.repo)
        by_id = {item['id']: item for item in linked['criteria']}
        self.assertEqual(by_id['plan-task-t1']['status'], 'verified')
        supports = [e.get('support') for e in by_id['plan-task-t1']['evidence']]
        self.assertIn('patch-admission-receipt', supports)
        self.assertNotIn('plan-task-no-such-task', by_id)
        append_jsonl(self.repo / '.oms' / 'artifacts' / 'index.jsonl', {
            'schema': 1, 'event_id': 'evt-admit-t1-reject', 'kind': 'patch-admit',
            'exit': 3, 'task_id': 't1', 'plan_id': plan['plan_id'],
        })
        rejected = evidence.build_envelope(self.repo)
        by_id = {item['id']: item for item in rejected['criteria']}
        self.assertEqual(by_id['plan-task-t1']['status'], 'failed')

    def test_plan_task_evidence_requires_the_exact_plan_lineage(self) -> None:
        """Reusing a task id must not reuse another plan instance's evidence."""
        plan_path = self.repo / '.oms' / 'plan' / 'tasks.json'
        index = self.repo / '.oms' / 'artifacts' / 'index.jsonl'
        bindings = self.repo / '.oms' / 'evidence' / 'bindings.jsonl'
        plan = read_json(plan_path)
        plan['schema'] = 3
        first_id = 'plan_' + 'a' * 32
        second_id = 'plan_' + 'b' * 32

        def write_plan(plan_id: str) -> None:
            current = dict(plan)
            current['plan_id'] = plan_id
            atomic_write_json(plan_path, current)

        def task_status() -> str:
            row = evidence.build_envelope(self.repo)
            return next(item['status'] for item in row['criteria']
                        if item['id'] == 'plan-task-t1')

        mechanisms = (
            ({'schema': 1, 'event_id': 'evt-auto', 'kind': 'patch-admit',
              'exit': 0, 'task_id': 't1', 'plan_id': first_id}, None),
            ({'schema': 1, 'event_id': 'evt-covers', 'kind': 'verify',
              'status': 'verified', 'covers': ['plan-task-t1'],
              'plan_id': first_id}, None),
            ({'schema': 1, 'event_id': 'evt-binding-source', 'kind': 'report'},
             {'schema': 1, 'binding_id': 'binding-first', 'action': 'bind',
              'criterion_id': 'plan-task-t1', 'evidence_ref': 'evt-binding-source',
              'status': 'verified', 'plan_id': first_id}),
        )
        for artifact, binding in mechanisms:
            with self.subTest(kind=artifact['event_id']):
                index.write_text(json.dumps(artifact) + '\n', encoding='utf-8')
                if bindings.exists():
                    bindings.unlink()
                if binding is not None:
                    append_jsonl(bindings, binding)
                write_plan(first_id)
                self.assertEqual(task_status(), 'verified')
                write_plan(second_id)
                self.assertEqual(task_status(), 'missing')

        # Missing and malformed legacy lineage is inert even when task ids and
        # explicit criterion ids match the current plan.
        legacy_rows = [
            {'schema': 1, 'event_id': 'evt-missing', 'kind': 'patch-admit',
             'exit': 0, 'task_id': 't1'},
            {'schema': 1, 'event_id': 'evt-malformed', 'kind': 'verify',
             'status': 'verified', 'covers': ['plan-task-t1'],
             'plan_id': 'plan_not-a-lineage'},
        ]
        index.write_text(''.join(json.dumps(item) + '\n' for item in legacy_rows),
                         encoding='utf-8')
        if bindings.exists():
            bindings.unlink()
        append_jsonl(bindings, {
            'schema': 1, 'binding_id': 'binding-missing-lineage',
            'action': 'bind', 'criterion_id': 'plan-task-t1',
            'evidence_ref': 'evt-missing', 'status': 'verified',
        })
        append_jsonl(bindings, {
            'schema': 1, 'binding_id': 'binding-malformed-lineage',
            'action': 'bind', 'criterion_id': 'plan-task-t1',
            'evidence_ref': 'evt-malformed', 'status': 'verified',
            'plan_id': 'plan_not-a-lineage',
        })
        write_plan(second_id)
        self.assertEqual(task_status(), 'missing')

        # Project criteria remain deliberately reusable and do not require a
        # plan lineage. Only plan-task claims are scoped to a plan instance.
        append_jsonl(index, {
            'schema': 1, 'event_id': 'evt-project-stable', 'kind': 'verify',
            'status': 'verified', 'covers': ['project-safe'],
        })
        projected = evidence.build_envelope(self.repo)
        statuses = {item['id']: item['status'] for item in projected['criteria']}
        self.assertEqual(statuses['project-safe'], 'verified')
        self.assertEqual(statuses['plan-task-t1'], 'missing')

        # The binding producer captures the current plan id. A later plan with
        # the same task and criterion id cannot inherit it.
        index.write_text(json.dumps({
            'schema': 1, 'event_id': 'evt-bind-current', 'kind': 'report'}) + '\n',
            encoding='utf-8')
        write_plan(first_id)
        created = evidence.bind(
            self.repo, 'plan-task-t1', 'evt-bind-current', 'verified')
        self.assertEqual(created['plan_id'], first_id)
        write_plan(second_id)
        self.assertEqual(task_status(), 'missing')

        # A parent binding a legacy plan task establishes the lineage before
        # writing. The receipt and the upgraded plan use the same locked ID.
        if bindings.exists():
            bindings.unlink()
        legacy = dict(plan)
        legacy.pop('plan_id', None)
        legacy['tasks'] = {key: dict(item) for key, item in legacy['tasks'].items()}
        atomic_write_json(plan_path, legacy)
        created = evidence.bind(
            self.repo, 'plan-task-t1', 'evt-bind-current', 'verified')
        upgraded = read_json(plan_path)
        self.assertRegex(created['plan_id'], r'^plan_[0-9a-f]{32}$')
        self.assertEqual(created['plan_id'], upgraded['plan_id'])

    def test_task_source_evidence_requires_exact_active_task_lineage(self) -> None:
        """A successor task cannot inherit evidence through a reused id."""
        task_path = self.repo / '.oms' / 'task' / 'current.md'
        index = self.repo / '.oms' / 'artifacts' / 'index.jsonl'
        bindings = self.repo / '.oms' / 'evidence' / 'bindings.jsonl'
        original_task = task_path.read_text(encoding='utf-8')
        original_index = index.read_text(encoding='utf-8')

        def write_successor() -> None:
            task_path.write_text(
                original_task.replace(
                    '- task_id: task-fixture\n',
                    '- task_id: task-successor\n',
                    1,
                ).replace(
                    '\npython3 -m unittest\n\n## Loop State',
                    '\npython3 -m unittest -q\n\n## Loop State',
                    1,
                ),
                encoding='utf-8',
            )

        mechanisms = ('binding', 'cover', 'legacy-cover')
        for mechanism in mechanisms:
            with self.subTest(mechanism=mechanism):
                task_path.write_text(original_task, encoding='utf-8')
                index.write_text(original_index, encoding='utf-8')
                if bindings.exists():
                    bindings.unlink()

                if mechanism == 'binding':
                    created = evidence.bind(
                        self.repo,
                        'task-tests',
                        'task-verification:task-fixture',
                        'verified',
                    )
                    self.assertEqual(
                        created.get('active_task_id'), 'task-fixture')
                else:
                    row = {
                        'schema': 1,
                        'event_id': 'evt-task-cover',
                        'kind': 'review-verify',
                        'status': 'verified',
                        'covers': ['task-tests'],
                    }
                    if mechanism == 'cover':
                        row['active_task_id'] = 'task-fixture'
                    append_jsonl(index, row)

                write_successor()
                projected = evidence.build_envelope(self.repo)
                by_id = {item['id']: item for item in projected['criteria']}
                self.assertEqual(by_id['task-tests']['status'], 'missing')
                self.assertEqual(by_id['project-api']['status'], 'verified')

        # The successor's own fresh gate remains valid after old task-scoped
        # evidence becomes inert.
        receipt = subprocess.run(
            [
                'bash', '-c',
                '. "$1/scripts/lib/oms-common.sh"; '
                'oms_git_state_fingerprint "$2"; '
                'printf \'%s\\n\' "python3 -m unittest -q" | oms_sha256_stream',
                'fixture', str(ROOT), str(self.repo),
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.split()
        refreshed = []
        for line in task_path.read_text(encoding='utf-8').splitlines():
            if line.startswith('- verified_state:'):
                line = '- verified_state: %s' % receipt[0]
            elif line.startswith('- verified_cmd_sha:'):
                line = '- verified_cmd_sha: %s' % receipt[1]
            refreshed.append(line)
        task_path.write_text('\n'.join(refreshed) + '\n', encoding='utf-8')
        projected = evidence.build_envelope(self.repo)
        successor = next(
            item for item in projected['criteria'] if item['id'] == 'task-tests')
        self.assertEqual(successor['status'], 'verified')
        self.assertEqual(
            [item['support'] for item in successor['evidence']],
            ['fresh-task-gate'],
        )

    def test_projection_subprocess_cost_does_not_scale_with_evidence_rows(self) -> None:
        # The staleness judgment once ran `git rev-parse` per evidence row —
        # ~4.5s of a ~5s build on a thousand-row index. Every row is now
        # judged against the base envelope's single head snapshot, so the
        # subprocess count must be a constant of the build, not of the index.
        from oms_runtime import common
        index = self.repo / '.oms' / 'artifacts' / 'index.jsonl'

        def spawns_after_adding(count: int, tag: str) -> int:
            for i in range(count):
                append_jsonl(index, {
                    'schema': 1, 'event_id': 'evt-scale-%s-%d' % (tag, i),
                    'kind': 'review-verify', 'exit': 0, 'status': 'verified',
                    'verified_head': 'deadbeefdeadbeef',
                })
            calls = []
            original = common.run_output

            def counting(command, **kwargs):
                calls.append(list(command))
                return original(command, **kwargs)

            common.run_output = counting
            try:
                evidence.build_envelope(self.repo)
            finally:
                common.run_output = original
            return len(calls)

        small = spawns_after_adding(5, 'small')
        large = spawns_after_adding(60, 'large')
        self.assertEqual(small, large)

    def test_writer_protocol_parity_with_the_root_durable_writer(self) -> None:
        # Two writer stacks, one protocol: the runtime writer and the root
        # durable-jsonl writer must refuse the same adversarial shapes. This
        # is the conformance contract that keeps parity from silently
        # drifting; the implementations stay separate on purpose (one is a
        # bash-invoked script, one an imported library).
        import importlib.util
        loader_spec = importlib.util.spec_from_file_location(
            'durable_jsonl_root', str(ROOT / 'scripts' / 'lib' / 'durable-jsonl.py'))
        root_writer = importlib.util.module_from_spec(loader_spec)
        loader_spec.loader.exec_module(root_writer)

        arena = Path(self.tmp.name) / 'writer-parity'
        arena.mkdir()
        real_dir = arena / 'real'
        real_dir.mkdir()
        (real_dir / 'seed.jsonl').write_text('{"i":0}\n', encoding='utf-8')

        # Symlinked target: both refuse to write through it.
        (arena / 'link-target.jsonl').symlink_to(real_dir / 'seed.jsonl')
        with self.assertRaises(CoreError):
            atomic_write_bytes(arena / 'link-target.jsonl', b'{}\n')
        with self.assertRaises(SystemExit):
            root_writer.append(str(arena / 'link-target.jsonl'), b'{"i":1}\n')

        # Symlinked parent: both refuse to cross it.
        (arena / 'link-dir').symlink_to(real_dir)
        with self.assertRaises(CoreError):
            atomic_write_bytes(arena / 'link-dir' / 'row.jsonl', b'{}\n')
        with self.assertRaises(SystemExit):
            root_writer.append(str(arena / 'link-dir' / 'row.jsonl'), b'{"i":1}\n')

        # Torn state fails closed on the runtime side (append refuses to build
        # on a partial final row), and both writers refuse a malformed input
        # row — no newline, embedded NUL — before anything touches disk.
        torn = real_dir / 'torn.jsonl'
        torn.write_bytes(b'{"i":0}\n{"i":1}')
        with self.assertRaises(CoreError):
            append_jsonl(torn, {'i': 2})
        clean = real_dir / 'clean.jsonl'
        clean.write_text('{"i":0}\n', encoding='utf-8')
        with self.assertRaises(SystemExit):
            root_writer.append(str(clean), b'{"i":1}')  # no trailing newline
        with self.assertRaises(SystemExit):
            root_writer.append(str(clean), b'{"i":1}\x00\n')

    def test_concurrent_appends_lose_nothing(self) -> None:
        lock_root = Path(self.tmp.name) / 'append-locks'
        target = self.repo / '.oms' / 'append-probe.jsonl'
        writer_script = (
            'import sys\n'
            'sys.path.insert(0, %r)\n'
            'from pathlib import Path\n'
            'from oms_runtime.common import append_jsonl\n'
            'for i in range(10):\n'
            '    append_jsonl(Path(%r), {"w": int(sys.argv[1]), "i": i})\n'
        ) % (str(ROOT / 'scripts' / 'lib'), str(target))
        env = dict(os.environ)
        env['OMS_LOCK_DIR'] = str(lock_root)
        writers = [
            subprocess.Popen([sys.executable, '-c', writer_script, str(index)], env=env)
            for index in range(4)
        ]
        for writer in writers:
            self.assertEqual(writer.wait(timeout=120), 0)
        rows = read_jsonl(target)
        self.assertEqual(len(rows), 40)
        self.assertEqual(len({(row['w'], row['i']) for row in rows}), 40)

    @unittest.skipUnless(os.name == 'posix', 'holder liveness probing is POSIX-only')
    def test_state_lock_never_displaces_a_live_holder_and_reclaims_dead_ones(self) -> None:
        from oms_runtime.common import file_lock, sha256_text
        lock_root = Path(self.tmp.name) / 'locks'
        target = self.repo / '.oms' / 'lock-probe.jsonl'
        env = dict(os.environ)
        env['OMS_LOCK_DIR'] = str(lock_root)
        hold_script = (
            'import sys\n'
            'sys.path.insert(0, %r)\n'
            'from pathlib import Path\n'
            'from oms_runtime.common import file_lock\n'
            'with file_lock(Path(%r), timeout_seconds=30):\n'
            '    print("held", flush=True)\n'
            '    sys.stdin.readline()\n'
            'print("released", flush=True)\n'
        ) % (str(ROOT / 'scripts' / 'lib'), str(target))
        contend_script = (
            'import sys\n'
            'sys.path.insert(0, %r)\n'
            'from pathlib import Path\n'
            'from oms_runtime.common import CoreError, file_lock\n'
            'try:\n'
            '    with file_lock(Path(%r), timeout_seconds=2):\n'
            '        print("acquired", flush=True)\n'
            'except CoreError as exc:\n'
            '    sys.exit(exc.exit_code)\n'
        ) % (str(ROOT / 'scripts' / 'lib'), str(target))

        holder = subprocess.Popen(
            [sys.executable, '-c', hold_script], env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(holder.stdout.readline().strip(), 'held')
            # A live holder is authoritative no matter how impatient the
            # contender: the short timeout must expire, never displace.
            contender = subprocess.run(
                [sys.executable, '-c', contend_script], env=env,
                capture_output=True, text=True, timeout=60)
            self.assertEqual(contender.returncode, 75, contender.stderr)
            self.assertNotIn('acquired', contender.stdout)
        finally:
            holder.stdin.write('\n')
            holder.stdin.close()
            holder.wait(timeout=30)
        after = subprocess.run(
            [sys.executable, '-c', contend_script], env=env,
            capture_output=True, text=True, timeout=60)
        self.assertEqual(after.returncode, 0, after.stderr)

        # A crashed holder leaves a dead PID; the protocol reclaims it instead
        # of waiting forever, and never by age alone.
        lock_dir = lock_root / ('runtime-%s.lock' % sha256_text(str(target.resolve(strict=False)))[:32])
        lock_dir.mkdir(parents=True)
        reaped = subprocess.Popen(['sh', '-c', 'exit 0'])
        reaped.wait()
        (lock_dir / 'owner').write_text('%d.0.0\n' % reaped.pid, encoding='utf-8')
        (lock_dir / 'pid').write_text('%d\n' % reaped.pid, encoding='utf-8')
        (lock_dir / 'started').write_text('1\n', encoding='utf-8')
        os.environ['OMS_LOCK_DIR'] = str(lock_root)
        try:
            with file_lock(target, timeout_seconds=10):
                pass
        finally:
            os.environ.pop('OMS_LOCK_DIR', None)

    def test_managed_shim_location_counts_as_installed(self) -> None:
        # Windows-native Python cannot which() an extensionless bash shim, so
        # a healthy install judged its own provider absent. The managed
        # ~/.local/bin location is part of the product contract, in both HOME
        # spellings (POSIX and the MSYS drive form bash hands to Python).
        fenced_home = Path(self.tmp.name) / 'shim-home'
        (fenced_home / '.local' / 'bin').mkdir(parents=True)
        shim = fenced_home / '.local' / 'bin' / 'codex'
        shim.write_text('#!/usr/bin/env bash\nexit 0\n', encoding='utf-8')
        empty_bin = Path(self.tmp.name) / 'empty-bin'
        empty_bin.mkdir()
        for name in ('bash', 'git', 'python3'):
            tool = empty_bin / name
            tool.write_text('#!/usr/bin/env sh\nexit 0\n', encoding='utf-8')
            tool.chmod(493)
        old_path, old_home = os.environ.get('PATH', ''), os.environ.get('HOME', '')
        os.environ['PATH'] = str(empty_bin)
        os.environ['HOME'] = str(fenced_home)
        try:
            self.assertTrue(profiles.check(['core'])['ready'])
        finally:
            os.environ['PATH'] = old_path
            os.environ['HOME'] = old_home

    def test_profiles_are_optional_and_install_plan_is_minimal(self) -> None:
        fake_bin = Path(self.tmp.name) / 'bin'
        fake_bin.mkdir()
        # The PATH is replaced, not prepended: presence checks go through
        # shutil.which, and a developer machine with a real ntn (or any other
        # optional tool) on PATH must not turn the "notion is not ready"
        # assertion into an environment lottery. Everything core needs is
        # faked explicitly instead.
        for name in ('bash', 'git', 'python3', 'codex', 'gh'):
            path = fake_bin / name
            path.write_text('#!/usr/bin/env sh\nexit 0\n', encoding='utf-8')
            path.chmod(493)
        old_path = os.environ.get('PATH', '')
        old_home = os.environ.get('HOME', '')
        os.environ['PATH'] = str(fake_bin)
        # HOME too: presence checks also honor the managed ~/.local/bin shim
        # directory, and a developer machine keeps real shims there.
        os.environ['HOME'] = str(Path(self.tmp.name) / 'fenced-home')
        try:
            self.assertTrue(profiles.check(['core'])['ready'])
            self.assertFalse(profiles.check(['notion'])['ready'])
            plan = profiles.install_plan(['core', 'github'], 'codex')
            self.assertEqual(plan['managed_tools'], ['node', 'codex', 'gh'])
            council = profiles.install_plan(['core', 'council'], 'codex')
            self.assertEqual(len([name for name in council['managed_tools'] if name in ('codex', 'claude', 'agy')]), 2)
            full = profiles.install_plan(['full'], 'codex')
            self.assertTrue({'codex', 'claude', 'agy'}.issubset(set(full['managed_tools'])))
            applied = profiles.apply(self.repo, ['core', 'github'])
            self.assertTrue(applied['check']['ready'])
        finally:
            os.environ['PATH'] = old_path
            os.environ['HOME'] = old_home

    def test_trusted_local_receipt_is_honest_and_timeout_is_bounded(self) -> None:
        receipt, rc = run_backend('trusted-local', self.repo, [sys.executable, '-c', "print('runtime-ok')"], timeout_seconds=10)
        self.assertEqual(rc, 0)
        self.assertFalse(receipt['enforced_capabilities']['filesystem'])
        self.assertFalse(receipt['enforced_capabilities']['network'])
        self.assertTrue((self.repo / receipt['receipt']).is_file())
        timed, rc = run_backend('trusted-local', self.repo, [sys.executable, '-c', 'import time; time.sleep(5)'], timeout_seconds=1)
        self.assertEqual(rc, 124)
        self.assertTrue(timed['timed_out'])

    def test_experiment_digest_is_stable_and_runs_are_summarized(self) -> None:
        contract = self._experiment()
        registered = experiment.register(self.repo, contract, 'exp1')
        loaded = experiment.load_contract(self.repo, 'exp1')
        self.assertEqual(registered['contract_digest'], loaded['contract_digest'])
        for arm, value, memory in (('baseline', 1.0, 10.0), ('treatment', 1.2, 10.3)):
            metrics = self.repo / ('metrics-%s.json' % arm)
            command = [sys.executable, '-c', "import json; json.dump({'score': %s, 'memory': %s}, open(%r,'w'))" % (value, memory, metrics.name)]
            row, rc = experiment.record_run(self.repo, 'exp1', arm, 0, metrics, command, timeout_seconds=10)
            self.assertEqual(rc, 0)
            self.assertEqual(row['metric_value'], value)
        summary = experiment.summarize(self.repo, 'exp1')
        self.assertTrue(summary['complete'])
        self.assertEqual(summary['verdict'], 'supported')
        self.assertAlmostEqual(summary['improvement'], 0.2)
        self.assertTrue(summary['no_regression']['memory']['passed'])
        # One run, one authority: every trusted-local run landed in the
        # existing run ledger, and the runtime row is a projection linked by
        # the ledger row's own id — the same run never lives under two names.
        ledger_rows = [json.loads(line) for line in
                       (self.repo / 'docs' / 'EXPERIMENTS.jsonl').read_text(encoding='utf-8').splitlines()
                       if line.strip()]
        runtime_rows = read_jsonl(self.repo / '.oms' / 'experiments' / 'runs.jsonl')
        self.assertEqual(len(ledger_rows), len(runtime_rows))
        ledger_by_op = {row['operation_id']: row['id'] for row in ledger_rows}
        for row in runtime_rows:
            self.assertEqual(row['ledger_id'], ledger_by_op[row['run_id']])

    def test_release_failure_taxonomy_and_benchmark(self) -> None:
        stable = release.resolve(self.repo, 'stable')
        self.assertTrue(stable['ready'])
        self.assertEqual(failures.classify('verification failed')['code'], 'verifier_failed')
        row = benchmark_snapshot(self.repo)
        self.assertIn('useful_work_efficiency', row)
        self.assertIn('human_corrections', row['unknown_metrics'])

    def test_benchmark_groups_outcomes_by_served_model(self) -> None:
        # The per-model table is what a routing decision can be argued from:
        # the served model when the transport reported one, the selected model
        # when it was pinned, and nothing for a provider-default route the
        # transport left anonymous.
        index = self.repo / '.oms' / 'artifacts' / 'index.jsonl'
        rows = [
            {'schema': 1, 'event_id': 'evt-m1', 'kind': 'call', 'provider': 'codex', 'served_model': 'gpt-5.6-terra', 'status': 'success', 'tokens': 100, 'duration_s': 2.0, 'cost_usd': 0.01},
            {'schema': 1, 'event_id': 'evt-m2', 'kind': 'call', 'provider': 'codex', 'served_model': 'gpt-5.6-terra', 'status': 'failed', 'tokens': 50, 'duration_s': 4.0},
            {'schema': 1, 'event_id': 'evt-m3', 'kind': 'call', 'provider': 'claude', 'selected_model': 'claude-opus-5', 'status': 'success', 'tokens': 10},
            {'schema': 1, 'event_id': 'evt-m4', 'kind': 'call', 'provider': 'antigravity', 'selected_model': 'provider-default', 'status': 'success'},
        ]
        with index.open('a', encoding='utf-8') as handle:
            for item in rows:
                handle.write(json.dumps(item) + '\n')
        models = benchmark_snapshot(self.repo)['models']
        terra = models['codex/gpt-5.6-terra']
        self.assertEqual(terra['calls'], 2)
        self.assertEqual(terra['verified'], 1)
        self.assertEqual(terra['failed'], 1)
        self.assertEqual(terra['tokens'], 150)
        self.assertEqual(terra['duration_seconds_mean'], 3.0)
        self.assertAlmostEqual(terra['cost_usd'], 0.01)
        self.assertEqual(models['claude/claude-opus-5']['calls'], 1)
        self.assertNotIn('antigravity/provider-default', models)
        self.assertFalse(any(key.startswith('antigravity/') for key in models))

    def test_seat_no_answer_is_not_classified_by_exit_code(self) -> None:
        # A seat that produced nothing is a distinct, recoverable failure, but
        # exit 3 is not what makes it one: goal-drive parks and failed
        # acceptances exit 3 as well. Classifying on the exit code would file
        # all three under a single recovery, which is why the rule reads the
        # phrase the seat recorder actually writes.
        seat = failures.classify(
            'claude call seat returned no answer (exit 3)', exit_code=3)
        self.assertEqual(seat['code'], 'provider_no_answer')
        self.assertEqual(seat['recovery'], 'retry_or_drop_seat')
        self.assertTrue(seat['retryable'])
        for other in ('goal-drive parked run=gd cycle=0 reason=intent-ref-moved',
                      'plan-accept: error acceptance-files-changed'):
            self.assertNotEqual(
                failures.classify(other, exit_code=3)['code'],
                'provider_no_answer', other)
        # An explicit timeout still outranks the phrase: the seat recorder
        # writes the same words with exit 124, and that is a timeout.
        self.assertEqual(
            failures.classify(
                'claude call seat returned no answer (exit 124)',
                exit_code=124)['code'],
            'provider_timeout')
        self.assertIn('provider_no_answer', failures.codes())
        # This harness refusing to send is not a seat that failed to answer:
        # the seat was never called, so retrying or dropping it fixes nothing.
        refused = failures.classify(
            'outbound context refused before send (sensitive-looking content);'
            ' no seat was called', exit_code=3)
        self.assertEqual(refused['code'], 'outbound_context_refused')
        self.assertEqual(refused['recovery'], 'remove_sensitive_context')
        self.assertFalse(refused['retryable'])
        self.assertFalse(refused['fallback_allowed'])

    def test_atomic_writer_rejects_symlink_and_preserves_parent_mode(self) -> None:
        parent = self.repo / 'tracked-config'
        parent.mkdir()
        parent.chmod(493)
        outside = Path(self.tmp.name) / 'outside.json'
        outside.write_text('unchanged\n', encoding='utf-8')
        target = parent / 'state.json'
        target.symlink_to(outside)
        before_mode = stat.S_IMODE(parent.stat().st_mode)
        with self.assertRaises(CoreError):
            atomic_write_json(target, {'unsafe': True})
        self.assertEqual(outside.read_text(encoding='utf-8'), 'unchanged\n')
        self.assertEqual(stat.S_IMODE(parent.stat().st_mode), before_mode)

    def test_capsule_rejects_secret_and_machine_path_even_with_valid_digest(self) -> None:
        row = capsule.build(self.repo)
        row['payload']['continuity']['note'] = bytes.fromhex('6170695f6b65793d73757065722d7365637265742d6d6174657269616c').decode('ascii')
        row['digest'] = sha256_bytes(canonical_json(row['payload']))
        row['capsule_id'] = 'capsule-' + row['digest'][:32]
        path = self.repo / ('bad-' + 'se' + 'cret-capsule.json')
        atomic_write_json(path, row)
        with self.assertRaises(CoreError):
            capsule.verify(path)
        row = capsule.build(self.repo)
        row['payload']['continuity']['note'] = '/mnt/private/project'
        row['digest'] = sha256_bytes(canonical_json(row['payload']))
        row['capsule_id'] = 'capsule-' + row['digest'][:32]
        atomic_write_json(path, row)
        with self.assertRaises(CoreError):
            capsule.verify(path)

    def test_context_omits_sensitive_source_and_unknown_evidence_ref_is_rejected(self) -> None:
        blocked_note = self.repo / ('se' + 'cret-note.txt')
        blocked_note.write_bytes(bytes.fromhex('6163636573735f746f6b656e3d6768705f6162636465666768696a6b6c6d6e6f70717273740a'))
        manifest = context.plan_context(self.repo, explicit=[(('se' + 'cret-note.txt'), 'should be scrubbed')], max_bytes=32768)
        omitted = {item['path']: item['reason'] for item in manifest['omitted']}
        self.assertEqual(omitted['se' + 'cret-note.txt'], 'sensitive-looking content')
        with self.assertRaises(CoreError):
            evidence.bind(self.repo, 'project-safe', 'evt-does-not-exist', 'verified')

    def test_council_profile_requires_two_providers(self) -> None:
        fake_bin = Path(self.tmp.name) / 'council-bin'
        fake_bin.mkdir()
        old_path = os.environ.get('PATH', '')
        old_home = os.environ.get('HOME', '')
        os.environ['PATH'] = str(fake_bin)
        os.environ['HOME'] = str(Path(self.tmp.name) / 'council-home')
        try:
            for required in ('bash', 'git', 'python3'):
                command = fake_bin / required
                command.write_text('#!/bin/sh\nexit 0\n', encoding='utf-8')
                command.chmod(493)
            # Optional transports satisfy the same runtime capability contract;
            # the installer still keeps the historical core as its managed
            # default, but an already-installed Cursor/Grok pair is usable.
            one = fake_bin / 'cursor-agent'
            one.write_text('#!/bin/sh\nexit 0\n', encoding='utf-8')
            one.chmod(493)
            self.assertFalse(profiles.check(['council'])['ready'])
            two = fake_bin / 'grok'
            two.write_text('#!/bin/sh\nexit 0\n', encoding='utf-8')
            two.chmod(493)
            self.assertTrue(profiles.check(['council'])['ready'])
            one.unlink()
            two.unlink()
            deepseek = fake_bin / 'dsh'
            deepseek.write_text('#!/bin/sh\nexit 0\n', encoding='utf-8')
            deepseek.chmod(493)
            vibe = fake_bin / 'vibe'
            vibe.write_text('#!/bin/sh\nexit 0\n', encoding='utf-8')
            vibe.chmod(493)
            result = profiles.check(['council'])
            self.assertTrue(result['ready'], result)
            self.assertEqual(result['minimum'][0]['present'], ['dsh', 'vibe'])
        finally:
            os.environ['PATH'] = old_path
            os.environ['HOME'] = old_home

    def test_backend_readiness_and_absolute_local_command(self) -> None:
        self.assertFalse(check_backend('isolated', image='missing-image')['ready'])
        self.assertFalse(check_backend('remote', adapter='missing-adapter')['ready'])
        script = Path(self.tmp.name) / 'absolute-script.py'
        script.write_text("print('absolute-ok')\n", encoding='utf-8')
        receipt, rc = run_backend('trusted-local', self.repo, [sys.executable, str(script)], timeout_seconds=10)
        self.assertEqual(rc, 0)
        self.assertFalse(receipt['resource_limits']['enforced'])

    def test_remote_adapter_protocol_records_attestation_without_claiming_enforcement(self) -> None:
        adapter = Path(self.tmp.name) / 'adapter.py'
        adapter.write_text("#!/usr/bin/env python3\nimport json,sys\nrequest=json.load(sys.stdin)\nassert request['schema']==1 and request['command']==['echo','remote']\nprint(json.dumps({'schema': 1, 'operation_id': request['operation_id'], 'accepted': True, 'exit': 0, 'attestation': {'transport_authenticated': True, 'filesystem_isolated': True, 'cleanup_confirmed': True}}))\n", encoding='utf-8')
        adapter.chmod(493)
        receipt, rc = run_backend('remote', self.repo, ['echo', 'remote'], adapter=str(adapter), timeout_seconds=10)
        self.assertEqual(rc, 0)
        self.assertTrue(receipt['adapter_attestation']['transport_authenticated'])
        self.assertEqual(receipt['unknown_capabilities'][0], 'remote authentication')
        self.assertTrue(receipt['resource_limits']['attested_only'])
        self.assertTrue(receipt['adapter_attestation_valid'])

    def test_release_promotion_requires_manifest_cas(self) -> None:
        before = release.resolve(self.repo, 'stable')['manifest_digest']
        with self.assertRaises(CoreError) as raised:
            release.promote(self.repo, git_head(self.repo), '1.0.1', expected_manifest_digest='0' * 64)
        self.assertEqual(raised.exception.exit_code, 75)
        promoted = release.promote(self.repo, git_head(self.repo), '1.0.1', expected_manifest_digest=before)
        self.assertTrue(promoted['promoted'])
        self.assertNotEqual(promoted['new_manifest_digest'], before)

    def test_experiment_refuses_stale_metrics_and_enforces_no_regression(self) -> None:
        experiment.register(self.repo, self._experiment(), 'exp-stale')
        metrics = self.repo / 'stale-metrics.json'
        metrics.write_text(json.dumps({'score': 9.0, 'memory': 1.0}), encoding='utf-8')
        with self.assertRaises(CoreError):
            experiment.record_run(self.repo, 'exp-stale', 'baseline', 0, metrics, [sys.executable, '-c', "print('does not write metrics')"], timeout_seconds=10)
        evaluated = experiment.evaluate(self._experiment(), {'baseline': {'score': [1.0], 'memory': [10.0]}, 'treatment': {'score': [1.2], 'memory': [11.0]}})
        self.assertEqual(evaluated['verdict'], 'not_supported')
        self.assertFalse(evaluated['no_regression']['memory']['passed'])
