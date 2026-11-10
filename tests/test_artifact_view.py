#!/usr/bin/env python3
"""Offline view/source binding and feedback import, a new public command family."""
from __future__ import annotations

import base64
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts/lib"))
spec = importlib.util.spec_from_file_location("artifact_view", ROOT / "scripts/lib/artifact_view.py")
view = importlib.util.module_from_spec(spec)
spec.loader.exec_module(view)


class ArtifactViewTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="oms-view-test-")
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name) / "repo"
        self.repo.mkdir()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("OMS_", "GIT_"))}
        self.env.update(HOME=self.temp.name, OMS_LOCK_DIR=str(Path(self.temp.name) / "locks"),
                        GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null")
        subprocess.run(["git", "init", "-q", str(self.repo)], env=self.env, check=True)
        self.data = {"schema": 1, "kind": "oms-study-results", "metric_directions": {"loss": "min"},
                     "suite_id": "smoke", "rows": [
                         {"run_id": "base", "case_id": "case-1", "variant": "baseline", "repeat": 0,
                          "status": "complete", "metrics": {"loss": 2.0}, "params": {"seed": 1}, "artifacts": []},
                         {"run_id": "trial", "case_id": "case-1", "variant": "candidate", "repeat": 0,
                          "status": "complete", "metrics": {"loss": 1.5}, "params": {"seed": 1}, "artifacts": []}]}
        self.source = self.repo / "results.json"
        self.source.write_text(json.dumps(self.data))

    def cli(self, *args, ok=True, env=None):
        done = subprocess.run(["bash", str(ROOT / "scripts/artifact-view.sh"), "--repo", str(self.repo), *args],
                              env=env or self.env, text=True, capture_output=True)
        if ok:
            self.assertEqual(done.returncode, 0, done.stderr)
            return json.loads(done.stdout)
        self.assertNotEqual(done.returncode, 0, done.stdout)
        return done

    def build(self):
        self.source.write_text(json.dumps(self.data))
        return self.cli("build", "--input", str(self.source))

    def test_build_offline_payload_and_overwrite(self):
        meta = self.build()
        page = (self.repo / meta["path"]).read_text()
        payload = json.loads(base64.b64decode(re.search(r'id="oms-payload"[^>]*>([^<]+)', page)[1]))
        self.assertEqual(payload["data"]["suite_id"], "smoke")
        self.assertEqual(len(payload["data"]["rows"]), 2)
        self.assertEqual(payload["view_id"], meta["id"])
        self.assertIn("connect-src 'none'", page)
        self.assertEqual(self.cli("list")["views"][0]["data_sha256"], meta["data_sha256"])
        self.assertIn("output exists", self.cli("build", "--input", str(self.source), ok=False).stderr)
        self.assertEqual(meta["id"], self.cli("build", "--input", str(self.source), "--replace")["id"])

    def test_source_strings_are_never_html(self):
        marker = "</script><script>window.OMS_UNSAFE=true</script>"
        self.data["rows"][0]["case_id"] = marker
        self.data["rows"][0]["params"]["note"] = marker
        meta = self.build()
        page = (self.repo / meta["path"]).read_text()
        self.assertNotIn(marker, page)
        self.assertNotIn("window.OMS_UNSAFE", page)

    def test_content_addressed_artifact_format(self):
        path = self.repo / "object-without-extension"
        path.write_text('{"prediction": 3}')
        self.data["rows"][0]["artifacts"] = [{"path": path.name, "format": "json", "type": "prediction", "label": "Retained output"}]
        self.data["base_id"] = None
        meta = self.build()
        page = (self.repo / meta["path"]).read_text()
        payload = json.loads(base64.b64decode(re.search(r'id="oms-payload"[^>]*>([^<]+)', page)[1]))
        self.assertEqual(payload["data"]["rows"][0]["artifacts"][0]["text"], path.read_text())

    def test_artifact_digest_and_path_binding(self):
        path = self.repo / "pose.sdf"
        path.write_text("molecule\n  OMS\n\n  0  0  0  0  0  0            999 V2000\nM  END\n$$$$\n")
        self.data["rows"][0]["artifacts"] = [{"path": "pose.sdf", "type": "molecule", "label": "pose", "sha256": hashlib.sha256(path.read_bytes()).hexdigest(), "frame": "receptor-A", "alignment": "supplied"}]
        meta = self.build()
        self.assertTrue((self.repo / meta["path"]).exists())
        path.write_text("changed")
        self.assertIn("bytes changed", self.cli("build", "--input", str(self.source), "--replace", ok=False).stderr)
        path.unlink()
        path.symlink_to(self.source)
        self.assertIn("symlink", self.cli("build", "--input", str(self.source), ok=False).stderr)

    def test_invalid_records_refused(self):
        invalid = []
        bad = copy.deepcopy(self.data); bad["rows"].append(bad["rows"][0]); invalid.append(bad)
        bad = copy.deepcopy(self.data); bad["rows"][0]["metrics"]["loss"] = True; invalid.append(bad)
        bad = copy.deepcopy(self.data); bad["rows"][0]["metrics"]["loss"] = 10 ** 400; invalid.append(bad)
        bad = copy.deepcopy(self.data); bad["rows"][0]["params"]["x"] = 10 ** 400; invalid.append(bad)
        bad = copy.deepcopy(self.data); bad["schema"] = True; invalid.append(bad)
        bad = copy.deepcopy(self.data); bad["rows"][0]["repeat"] = -1; invalid.append(bad)
        bad = copy.deepcopy(self.data); bad["rows"][0]["repeat"] = 2 ** 53; invalid.append(bad)
        bad = copy.deepcopy(self.data); bad["rows"][0]["params"]["x"] = "api_" "key=not-a-real-credential"; invalid.append(bad)
        bad = copy.deepcopy(self.data); bad["rows"][0]["artifacts"] = [{"path": "../outside.txt"}]; invalid.append(bad)
        bad = copy.deepcopy(self.data); bad["metric_directions"] = {"loss": {"direction": "min"}}; invalid.append(bad)
        for data in invalid:
            with self.subTest(data=data):
                with self.assertRaises(view.CoreError):
                    view.normalize(data)
        self.assertFalse((self.repo / ".oms/artifact-views").exists())

    def test_feedback_bound_and_idempotent(self):
        meta = self.build()
        feedback = {"schema": 1, "kind": "oms-artifact-feedback", "view_id": meta["id"],
                    "data_sha256": meta["data_sha256"], "events": [{"row_key": meta["row_keys"][0], "action": "accept", "comment": "pose retained"}], "report": {"question": "Does the pose fit?", "next": "Inspect hydrogen bonds"}}
        path = self.repo / "feedback.json"
        path.write_text(json.dumps(feedback))
        self.assertTrue(self.cli("import-events", str(path))["imported"])
        self.assertTrue(self.cli("import-events", str(path))["duplicate"])
        self.assertEqual(len(self.cli("events")["feedback"]), 1)
        feedback["events"][0]["row_key"] = "row-not-in-view"
        path.write_text(json.dumps(feedback))
        self.assertIn("outside this view", self.cli("import-events", str(path), ok=False).stderr)
        feedback["events"] = []; feedback["data_sha256"] = "0" * 64
        path.write_text(json.dumps(feedback))
        self.assertIn("registered source", self.cli("import-events", str(path), ok=False).stderr)

    def test_child_only_reads(self):
        self.build()
        env = dict(self.env, OMS_HARNESS_CHILD="1")
        self.assertEqual(len(self.cli("list", env=env)["views"]), 1)
        self.assertIn("child", self.cli("build", "--input", str(self.source), env=env, ok=False).stderr)

    def test_remote_read_quotes_paths_and_refuses_symlink(self):
        bindir = Path(self.temp.name) / "bin"; bindir.mkdir()
        ssh = bindir / "ssh"
        ssh.write_text("#!/usr/bin/env python3\nimport subprocess,sys\nassert sys.argv[1:6]==['-o','BatchMode=yes','-o','ConnectTimeout=5','--']\nsys.exit(subprocess.call(sys.argv[-1],shell=True))\n")
        ssh.chmod(0o755)
        remote = Path(self.temp.name) / "remote 'quoted' $(false)"; remote.mkdir()
        (remote / "results.json").write_text(json.dumps(self.data))
        env = dict(self.env, PATH=str(bindir) + os.pathsep + self.env["PATH"])
        meta = self.cli("remote", "--host", "fixture", "--remote-root", str(remote), "--input", "results.json", env=env)
        self.assertEqual(meta["snapshot"], "remote read-only; not a live view")
        (remote / "results.json").unlink(); (remote / "results.json").symlink_to(self.source)
        optimized = dict(env, PYTHONOPTIMIZE="1")
        self.assertIn("remote snapshot read failed", self.cli("remote", "--host", "fixture", "--remote-root", str(remote), "--input", "results.json", "--replace", env=optimized, ok=False).stderr)

    def test_remote_transport_is_bounded_before_capture(self):
        bindir = Path(self.temp.name) / "bin"; bindir.mkdir()
        ssh = bindir / "ssh"
        ssh.write_text("#!/usr/bin/env python3\nimport sys\nsys.stderr.buffer.write(b'x'*(8*1024*1024))\nsys.stdout.buffer.write(b'x'*(3*1024*1024))\n")
        ssh.chmod(0o755)
        env = dict(self.env, PATH=str(bindir) + os.pathsep + self.env["PATH"])
        done = self.cli("remote", "--host", "fixture", "--remote-root", "/explicit-root", "--input", "results.json", env=env, ok=False)
        self.assertIn("exceeds 2 MiB", done.stderr)
        self.assertLess(len(done.stderr), 200)

    def test_capsule_adapter_labels_aggregate(self):
        root = self.repo / ".oms/runs/run-a"; root.mkdir(parents=True)
        (root / "capsule.json").write_text(json.dumps({"id": "run-a", "result": {"exit": 0, "metrics": {"loss": 1}}}))
        meta = self.cli("capsules", "run-a")
        page = (self.repo / meta["path"]).read_text()
        payload = json.loads(base64.b64decode(re.search(r'id="oms-payload"[^>]*>([^<]+)', page)[1]))
        self.assertEqual(payload["data"]["rows"][0]["case_id"], "aggregate")

    @unittest.skipUnless(shutil.which("node"), "Node is optional; browser calculation test requires it")
    def test_client_pairing_and_numeric_summaries(self):
        script = r'''
const assert = require('node:assert/strict');
const a = require(process.argv[1]);
const row = (run, variant, score, repeat=0, status='complete', case_id='A') => ({run_id:run,variant,case_id,repeat,status,metrics:{score}});
const rows = [row('b1','base',4),row('b2','base',10,0,'complete','B'),row('t1','trial',2),row('t2','trial',8,0,'complete','B'),row('bad','trial',100,0,'failed'),row('missing','trial',null)];
const paired = a.baselineIndex(rows,'variant:base','score');
assert.equal(a.delta(rows[2],paired,'score'),-2); assert.equal(a.delta(rows[3],paired,'score'),-2);
assert.equal(a.delta(rows[4],paired,'score'),null); assert.equal(a.delta(rows[5],paired,'score'),null);
assert.equal(a.delta(row('repeat','trial',1,1),paired,'score'),null);
const duplicate = a.baselineIndex([...rows,row('dup','base',5)],'variant:base','score');
assert.equal(a.delta(rows[2],duplicate,'score'),null);
assert.equal(a.delta(rows[2],a.baselineIndex(rows,'run:b1','score'),'score'),-2);
const trial = a.groups(rows,'score').find(x=>x.variant==='trial');
assert.equal(trial.count,2);assert.equal(trial.total,4);assert.equal(trial.mean,5);assert.ok(Math.abs(trial.sd-Math.sqrt(18))<1e-12);
assert.equal(a.groups([row('x','v',1e308),row('y','v',1e308)],'score')[0].mean,1e308);
assert.equal(a.delta(row('x','trial',1e308),a.baselineIndex([row('b','base',-1e308)],'variant:base','score'),'score'),null);
'''
        done = subprocess.run(["node", "-e", script, str(ROOT / "scripts/lib/artifact_view_assets/analysis.js")], capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stderr)

    @unittest.skipUnless(shutil.which("node"), "Node is optional; browser calculation test requires it")
    def test_client_scatter_axes_keep_extreme_finite_values_and_ticks_in_range(self):
        script = r'''
const assert = require('node:assert/strict');
const a = require(process.argv[1]);
const domains = [
  [-1e308, 0, 1e308], [-Number.MAX_VALUE, 0, Number.MAX_VALUE],
  [-Number.MIN_VALUE, 0, Number.MIN_VALUE], [0, Number.MIN_VALUE],
  [1e308, 1.0000000000000002e308, 1.0000000000000004e308],
  [-1.0000000000000004e308, -1.0000000000000002e308, -1e308],
  [5,5], [Number.MAX_VALUE,Number.MAX_VALUE], [-0,0]
];
for (const domain of domains) {
  const axis = a.createAxis(domain);
  assert.equal(axis.at(0),axis.min); assert.equal(axis.at(1),axis.max);
  const positions = domain.map(n=>axis.position(n));
  positions.forEach((p,i)=>{assert.ok(Number.isFinite(p) && p>=0 && p<=1);if(i)assert.ok(p>=positions[i-1]);});
  for (const t of [0,.01,.25,.5,.75,.99,1]) {
    const value=axis.at(t), x=68+axis.position(value)*544, y=208-axis.position(value)*180;
    assert.ok(Number.isFinite(value) && value>=axis.min && value<=axis.max);
    assert.ok(Number.isFinite(x) && x>=68 && x<=612);
    assert.ok(Number.isFinite(y) && y>=28 && y<=208);
  }
  if(axis.min===axis.max) assert.equal(positions[0],.5);
  else {assert.equal(positions[0],0);assert.equal(positions.at(-1),1);}
}
const wide=a.createAxis([-1e308,0,1e308]);
assert.equal(wide.position(0),.5);assert.equal(wide.at(.5),0);
assert.equal(a.createAxis(domains[4]).position(domains[4][1]),.5);
assert.throws(()=>a.createAxis([Infinity]),/finite/);
assert.throws(()=>a.createAxis([NaN]),/finite/);
assert.throws(()=>wide.position(Infinity),/finite/);
assert.throws(()=>wide.at(NaN),/finite/);
'''
        done = subprocess.run(["node", "-e", script, str(ROOT / "scripts/lib/artifact_view_assets/analysis.js")],
                              capture_output=True, text=True, timeout=20)
        self.assertEqual(done.returncode, 0, done.stderr)

    @unittest.skipUnless(shutil.which("node"), "Node is optional; browser state test requires it")
    def test_client_saved_annotations_survive_restore_and_selected_export_batches(self):
        script = r'''
const assert = require('node:assert/strict');
const a = require(process.argv[1]);
const keys = Array.from({length:1001},(_,i)=>`row-${i}`), known=new Set([...keys,'unjudged']);
const original = new Map(keys.map((key,i)=>[key,{row_key:key,action:'accept',comment:`검토 ${i} 🧪`} ]));
const state = {selected:new Set([keys[0],keys[1000]]),feedback:original,focused:keys[1000]};
const report = {title:'Review',question:'Question',decision:'Decision',next:'Next step'};
const snapshot = a.snapshotState({search:'case',metric:'loss','molecule-a':'0'},report,state);
const restored = a.restoreState(JSON.parse(JSON.stringify(snapshot)),known);
assert.equal(restored.feedback.size,1001);assert.equal(restored.feedback.get(keys[1000]).comment,'검토 1000 🧪');
assert.deepEqual([...restored.feedback],[...original]);assert.deepEqual(restored.selected,state.selected);
assert.deepEqual(restored.controls,snapshot.controls);assert.deepEqual(restored.report,report);assert.equal(restored.focused,state.focused);
const payload={view_id:'view-fixture',data_sha256:'a'.repeat(64)}, before=JSON.stringify([...restored.feedback]);
assert.throws(()=>a.exportFeedback(payload,restored,report,'all'),/1,000/);
const collected=[];
for (const selected of [keys.slice(0,500),keys.slice(500)]) {
  restored.selected=new Set(selected);
  const result=JSON.parse(a.exportFeedback(payload,restored,report,'selected'));
  assert.equal(result.events.length,selected.length);assert.equal(result.view_id,payload.view_id);assert.equal(result.data_sha256,payload.data_sha256);
  assert.deepEqual(result.report,report);collected.push(...result.events);
}
assert.deepEqual(collected,[...original.values()]);assert.equal(JSON.stringify([...restored.feedback]),before);
restored.selected.clear();assert.throws(()=>a.exportFeedback(payload,restored,report,'selected'),/Select at least one/);
assert.equal(restored.feedback.size,1001);
restored.selected.add('unjudged');
assert.deepEqual(JSON.parse(a.exportFeedback(payload,restored,report,'selected')).events,[{row_key:'unjudged',action:'select',comment:''}]);
const invalid=JSON.parse(JSON.stringify(snapshot));invalid.feedback[0].comment='x'.repeat(4001);
assert.throws(()=>a.restoreState(invalid,known),/4,000/);
assert.equal(original.get(keys[0]).comment,'검토 0 🧪');
const allKeys=Array.from({length:10000},(_,i)=>`saved-${i}`);
const allState={selected:new Set(),feedback:new Map(allKeys.map(k=>[k,{row_key:k,action:'comment',comment:k}])),focused:null};
assert.equal(a.restoreState(a.snapshotState({},report,allState),new Set(allKeys)).feedback.size,10000);
'''
        done = subprocess.run(["node", "-e", script, str(ROOT / "scripts/lib/artifact_view_assets/analysis.js")],
                              capture_output=True, text=True, timeout=20)
        self.assertEqual(done.returncode, 0, done.stderr)

    @unittest.skipUnless(shutil.which("node"), "Node is optional; browser export test requires it")
    def test_client_feedback_utf8_limit_and_selected_batch_import(self):
        self.data["rows"] = [{"run_id": "review", "case_id": "case-%d" % i, "variant": "candidate",
                              "metrics": {"loss": i}, "params": {}, "artifacts": []} for i in range(100)]
        meta = self.build()
        script = r'''
const assert=require('node:assert/strict'),fs=require('node:fs');
const a=require(process.argv[1]),meta=JSON.parse(fs.readFileSync(0,'utf8'));
const state={selected:new Set(),feedback:new Map(meta.row_keys.map(k=>[k,{row_key:k,action:'accept',comment:'한'.repeat(4000)}]))};
const payload={view_id:meta.id,data_sha256:meta.data_sha256},report={title:'Unicode review',next:'Keep every comment'};
const before=JSON.stringify([...state.feedback]);
assert.throws(()=>a.exportFeedback(payload,state,report,'all'),/1 MiB/);
assert.equal(JSON.stringify([...state.feedback]),before);
state.selected=new Set(meta.row_keys.slice(0,50));
const json=a.exportFeedback(payload,state,report,'selected');
assert.ok(Buffer.byteLength(json,'utf8')<=1024*1024);
const batch=JSON.parse(json);assert.equal(batch.events.length,50);assert.equal(batch.events[49].comment,'한'.repeat(4000));
assert.equal(JSON.stringify([...state.feedback]),before);
process.stdout.write(json);
'''
        done = subprocess.run(["node", "-e", script, str(ROOT / "scripts/lib/artifact_view_assets/analysis.js")],
                              input=json.dumps(meta), capture_output=True, text=True, timeout=20)
        self.assertEqual(done.returncode, 0, done.stderr)
        exported = self.repo / "selected-feedback.json"
        exported.write_text(done.stdout, encoding="utf-8")
        self.assertLessEqual(exported.stat().st_size, 1024 * 1024)
        receipt = self.cli("import-events", str(exported))
        self.assertTrue(receipt["imported"])
        self.assertEqual(receipt["events"], 50)
        self.assertTrue(self.cli("import-events", str(exported))["duplicate"])


if __name__ == "__main__":
    unittest.main()
