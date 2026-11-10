"""Canonical shell/CLI integration. Requires a full checkout, not module stubs."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class InspectionCLIIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='oms-inspection-cli-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / 'repo'; self.repo.mkdir()
        self.home = self.root / 'home'; self.home.mkdir()
        self.env = dict(os.environ, HOME=str(self.home), XDG_CACHE_HOME=str(self.home/'cache'),
                        XDG_STATE_HOME=str(self.home/'state'), PYTHONDONTWRITEBYTECODE='1')
        self.env.pop('OMS_HARNESS_CHILD', None)
        self.entry = ROOT / 'scripts/runtime.sh'
        self.assertTrue(self.entry.is_file(), 'full OMS checkout required for canonical CLI integration')
        subprocess.run(['git','init','-q',str(self.repo)], check=True, env=self.env)
        self.project = self.repo/'PROJECT.md'
        self.project.write_text('# Fixture\n\n## Goal\n\nInspect without mutation.\n\n## Acceptance Criteria\n\n- [id:fixture-criterion] Reports preserve evidence meaning.\n')
        subprocess.run(['git','-C',str(self.repo),'add','PROJECT.md'],check=True,env=self.env)
        subprocess.run(['git','-C',str(self.repo),'-c','user.name=Fixture','-c','user.email=fixture@example.invalid',
                        'commit','-qm','fixture'],check=True,env=self.env)

    def call(self,*args,child=False):
        return subprocess.run(['bash',str(self.entry),'--repo',str(self.repo),*args],
                              env=dict(self.env,**({'OMS_HARNESS_CHILD':'1'} if child else {})),
                              stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,timeout=45)

    def ok(self,*args,child=False):
        result=self.call(*args,child=child)
        self.assertEqual(result.returncode,0,result.stderr)
        return json.loads(result.stdout)

    def snapshot(self):
        return {str(p.relative_to(self.repo)):p.read_bytes() for p in self.repo.rglob('*')
                if p.is_file() and not p.is_symlink() and '.git' not in p.relative_to(self.repo).parts}

    def test_default_contracts_matrix_and_explanation(self):
        initial=self.snapshot()
        self.assertEqual(self.ok('envelope','show')['schema'],2)
        self.assertEqual(set(self.ok('next')),{'schema','state_digest','actions'})
        self.assertEqual(self.ok('evidence','show','--matrix',child=True)['kind'],'oms-evidence-matrix')
        self.assertEqual(self.ok('next','--explain',child=True)['kind'],'oms-next-explanation')
        self.assertEqual(self.call('next','--task','unselected').returncode,2)
        self.assertEqual(self.call('evidence','show','--matrix','--limit','0').returncode,2)
        self.assertEqual(initial,self.snapshot())

    def test_return_brief_live_and_saved(self):
        self.ok('envelope','write','--output','before.json')
        self.project.write_text(self.project.read_text().replace('Reports preserve evidence meaning.','Changed acceptance definition.'))
        self.ok('envelope','write','--output','after.json')
        initial=self.snapshot()
        recorded=self.ok('return-brief','before.json','--against','after.json',child=True)
        self.assertEqual(recorded['kind'],'oms-return-brief')
        self.assertGreater(recorded['criteria']['changed']['total'],0)
        self.assertEqual(self.ok('return-brief','before.json')['kind'],'oms-return-brief')
        self.assertFalse(recorded['repository_identity_verified'])
        self.assertEqual(initial,self.snapshot())

    def test_digest_and_diff_anchor_are_read_only(self):
        (self.repo/'run.log').write_bytes(b'ERROR: synthetic fixture\n')
        diff=b'diff --git a/file.py b/file.py\n--- a/file.py\n+++ b/file.py\n@@ -1 +1 @@\n-old\n+new\n'
        (self.repo/'review.diff').write_bytes(diff)
        (self.repo/'findings.json').write_text(json.dumps({'schema':1,'findings':[{'path':'file.py','line':1,'quote':'new'}]}))
        initial=self.snapshot()
        self.assertEqual(self.ok('output-digest','run.log','--exit-code','1',child=True)['reported_exit_code'],1)
        result=self.ok('evidence','anchors','findings.json','--diff','review.diff',
                       '--expected-diff-sha256',hashlib.sha256(diff).hexdigest(),child=True)
        self.assertEqual(result['findings']['items'][0]['anchor_status'],'in-diff')
        self.assertEqual(initial,self.snapshot())

    def test_child_mutations_remain_refused(self):
        initial=self.snapshot()
        result=self.call('evidence','bind','--criterion','fixture-criterion','--ref','arbitrary','--status','verified',child=True)
        self.assertEqual(result.returncode,2)
        self.assertIn('a harness child may only use read-only runtime actions',result.stderr)
        self.assertEqual(self.call('envelope','write',child=True).returncode,2)
        self.assertEqual(initial,self.snapshot())


if __name__=='__main__':unittest.main(verbosity=2)
