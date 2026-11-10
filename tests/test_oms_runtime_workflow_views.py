"""New read projections share the existing runtime-core unittest discovery lane."""
from copy import deepcopy
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/lib'))
from oms_runtime.workflow_views import ViewError, evidence_matrix, explain_next


def fixture():
    plan_id = 'plan_' + '1' * 32
    def task(tid, state, depends=()):
        return {'id': tid, 'title': 'Task ' + tid, 'state': state, 'depends': list(depends), 'verify_digest': 'a' * 64}
    def criterion(cid, status, **kwargs):
        return {'id': cid, 'text': 'Criterion ' + cid, 'source': 'project', 'status': status, 'evidence': [], **kwargs}
    return {'schema': 2, 'generated_at': '2026-11-10T00:00:00Z', 'state_digest': 'b' * 64,
            'repo': {'head': 'c' * 40, 'branch': 'main'}, 'sources': [],
            'task': {'present': False}, 'evidence': {'complete': False},
            'criteria': [criterion('public-api', 'missing'),
                         criterion('plan-task-build', 'verified', source='plan-task', plan_task_id='build', plan_id=plan_id),
                         criterion('plan-task-test', 'failed', source='plan-task', plan_task_id='test', plan_id=plan_id)],
            'plan': {'present': True, 'plan_id': plan_id, 'contract': {'bound': True, 'satisfied': True},
                     'actionable': ['test'], 'tasks': [task('build', 'done'), task('test', 'ready', ('build',))]},
            'failures': [], 'warnings': [], 'next_actions': [
                {'id': 'execute_ready_task', 'priority': 80, 'authority': 'worktree_write',
                 'reason': '1 plan task(s) are ready.', 'command': 'oms plan-run --next'}]}


class EvidenceMatrixTests(unittest.TestCase):
    def test_explicit_lineage_and_unlinked_requirements(self):
        result = evidence_matrix(fixture())
        items = {r['id']: r for r in result['criteria']['items']}
        self.assertIsNone(items['public-api']['task'])
        self.assertEqual(items['public-api']['task_link'], 'not-recorded')
        self.assertEqual(items['plan-task-build']['task']['id'], 'build')
        self.assertEqual(result['unlinked_criteria'], 1)
        self.assertEqual(result['criteria']['items'][0]['reported_status'], 'failed')
        self.assertNotIn('complete', result)

    def test_foreign_plan_and_missing_task_do_not_link(self):
        for field, value in [('plan_id', 'plan_' + '2'*32), ('plan_task_id', 'missing')]:
            data = fixture(); data['criteria'][1][field] = value
            with self.subTest(field=field):
                result = evidence_matrix(data)
                row = next(r for r in result['criteria']['items'] if r['id'] == 'plan-task-build')
                self.assertIsNone(row['task'])
                self.assertEqual(row['task_link'], 'invalid-plan-lineage')

    def test_active_task_requires_explicit_matching_id(self):
        data = fixture(); data['task'] = {'present': True, 'task_id': 'active', 'status': 'active'}
        data['criteria'][0].update(source='task', active_task_id='active')
        self.assertEqual(next(r for r in evidence_matrix(data)['criteria']['items'] if r['id']=='public-api')['task']['id'], 'active')
        data['criteria'][0]['active_task_id'] = 'old'
        self.assertEqual(next(r for r in evidence_matrix(data)['criteria']['items'] if r['id']=='public-api')['task_link'], 'invalid-active-task-lineage')

    def test_nested_limits_and_historical_evidence(self):
        data = fixture()
        data['criteria'][2]['evidence'] = [{'evidence_ref': 'proof:'+str(i), 'kind': 'review',
            'status': 'stale', 'signal': 'advisory', 'source': 'index.jsonl', 'ts': '', 'scope_digest': ''} for i in range(3)]
        result = evidence_matrix(data, limit=1)
        self.assertEqual(result['criteria']['omitted'], 2)
        self.assertEqual(result['criteria']['items'][0]['evidence']['omitted'], 2)
        self.assertEqual(result['criteria']['items'][0]['reported_status'], 'failed')

    def test_empty_is_not_completed(self):
        data = fixture(); data['criteria'] = []
        result = evidence_matrix(data)
        self.assertEqual(result['criteria']['total'], 0)
        self.assertNotIn('complete', result)

    def test_duplicate_id_unknown_status_and_bad_schema_fail(self):
        variants=[]
        data=fixture(); data['criteria'].append(deepcopy(data['criteria'][0])); variants.append(data)
        data=fixture(); data['plan']['tasks'].append(deepcopy(data['plan']['tasks'][0])); variants.append(data)
        data=fixture(); data['criteria'][0]['status']='passed'; variants.append(data)
        data=fixture(); data['schema']=True; variants.append(data)
        data=fixture(); data['state_digest']='bad'; variants.append(data)
        for data in variants:
            with self.subTest(data=data), self.assertRaises(ViewError): evidence_matrix(data)

    def test_control_characters_never_escape_output(self):
        data=fixture(); data['criteria'][0]['text']='검사\x1b[31m\u202e\n다음'
        row=next(r for r in evidence_matrix(data)['criteria']['items'] if r['id']=='public-api')
        for ch in ('\x1b','\u202e','\n'): self.assertNotIn(ch,row['text'])
        self.assertIn('검사', row['text'])

    def test_inputs_unchanged_and_projection_has_no_io(self):
        data=fixture(); before=deepcopy(data)
        with patch('builtins.open', side_effect=AssertionError('file I/O')):
            evidence_matrix(data); explain_next(data)
        self.assertEqual(before,data)

    def test_limits_reject_booleans_and_large_values(self):
        for n in (0,101,True,1.5,'2'):
            with self.subTest(n=n), self.assertRaises(ViewError): evidence_matrix(fixture(),limit=n)


