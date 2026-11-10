"""Study-only contracts: offline real argv execution and immutable provenance.

These paths have no preceding suite; shared runtime primitives retain their
existing tests. No provider, GPU, network, scheduler, or user cache is used.
"""
from __future__ import annotations

from copy import deepcopy
import importlib.util
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
from oms_runtime.common import CoreError, canonical_json, sha256_bytes  # noqa: E402
from oms_studies import execution, records, selection, suite, workflow  # noqa: E402
from oms_studies.common import blob, load, read_results  # noqa: E402


GENERATOR = '''import json, pathlib, sys
value = json.loads(pathlib.Path(sys.argv[1]).read_text())
out = {"answer": value["input"]["x"] + value["params"]["offset"]}
pathlib.Path(sys.argv[2]).write_text(json.dumps(out))
counter = pathlib.Path(sys.argv[3])
counter.write_text(str(int(counter.read_text()) + 1) if counter.exists() else "1")
'''
EVALUATOR = '''import json, pathlib, sys
case = json.loads(pathlib.Path(sys.argv[1]).read_text())
prediction = json.loads(pathlib.Path(sys.argv[2]).read_text())
pathlib.Path(sys.argv[3]).write_text(json.dumps({"metrics": {
 "error": abs(prediction["answer"] - case["expected"]), "cost": 1 + abs(case["params"]["offset"])}}))
'''
DVC_FIXTURE = '''import json, pathlib, subprocess, sys
args = sys.argv[1:]
if args == ["init", "--no-scm"]:
    pathlib.Path(".dvc").mkdir()
elif args[:2] == ["config", "cache.dir"]:
    pathlib.Path(".dvc/cache-dir").write_text(args[2])
elif args[:3] == ["status", "--json", "--no-updates"]:
    print("{}")
elif args[:2] == ["repro", "--single-item"] and len(args) == 3:
    name = args[2].split(":", 1)[1]
    stages = json.loads(pathlib.Path("dvc.yaml").read_text())["stages"]
    with pathlib.Path("invoked.txt").open("a") as out: out.write(name + "\\n")
    subprocess.run(stages[name]["cmd"], shell=True, check=True)
else:
    raise SystemExit("unsupported fixture DVC argv: " + repr(args))
'''


class StudyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="oms-study-test.")
        self.repo = Path(self.temp.name) / "repo"
        self.repo.mkdir()
        self.home = Path(self.temp.name) / "home"
        self.home.mkdir()
        self.environment = patch.dict(os.environ, {"HOME": str(self.home), "OMS_LOCK_DIR": str(self.home / "locks"),
                                                   "PYTHONDONTWRITEBYTECODE": "1", "OMS_HARNESS_CHILD": "0",
                                                   "OMS_HARNESS_DELEGATE_DEPTH": "0"})
        self.environment.start()
        self.write("generate.py", GENERATOR)
        self.write("evaluate.py", EVALUATOR)
        self.write("fixture.txt", "unchanged")
        self.spec = {"schema": 1, "kind": "oms-study-suite", "base_id": "fixture-v1",
                     "cases": [{"case_id": "a", "input": {"x": 2}, "expected": 3, "fixtures": {"data": "fixture.txt"}},
                               {"case_id": "b", "input": {"x": 4}, "expected": 5}],
                     "variants": [{"name": name, "params": {"offset": offset},
                                   "command": self.command("generate.py", "{input}", "{output}", str(self.repo / "counter"))}
                                  for name, offset in (("base", 0), ("change", 1))],
                     "repeats": [0, 1], "evaluator": self.command("evaluate.py", "{input}", "{prediction}", "{output}"),
                     "metric_directions": {"error": "min", "cost": "min"}}

    def tearDown(self):
        self.environment.stop()
        self.temp.cleanup()

    def write(self, name, value):
        path = self.repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value if isinstance(value, str) else json.dumps(value), encoding="utf-8")
        return path

    def command(self, name, *args, timeout=10):
        return {"argv": [sys.executable, "{file:" + name + "}", *args], "files": [name], "timeout_seconds": timeout}

    def make(self, name="suite"):
        return suite.create(self.repo, self.spec, name)

    def run_fixture(self, name="batch", **kwargs):
        self.make()
        return suite.run_suite(self.repo, "suite", batch_id=name, **kwargs)

    def path(self, name="batch"):
        return ".oms/studies/results/" + name + ".json"

    def cli(self, *args, child=False):
        env = dict(os.environ, OMS_HARNESS_CHILD="1" if child else "0")
        return subprocess.run(["bash", str(ROOT / "scripts/study.sh"), "--repo", str(self.repo), *args],
                              text=True, capture_output=True, env=env, timeout=30)

    def test_planned_coverage_missing_selection_and_real_pairwise_execution(self):
        self.make()
        first = suite.coverage(self.repo, "suite")
        self.assertEqual(first["missing_count"], 8)
        self.assertFalse((self.repo / "counter").exists())
        part = suite.run_suite(self.repo, "suite", selected=[first["missing"][0]["cell_id"]], batch_id="one")
        self.assertEqual(part["rows"][0]["status"], "complete")
        missing = suite.coverage(self.repo, "suite")
        self.assertEqual(missing["missing_count"], 7)
        rest = suite.run_suite(self.repo, "suite", missing=True, batch_id="rest")
        self.assertEqual(len(rest["rows"]), 7)
        self.assertEqual(suite.coverage(self.repo, "suite")["completed_count"], 8)
        combined = {k: v for k, v in rest.items() if k != "content_sha256"}
        combined["rows"] = part["rows"] + rest["rows"]
        paired = selection.compare(combined, "base", "change")
        self.assertEqual(paired["paired_count"], 4)
        self.assertEqual(paired["metrics"]["error"]["right_wins"], 4)
        self.assertEqual(paired["metrics"]["cost"]["left_wins"], 4)
        self.assertTrue(paired["controls"]["recorded"])
        with self.assertRaisesRegex(CoreError, "no cells"):
            suite.run_suite(self.repo, "suite", missing=True)

    def test_source_files_are_frozen_before_suite_run(self):
        self.make()
        self.write("generate.py", "raise SystemExit(99)")
        self.write("fixture.txt", "changed")
        value = suite.run_suite(self.repo, "suite", limit=1)
        self.assertEqual(value["rows"][0]["status"], "complete")
        artifact = next(a for a in value["rows"][0]["artifacts"] if a["type"] == "fixture")
        self.assertEqual(blob(self.repo, artifact), b"unchanged")

    def test_duplicate_attempts_are_visible_and_not_double_counted(self):
        first = self.run_fixture(limit=1)
        second = suite.run_suite(self.repo, "suite", limit=1, batch_id="again")
        coverage = suite.coverage(self.repo, "suite")
        self.assertEqual(coverage["completed_count"], 1)
        self.assertEqual(len(coverage["duplicates"]), 1)
        data = {k: v for k, v in first.items() if k != "content_sha256"}
        data["rows"] += second["rows"]
        extra = deepcopy(data["rows"][0]); extra.update(run_id="other", variant="change")
        data["rows"].append(extra)
        with self.assertRaisesRegex(CoreError, "duplicate completed"):
            selection.compare(data, "base", "change")

    def test_suite_rejects_duplicate_axes_and_invalid_values(self):
        for key, value in (("cases", [self.spec["cases"][0]] * 2), ("variants", [self.spec["variants"][0]] * 2),
                           ("repeats", [1, 1]), ("repeats", [True]), ("schema", True)):
            bad = deepcopy(self.spec); bad[key] = value
            with self.subTest(key=key), self.assertRaises(CoreError):
                suite.create(self.repo, bad)
        bad = deepcopy(self.spec); bad["variants"][0]["params"]["offset"] = float("inf")
        with self.assertRaises(CoreError):
            suite.create(self.repo, bad)

    def test_failed_missing_and_nonfinite_evaluators_leave_missing_coverage(self):
        scripts = ["raise SystemExit(4)", "pass",
                   "import pathlib,sys; pathlib.Path(sys.argv[3]).write_text('{\"metrics\":{\"error\":NaN,\"cost\":1}}')",
                   "import pathlib,sys; pathlib.Path(sys.argv[3]).write_text('{\"metrics\":{\"error\":0}}')"]
        for index, code in enumerate(scripts):
            with self.subTest(index=index):
                name = "bad%d" % index
                self.write("bad.py", code)
                self.spec["evaluator"] = self.command("bad.py", "{input}", "{prediction}", "{output}")
                self.make(name)
                value = suite.run_suite(self.repo, name, limit=1)
                self.assertEqual(value["rows"][0]["status"], "failed")
                self.assertEqual(value["rows"][0]["metrics"], {})
                self.assertEqual(suite.coverage(self.repo, name)["completed_count"], 0)

    def test_missing_generator_output_is_failed(self):
        self.write("generate.py", "pass")
        value = self.run_fixture(limit=1)
        self.assertEqual(value["rows"][0]["status"], "failed")
        self.assertNotIn("evaluation", value["rows"][0])

    def test_rescore_new_pass_hashes_and_no_generation_rerun(self):
        source = self.run_fixture(limit=2)
        original = (self.repo / self.path()).read_bytes()
        value = suite.rescore(self.repo, self.path(), self.spec["evaluator"], pass_id="eval-one")
        self.assertFalse(value["generation_rerun"])
        self.assertEqual((self.repo / "counter").read_text(), "2")
        self.assertEqual((self.repo / self.path()).read_bytes(), original)
        self.assertEqual(value["source_sha256"], sha256_bytes(original))
        self.assertEqual(value["rows"][0]["source_run_id"], source["rows"][0]["run_id"])
        self.assertNotEqual(value["rows"][0]["run_id"], source["rows"][0]["run_id"])
        self.assertEqual(len(value["evaluator_sha256"]), 64)
        self.assertEqual(len(value["rows"][0]["prediction_sha256"]), 64)
        with self.assertRaisesRegex(CoreError, "already exists"):
            suite.rescore(self.repo, self.path(), self.spec["evaluator"], pass_id="eval-one")
        self.assertEqual(suite.coverage(self.repo, "suite")["completed_count"], 2)

    def test_tampered_source_artifact_prevents_any_rescore_execution(self):
        source = self.run_fixture(limit=1)
        prediction = next(a for a in source["rows"][0]["artifacts"] if a["type"] == "prediction")
        path = self.repo / prediction["path"]
        path.chmod(0o600); path.write_text("tampered")
        with self.assertRaisesRegex(CoreError, "hash mismatch"):
            suite.rescore(self.repo, self.path(), self.spec["evaluator"])
        self.assertFalse((self.repo / ".oms/studies/work/evaluations").exists())

    def test_tampered_record_is_not_accepted(self):
        self.run_fixture(limit=1)
        path = self.repo / self.path()
        data = json.loads(path.read_text()); data["rows"][0]["metrics"]["error"] = 999
        path.chmod(0o600); path.write_text(json.dumps(data))
        with self.assertRaisesRegex(CoreError, "record hash mismatch"):
            read_results(self.repo, self.path())

    def test_evaluator_cannot_silently_mutate_local_prediction(self):
        self.run_fixture(limit=1)
        self.write("mutate.py", EVALUATOR + '\npathlib.Path(sys.argv[2]).chmod(0o600)\npathlib.Path(sys.argv[2]).write_text("changed")\n')
        value = suite.rescore(self.repo, self.path(), self.command("mutate.py", "{input}", "{prediction}", "{output}"))
        self.assertEqual(value["rows"][0]["status"], "failed")
        self.assertIn("modified", value["rows"][0]["error"])
        source = read_results(self.repo, self.path())[0]
        prediction = next(a for a in source["rows"][0]["artifacts"] if a["type"] == "prediction")
        self.assertEqual(json.loads(blob(self.repo, prediction))["answer"], 2)

    def test_timeout_and_bounded_logs(self):
        self.write("timeout.py", 'import time; print("x" * 100000, flush=True); time.sleep(20)')
        command = execution.freeze(self.repo, self.command("timeout.py", timeout=1))
        cwd = self.repo / "work"; cwd.mkdir()
        receipt = execution.execute(self.repo, command, cwd, {})
        self.assertEqual(receipt["status"], "failed")
        self.assertIn("timeout", receipt["error"])
        self.assertTrue(receipt["log_truncated"])
        self.assertLessEqual(len(receipt["stdout"].encode()), execution.MAX_LOG)

    def test_interrupted_rescore_persists_terminal_record(self):
        self.run_fixture(limit=1)
        marker = self.repo / "started"
        self.write("slow.py", 'import pathlib,time; pathlib.Path(' + repr(str(marker)) + ').write_text("started"); time.sleep(20)')
        self.write("slow.json", self.command("slow.py", "{input}", "{prediction}", "{output}", timeout=30))
        process = subprocess.Popen(["bash", str(ROOT / "scripts/study.sh"), "--repo", str(self.repo), "rescore",
                                    "--results", self.path(), "--evaluator", "slow.json", "--id", "interrupted"],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=os.environ.copy())
        try:
            deadline = time.monotonic() + 10
            while not marker.exists() and process.poll() is None and time.monotonic() < deadline:
                time.sleep(0.03)
            self.assertTrue(marker.exists())
            process.send_signal(signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 130, stderr)
            value = json.loads(stdout)
            self.assertEqual(value["rows"][0]["status"], "interrupted")
            self.assertEqual(load(self.repo, "results", "interrupted")["evaluation_pass_id"], "interrupted")
            self.assertEqual((self.repo / "counter").read_text(), "1")
        finally:
            if process.poll() is None:
                process.kill(); process.communicate()

    def test_annotation_history_export_and_original_immutability(self):
        source = self.run_fixture(limit=1)
        original = (self.repo / self.path()).read_bytes()
        run_id = source["rows"][0]["run_id"]
        first = records.annotate(self.repo, self.path(), run_id, 4, "manual correction", annotation_id="a1")
        with self.assertRaisesRegex(CoreError, "explicitly supersede"):
            records.annotate(self.repo, self.path(), run_id, 5, "later correction")
        second = records.annotate(self.repo, self.path(), run_id, 5, "reviewed correction", supersedes="a1", annotation_id="a2")
        chain = records.history(self.repo, run_id)["annotations"]
        self.assertEqual([a["annotation_id"] for a in chain], ["a1", "a2"])
        self.assertEqual(second["previous_sha256"], first["content_sha256"])
        exported = records.export_annotations(self.repo, "suite", ["a2"], "corrected")
        self.assertEqual(len(exported["cases"]), 1)
        self.assertEqual(exported["cases"][0]["expected"], 5)
        self.assertEqual(load(self.repo, "suites", "suite")["cases"][0]["expected"], 3)
        self.assertEqual((self.repo / self.path()).read_bytes(), original)
        rerun = suite.run_suite(self.repo, "corrected", limit=1)
        self.assertEqual(rerun["rows"][0]["metrics"]["error"], 3)
        with self.assertRaisesRegex(CoreError, "one annotation per case"):
            records.export_annotations(self.repo, "suite", ["a1", "a2"], "invalid")

    def test_baseline_named_immutable_snapshot_and_rationale(self):
        source = self.run_fixture(limit=1)
        value = records.bind_baseline(self.repo, "release", self.path(), "reference at review")
        self.assertEqual(value["rows"][0]["metrics"], source["rows"][0]["metrics"])
        with self.assertRaisesRegex(CoreError, "already exists"):
            records.bind_baseline(self.repo, "release", self.path(), "different rationale")
        path = self.repo / self.path(); path.chmod(0o600); path.write_text("edited external result")
        shown = records.baseline(self.repo, "release")
        self.assertEqual(shown["source_sha256"], value["source_sha256"])
        self.assertEqual(shown["rationale"], "reference at review")
        with self.assertRaisesRegex(CoreError, "rationale"):
            records.bind_baseline(self.repo, "empty", self.path(), "")

    def test_pareto_constraints_ties_and_no_import_of_optuna(self):
        rows = []
        for i, (quality, cost) in enumerate(((5, 4), (4, 2), (3, 5), (5, 4), (9, 99))):
            rows.append({"run_id": "r%d" % i, "case_id": "one", "variant": "v%d" % i, "repeat": 0,
                         "status": "complete", "metrics": {"quality": quality, "cost": cost}, "params": {}, "artifacts": []})
        value = {"schema": 1, "kind": "oms-study-results", "metric_directions": {"quality": "max", "cost": "min"}, "rows": rows}
        with patch("importlib.import_module", side_effect=AssertionError("optional dependency imported")):
            front = selection.pareto(value, constraints={"cost": {"max": 10}})
        self.assertEqual([r["run_id"] for r in front["front"]], ["r0", "r1", "r3"])
        self.assertEqual(front["dominated_run_ids"], ["r2"])
        self.assertEqual(front["excluded"][0]["run_id"], "r4")
        self.assertFalse(front["search_performed"])
        with self.assertRaises(CoreError):
            selection.pareto(value, constraints={"cost": {"min": 20, "max": 10}})

    def test_optional_optuna_missing_has_actionable_error_and_no_proposals(self):
        self.run_fixture(limit=1)
        with patch("importlib.import_module", side_effect=ImportError("missing")):
            with self.assertRaisesRegex(CoreError, "optional Optuna"):
                selection.propose(self.repo, self.path(), {"offset": {"type": "int", "low": 0, "high": 2}})
        self.assertFalse((self.repo / ".oms/studies/proposals").exists())

    @unittest.skipUnless(importlib.util.find_spec("optuna"), "optional Optuna is not installed")
    def test_real_optuna_adaptive_proposals(self):
        self.run_fixture(limit=4)
        value = selection.propose(self.repo, self.path(), {"offset": {"type": "int", "low": 0, "high": 8}}, count=2)
        self.assertEqual(len(value["proposals"]), 2)
        self.assertTrue(value["search_performed"])
        self.assertFalse(value["execution_performed"])
        self.assertTrue(all(p["params"]["offset"] not in (0, 1) for p in value["proposals"]))

    def test_cli_json_and_child_guard_are_read_only(self):
        self.write("suite.json", self.spec)
        created = self.cli("suite", "create", "--spec", "suite.json", "--id", "suite")
        self.assertEqual(created.returncode, 0, created.stderr)
        before = {p.relative_to(self.repo).as_posix(): p.read_bytes() for p in self.repo.rglob("*") if p.is_file()}
        for command in (("suite", "run", "--suite", "suite"),
                        ("suite", "create", "--spec", "suite.json"),
                        ("baseline", "bind", "--name", "x", "--results", "no.json", "--rationale", "test")):
            with self.subTest(command=command):
                child = self.cli(*command, child=True)
                self.assertEqual(child.returncode, 2)
                self.assertIn("read-only", child.stderr)
        shown = self.cli("suite", "coverage", "--suite", "suite", child=True)
        self.assertEqual(shown.returncode, 0, shown.stderr)
        self.assertEqual(json.loads(shown.stdout)["missing_count"], 8)
        after = {p.relative_to(self.repo).as_posix(): p.read_bytes() for p in self.repo.rglob("*") if p.is_file()}
        self.assertEqual(before, after)

    def test_argv_has_no_shell_interpolation(self):
        self.write("literal.py", 'import pathlib,sys; pathlib.Path("literal.txt").write_text(sys.argv[1])')
        command = execution.freeze(self.repo, self.command("literal.py", "$(touch BAD); echo secret"))
        cwd = self.repo / "exec"; cwd.mkdir()
        receipt = execution.execute(self.repo, command, cwd, {})
        self.assertEqual(receipt["status"], "complete")
        self.assertEqual((cwd / "literal.txt").read_text(), "$(touch BAD); echo secret")
        self.assertFalse((cwd / "BAD").exists())

    def test_interpreter_invocation_path_preserves_virtualenv_routing(self):
        link = self.repo / "python-link"
        link.symlink_to(sys.executable)
        command = execution.freeze(self.repo, {"argv": [str(link), "-c", "import sys; print(sys.executable)"]})
        self.assertEqual(command["executable"], str(link))
        receipt = execution.execute(self.repo, command, self.repo, {})
        self.assertEqual(receipt["status"], "complete")
        self.assertEqual(receipt["stdout"].strip(), str(link))

    def test_depth_only_child_cannot_write_or_execute(self):
        self.make()
        before = {p.relative_to(self.repo).as_posix(): p.read_bytes() for p in self.repo.rglob("*") if p.is_file()}
        with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "0", "OMS_HARNESS_DELEGATE_DEPTH": "1"}):
            value = self.cli("suite", "run", "--suite", "suite")
            self.assertEqual(value.returncode, 2)
            self.assertIn("read-only", value.stderr)
            with self.assertRaisesRegex(CoreError, "read-only"):
                suite.run_suite(self.repo, "suite")
            self.assertEqual(suite.coverage(self.repo, "suite")["missing_count"], 8)
        after = {p.relative_to(self.repo).as_posix(): p.read_bytes() for p in self.repo.rglob("*") if p.is_file()}
        self.assertEqual(before, after)

    def test_artifact_format_is_explicit_and_preserved_through_rescore(self):
        self.spec["variants"][0]["output_format"] = "json"
        source = self.run_fixture(limit=1)
        artifacts = {a["type"]: a for a in source["rows"][0]["artifacts"]}
        self.assertEqual(artifacts["input"]["format"], "json")
        self.assertEqual(artifacts["metrics"]["format"], "json")
        self.assertEqual(artifacts["prediction"]["format"], "json")
        self.assertEqual(artifacts["fixture"]["format"], "txt")
        rescored = suite.rescore(self.repo, self.path(), self.spec["evaluator"])
        prediction = next(a for a in rescored["rows"][0]["artifacts"] if a["type"] == "prediction")
        self.assertEqual(prediction["format"], "json")

    def test_symlink_and_oversized_inputs_fail_closed(self):
        (self.repo / "link.py").symlink_to(self.repo / "generate.py")
        with self.assertRaisesRegex(CoreError, "symlink"):
            execution.freeze(self.repo, self.command("link.py"))
        outside = Path(self.temp.name) / "outside"; outside.mkdir()
        (self.repo / "linked").symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(CoreError, "symlink"):
            read_results(self.repo, "linked/results.json")
        with self.assertRaises(CoreError):
            read_results(self.repo, "../outside/results.json")
        with (self.repo / "large.py").open("wb") as handle:
            handle.truncate(33 * 1024 * 1024)
        with self.assertRaisesRegex(CoreError, "oversized"):
            execution.freeze(self.repo, self.command("large.py"))

    def make_workflow(self):
        self.write("dvc-fixture.py", DVC_FIXTURE)
        self.write("source.txt", "2")
        self.write("offset.txt", "1")
        self.write("next-offset.txt", "3")
        self.write("step.py", 'import pathlib,sys\nmode=sys.argv[1]\n'
                   'if mode=="prepare": pathlib.Path("prepared.txt").write_text(pathlib.Path("source.txt").read_text())\n'
                   'elif mode=="score": pathlib.Path("scored.txt").write_text(str(int(pathlib.Path("prepared.txt").read_text())+int(pathlib.Path("offset.txt").read_text())))\n'
                   'else: pathlib.Path("report.txt").write_text(pathlib.Path("scored.txt").read_text())\n')
        stages = {"prepare": {"cmd": "python3 step.py prepare", "deps": ["step.py", "source.txt"], "outs": ["prepared.txt"]},
                  "score": {"cmd": "python3 step.py score", "deps": ["step.py", "prepared.txt", "offset.txt"], "outs": ["scored.txt"]},
                  "report": {"cmd": "python3 step.py report", "deps": ["step.py", "scored.txt"], "outs": ["report.txt"]}}
        self.write("dvc.yaml", {"stages": stages})
        self.write("prepared.txt", "2"); self.write("scored.txt", "3"); self.write("report.txt", "3")
        return workflow.capture(self.repo, "workflow", self.command("dvc-fixture.py"))

    def test_dvc_fork_real_command_invocation_and_exact_upstream_reuse(self):
        captured = self.make_workflow()
        planned = workflow.plan(self.repo, "workflow", {"offset.txt": "next-offset.txt"})
        self.assertEqual(planned["directly_changed"], ["score"])
        self.assertEqual(planned["affected_downstream"], ["report"])
        self.assertEqual(planned["reused_stages"], ["prepare"])
        fork = workflow.fork(self.repo, "workflow", {"offset.txt": "next-offset.txt"}, name="fork",
                             expected_plan_sha256=planned["plan_sha256"])
        self.assertEqual(fork["status"], "complete")
        self.assertTrue(fork["reuse_verified"])
        self.assertEqual(set(fork["changed_output_paths"]), {"scored.txt", "report.txt"})
        self.assertEqual(blob(self.repo, fork["outputs"]["report.txt"]), b"5")
        self.assertEqual(blob(self.repo, captured["outputs"]["report.txt"]), b"3")
        cwd = self.repo / fork["workspace"]
        self.assertEqual((cwd / "invoked.txt").read_text(), "score\nreport\n")
        self.assertEqual((self.repo / "report.txt").read_text(), "3")
        self.assertEqual(fork["native_cache_hits"], "unknown")

    def test_workflow_plan_digest_rejects_changed_replacement(self):
        self.make_workflow()
        planned = workflow.plan(self.repo, "workflow", {"offset.txt": "next-offset.txt"})
        self.write("next-offset.txt", "4")
        with self.assertRaisesRegex(CoreError, "plan changed"):
            workflow.fork(self.repo, "workflow", {"offset.txt": "next-offset.txt"}, expected_plan_sha256=planned["plan_sha256"])
        self.assertFalse((self.repo / ".oms/studies/work/workflows").exists())

    def test_workflow_rejects_unsupported_yaml_and_cycles(self):
        with self.assertRaisesRegex(CoreError, "JSON-encoded"):
            workflow.pipeline(b"stages:\n  foo:\n    cmd: echo x\n")
        with self.assertRaisesRegex(CoreError, "cycle"):
            workflow.pipeline(canonical_json({"stages": {
                "one": {"cmd": "true", "deps": ["b"], "outs": ["a"]},
                "two": {"cmd": "true", "deps": ["a"], "outs": ["b"]}}}))
        with self.assertRaisesRegex(CoreError, "cmd/deps/outs"):
            workflow.pipeline(canonical_json({"stages": {"one": {"cmd": "true", "params": ["x"], "outs": ["a"]}}}))

    def test_workflow_detects_modified_reused_outputs(self):
        self.make_workflow()
        changed = json.loads((self.repo / "dvc.yaml").read_text())
        changed["stages"]["score"]["cmd"] += '; echo broken > prepared.txt'
        self.write("changed-dvc.json", changed)
        value = workflow.fork(self.repo, "workflow", {"dvc.yaml": "changed-dvc.json"})
        self.assertEqual(value["status"], "failed")
        self.assertFalse(value["reuse_verified"])
        self.assertEqual(value["reuse_violations"], ["prepared.txt"])

    def test_workflow_capture_rejects_stale_native_status(self):
        self.make_workflow()
        self.write("stale-dvc.py", 'print(\'{"score":[{"changed deps":{"offset.txt":"modified"}}]}\')')
        with self.assertRaisesRegex(CoreError, "changed or missing"):
            workflow.capture(self.repo, "stale", self.command("stale-dvc.py"))
        self.assertFalse((self.repo / ".oms/studies/workflows/stale.json").exists())

    def test_workflow_failed_stage_retains_receipt_and_unrequested_downstream(self):
        self.make_workflow()
        changed = json.loads((self.repo / "dvc.yaml").read_text())
        changed["stages"]["score"]["cmd"] = 'python3 -c "raise SystemExit(9)"'
        self.write("changed-dvc.json", changed)
        value = workflow.fork(self.repo, "workflow", {"dvc.yaml": "changed-dvc.json"})
        self.assertEqual(value["status"], "failed")
        self.assertEqual(value["adapter_receipts"][-1]["status"], "failed")
        self.assertEqual((self.repo / value["workspace"] / "invoked.txt").read_text(), "score\n")

    def test_workflow_detects_frozen_input_mutation(self):
        self.make_workflow()
        changed = json.loads((self.repo / "dvc.yaml").read_text())
        changed["stages"]["score"]["cmd"] += '; echo changed > source.txt'
        self.write("changed-dvc.json", changed)
        value = workflow.fork(self.repo, "workflow", {"dvc.yaml": "changed-dvc.json"})
        self.assertEqual(value["status"], "failed")
        self.assertEqual(value["input_violations"], ["source.txt"])

    @unittest.skipUnless(shutil.which("dvc"), "optional DVC CLI is not installed")
    def test_real_dvc_file_stage_adapter(self):
        self.make_workflow()
        env = dict(os.environ, DVC_NO_ANALYTICS="1")
        for args in (["init", "--no-scm"], ["repro"]):
            probe = subprocess.run(["dvc", *args], cwd=self.repo, env=env, capture_output=True, text=True, timeout=30)
            self.assertEqual(probe.returncode, 0, probe.stderr)
        workflow.capture(self.repo, "real-dvc", {"argv": ["dvc"], "timeout_seconds": 30})
        value = workflow.fork(self.repo, "real-dvc", {"offset.txt": "next-offset.txt"})
        self.assertEqual(value["status"], "complete")
        self.assertTrue(value["reuse_verified"])
        self.assertEqual(blob(self.repo, value["outputs"]["report.txt"]), b"5")


if __name__ == "__main__":
    unittest.main()
