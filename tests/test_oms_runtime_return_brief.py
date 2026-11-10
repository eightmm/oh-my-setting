from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'scripts/lib'))
from oms_runtime.inspection_io import ReadError, read_object, read_segments
from oms_runtime.return_brief import compare_envelopes
from test_oms_runtime_workflow_views import fixture


class ReturnBriefTests(unittest.TestCase):
    def pair(self):
        before=fixture(); after=deepcopy(before)
        after.update(generated_at='2026-11-10T01:00:00Z',state_digest='d'*64)
        return before,after

    def test_reports_changes_without_completion_inference(self):
        before,after=self.pair(); after['plan']['tasks'][1]['state']='done'
        after['criteria'][2]['status']='stale'
        result=compare_envelopes(before,after)
        self.assertEqual(result['tasks']['changed']['items'][0]['changes']['state']['after'],'done')
        self.assertEqual(result['criteria']['changed']['items'][0]['changes']['status']['after'],'stale')
        self.assertFalse(result['history_complete'])
        self.assertNotIn('accepted',result)

    def test_plan_rollover_does_not_alias_tasks(self):
        before,after=self.pair(); after['plan']['plan_id']='plan_'+'2'*32
        result=compare_envelopes(before,after)
        self.assertEqual(result['scope'],'different-recorded-plan')
        self.assertNotIn('changed',result['tasks'])
        self.assertEqual(result['tasks']['before_ids']['items'],['build','test'])

    def test_head_equality_is_not_repo_identity(self):
        before,after=self.pair()
        for data in (before,after): data['plan']['plan_id']=''
        result=compare_envelopes(before,after)
        self.assertEqual(result['scope'],'unproven')
        self.assertFalse(result['repository_identity_verified'])

    def test_criterion_definition_change_is_visible(self):
        before,after=self.pair(); after['criteria'][0]['text']='A different requirement'
        self.assertIn('text',compare_envelopes(before,after)['criteria']['changed']['items'][0]['changes'])

    def test_missing_failure_is_not_reported_resolved(self):
        before,after=self.pair(); before['failures']=[{'id':'fail-1','source':'.oms/failures.jsonl'}]
        result=compare_envelopes(before,after)
        self.assertEqual(result['failures']['no_longer_present']['total'],1)
        self.assertNotIn('resolved',result['failures'])

    def test_failure_dedupe_and_missing_identity(self):
        before,after=self.pair(); after['failures']=[{'id':'f','source':'file'}]*2+[{'id':''}]
        result=compare_envelopes(before,after)
        self.assertEqual(result['failures']['appeared']['total'],1)
        self.assertEqual(result['failures']['after_unkeyed'],1)

    def test_reversed_equal_and_unknown_time(self):
        before,after=self.pair()
        for stamp,want in [('2026-11-09T00:00:00Z','reversed'),(before['generated_at'],'equal'),('2026-11-10T01:00:00','unknown'),('bad','unknown')]:
            after['generated_at']=stamp
            with self.subTest(stamp=stamp):
                result=compare_envelopes(before,after)
                self.assertEqual(result['chronology'],want)
                self.assertTrue(result['warnings'])

    def test_missing_rows_are_bounded_and_inputs_preserved(self):
        before,after=self.pair(); after['criteria']=[]
        saved=deepcopy((before,after)); result=compare_envelopes(before,after,limit=1)
        self.assertEqual(result['criteria']['no_longer_present']['omitted'],2)
        self.assertEqual(saved,(before,after))

    def test_changed_contract_does_not_promote_old_evidence(self):
        before,after=self.pair(); after['plan']['contract'].update(satisfied=False,blocker='spec-changed')
        result=compare_envelopes(before,after)
        self.assertIn('satisfied',result['contract_fields_changed'])
        self.assertNotIn('verified',result)