class NextExplanationTests(unittest.TestCase):
    def test_reuses_actionable_and_next_actions(self):
        data=fixture(); result=explain_next(data,task_id='test')
        row=result['tasks']['items'][0]
        self.assertTrue(row['reported_actionable'])
        self.assertEqual(row['blockers']['total'],0)
        self.assertEqual(result['canonical_next_actions']['items'][0]['command'],'oms plan-run --next')
        self.assertEqual(data,fixture())

    def test_cancelled_and_missing_dependencies_do_not_satisfy(self):
        data=fixture(); data['plan']['actionable']=[]
        data['plan']['tasks'][0]['state']='cancelled'
        data['plan']['tasks'][1]['depends'].append('gone')
        blockers=explain_next(data,task_id='test')['tasks']['items'][0]['blockers']['items']
        self.assertEqual({x['state'] for x in blockers},{'cancelled','missing'})

    def test_contract_and_unknown_blockers_preserved(self):
        data=fixture(); data['plan']['actionable']=[]
        data['plan']['contract'].update(satisfied=False,blocker='spec-changed')
        blockers=explain_next(data,task_id='test')['tasks']['items'][0]['blockers']['items']
        self.assertEqual(blockers[0]['reason'],'spec-changed')
        data['plan']['contract']['satisfied']=True; data['plan']['tasks'][1]['state']='blocked'
        self.assertEqual(explain_next(data,task_id='test')['tasks']['items'][0]['blockers']['items'][0]['code'],'reason-not-recorded')

    def test_cycle_only_marks_members(self):
        data=fixture(); data['plan']['actionable']=[]
        data['plan']['tasks'][0].update(state='ready',depends=['test'])
        data['plan']['tasks'][1]['depends']=['build']
        data['plan']['tasks'].append({'id':'downstream','title':'','state':'ready','depends':['test']})
        output={r['id']:r for r in explain_next(data)['tasks']['items']}
        for k in ('build','test'):
            self.assertIn('dependency-cycle',{b['code'] for b in output[k]['blockers']['items']})
        self.assertNotIn('dependency-cycle',{b['code'] for b in output['downstream']['blockers']['items']})

    def test_long_chain_does_not_recurse(self):
        data=fixture(); data['criteria']=[]; data['plan']['actionable']=[]
        data['plan']['tasks']=[{'id':f't{i}','state':'ready','title':'','depends':[f't{i+1}'] if i<1999 else []} for i in range(2000)]
        self.assertEqual(explain_next(data,limit=1)['tasks']['omitted'],1999)

    def test_parent_is_not_automatically_person(self):
        data=fixture(); data['plan']['actionable']=[]; data['plan']['tasks'][1]['state']='review'
        row=explain_next(data,task_id='test')['tasks']['items'][0]
        self.assertEqual(row['blockers']['items'][0]['actor'],'parent')
        self.assertNotIn('human',json.dumps(row))

    def test_unknown_task_bad_actionable_duplicate_edges_fail(self):
        with self.assertRaises(ViewError): explain_next(fixture(),task_id='other')
        data=fixture(); data['plan']['actionable']=['missing']
        with self.assertRaises(ViewError): explain_next(data)
        data=fixture(); data['plan']['tasks'][1]['depends']=['build','build']
        with self.assertRaises(ViewError): explain_next(data)




class WorkflowMalformedInputTests(unittest.TestCase):
    def test_unhashable_task_link_is_a_safe_error(self):
        data=fixture();data['criteria'][1]['plan_task_id']=['build']
        with self.assertRaises(ViewError):explain_next(data)

    def test_invalid_dependency_after_missing_one_is_validated(self):
        data=fixture();data['plan']['tasks'][1]['depends']=['gone',{}]
        with self.assertRaises(ViewError):explain_next(data)

if __name__=='__main__': unittest.main(verbosity=2)