class InspectionIOTests(unittest.TestCase):
    def setUp(self):
        temp=tempfile.TemporaryDirectory(prefix='oms-view-'); self.addCleanup(temp.cleanup)
        self.root=Path(temp.name); self.repo=self.root/'repo'; self.repo.mkdir()
        self.path=self.repo/'before.json'; self.path.write_text(json.dumps(fixture()))

    def test_reads_only_selected_file_and_does_not_write(self):
        original=self.path.read_bytes(); value=read_object(self.repo,'before.json')
        self.assertEqual(value,fixture()); self.assertEqual(original,self.path.read_bytes())
        self.assertEqual([p.name for p in self.repo.iterdir()],['before.json'])

    def test_symlink_parent_hardlink_fifo_traversal_rejected(self):
        (self.repo/'link').symlink_to(self.path)
        (self.repo/'parent').symlink_to(self.root,target_is_directory=True)
        os.mkfifo(self.repo/'fifo'); os.link(self.path,self.repo/'hard')
        for path in ('link','parent/repo/before.json','fifo','hard','../outside','.',self.root/'outside'):
            with self.subTest(path=path), self.assertRaises(ReadError): read_object(self.repo,path)

    def test_duplicate_keys_nonfinite_invalid_and_deep_json_rejected(self):
        for raw in (b'{"a":1,"a":2}',b'{"a":NaN}',b'{"a":Infinity}',b'\xff',b'[]',b'{' ,b'{"x":'+b'['*100+b'0'+b']'*100+b'}'):
            self.path.write_bytes(raw)
            with self.subTest(raw=raw[:30]), self.assertRaises(ReadError): read_object(self.repo,'before.json')

    def test_full_and_partial_hash_scope(self):
        self.path.write_bytes(b'0123456789')
        result=read_segments(self.repo,'before.json')
        self.assertEqual(result['sha256'],hashlib.sha256(b'0123456789').hexdigest())
        result=read_segments(self.repo,'before.json',max_bytes=6,allow_partial=True)
        self.assertIsNone(result['sha256']); self.assertFalse(result['complete'])
        self.assertEqual(result['segments'],[(0,b'012'),(7,b'789')])
        with self.assertRaises(ReadError):read_segments(self.repo,'before.json',max_bytes=6)

    def test_mid_read_change_and_path_replacement_rejected(self):
        original=os.read
        for replace in (False,True):
            self.path.write_bytes(b'1234567890'); changed=[]
            def racing(fd,count):
                value=original(fd,count)
                if not changed:
                    changed.append(True)
                    if replace:
                        other=self.repo/'replacement'; other.write_bytes(b'abcdefghij'); other.replace(self.path)
                    else:
                        with self.path.open('ab') as f:f.write(b'changed')
                return value
            with self.subTest(replace=replace), patch('os.read',racing), self.assertRaises(ReadError):
                read_segments(self.repo,'before.json')

    def test_partial_read_loop_empty_file_and_unicode_filename(self):
        self.path.write_bytes(b'abc'); original=os.read
        with patch('os.read',lambda fd,n:original(fd,min(n,1))):
            self.assertEqual(read_segments(self.repo,'before.json')['segments'][0][1],b'abc')
        (self.repo/'빈파일').write_bytes(b'')
        self.assertEqual(read_segments(self.repo,'빈파일')['sha256'],hashlib.sha256(b'').hexdigest())

    def test_bounds_and_errors_do_not_echo_content(self):
        for n in (0,True,16*1024*1024+1):
            with self.assertRaises(ReadError):read_segments(self.repo,'before.json',max_bytes=n)
        self.path.write_text('{"private-fixture":')
        with self.assertRaises(ReadError) as raised:read_object(self.repo,'before.json')
        self.assertNotIn('private-fixture',str(raised.exception))
        self.assertNotIn(str(self.root),str(raised.exception))




class JSONFiniteNumberTests(unittest.TestCase):
    def test_exponent_overflow_is_not_a_finite_number(self):
        with tempfile.TemporaryDirectory(prefix='oms-finite-') as folder:
            repo=Path(folder);(repo/'input.json').write_text('{"value":1e999}')
            with self.assertRaises(ReadError):read_object(repo,'input.json')

if __name__=='__main__': unittest.main(verbosity=2)
