"""Work views use canonical schema-2 snapshots; no provider or network needed."""
from copy import deepcopy
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
CLI = "--cli" in sys.argv
if CLI:
    sys.argv.remove("--cli")
spec = importlib.util.spec_from_file_location("work_inspect", ROOT / "scripts/lib/oms_runtime/work_inspect.py")
w = importlib.util.module_from_spec(spec)
spec.loader.exec_module(w)


def task(ident="t1", state="ready", depends=None):
    return {"id": ident, "title": "구현 " + ident, "state": state, "depends": depends or [],
            "verify_digest": "c" * 64, "verify_present": True}


def criterion(ident="req1", status="missing", tid="t1"):
    return {"id": ident, "text": "확인할 기준", "status": status, "source": "plan-task",
            "plan_id": "plan_" + "1" * 32, "plan_task_id": tid, "evidence": []}


def envelope():
    return {"schema": 2, "generated_at": "2026-11-10T00:00:00Z", "state_digest": "a" * 64,
            "repo": {"head": "b" * 40, "branch": "fixture"}, "task": {"present": False},
            "plan": {"present": True, "plan_id": "plan_" + "1" * 32,
                     "tasks": [task()], "actionable": ["t1"], "contract": {"bound": True, "satisfied": True}},
            "criteria": [criterion()],
            "sources": [{"path": "PROJECT.md", "sha256": "d" * 64}],
            "next_actions": [{"id": "execute_ready_task", "priority": 80,
                              "authority": "worktree_write", "reason": "one task ready", "command": "oms plan-run --next"}]}


class MatrixTests(unittest.TestCase):
    def test_proven_plan_link_and_verifier_digest(self):
        report = w.evidence_matrix(envelope())
        row = report["criteria"]["items"][0]
        self.assertEqual(row["task_id"], "t1")
        self.assertEqual(row["verify_digest"], "c" * 64)
        self.assertEqual(row["status"], "missing")
        self.assertFalse(report["authoritative"])
        self.assertEqual(report["tasks_without_criterion_link"]["total"], 0)

    def test_foreign_plan_does_not_link_same_task_id(self):
        data = envelope(); data["criteria"][0]["plan_id"] = "plan_foreign"
        report = w.evidence_matrix(data)
        self.assertIsNone(report["criteria"]["items"][0]["task_id"])
        self.assertEqual(report["tasks_without_criterion_link"]["items"], ["t1"])

    def test_project_requirement_does_not_inherit_done_task(self):
        data = envelope(); data["criteria"][0] = dict(criterion(), source="project", plan_task_id="")
        data["plan"]["tasks"][0]["state"] = "done"
        row = w.evidence_matrix(data)["criteria"]["items"][0]
        self.assertEqual(row["status"], "missing")
        self.assertEqual(row["link"], "unrecorded")

    def test_active_task_link_is_separate(self):
        data = envelope(); data["task"]["task_id"] = "active2"
        data["criteria"] = [dict(criterion(), plan_task_id="", active_task_id="active2", source="task")]
        row = w.evidence_matrix(data, task_id="active2")["criteria"]["items"][0]
        self.assertEqual(row["link"], "active-task-lineage")
        self.assertIsNone(row["verify_digest"])

    def test_missing_task_filter_errors(self):
        with self.assertRaises(w.WorkViewError):
            w.evidence_matrix(envelope(), task_id="absent")

    def test_output_bound_counts_all_statuses_and_nested_evidence(self):
        data = envelope()
        data["criteria"] = [criterion("r1", "verified"), criterion("r2", "stale")]
        data["criteria"][0]["evidence"] = [{"evidence_ref": "e1"}, {"evidence_ref": "e2"}]
        report = w.evidence_matrix(data, limit=1)
        self.assertEqual(report["criteria"]["omitted"], 1)
        self.assertEqual(report["status_counts"], {"stale": 1, "verified": 1})
        self.assertEqual(report["criteria"]["items"][0]["evidence"]["omitted"], 1)
        self.assertIn("evidence omitted: 1", w.render(report))

    def test_filter_count_does_not_change_total_declared(self):
        data = envelope(); data["plan"]["tasks"].append(task("t2"))
        data["criteria"].append(criterion("r2", tid="t2"))
        report = w.evidence_matrix(data, task_id="t2")
        self.assertEqual(report["criteria"]["total"], 1)
        self.assertEqual(report["total_declared_criteria"], 2)


class InboxTests(unittest.TestCase):
    def test_cancelled_dependency_is_not_satisfied(self):
        data = envelope(); data["plan"]["actionable"] = []
        data["plan"]["tasks"] = [task(depends=["t2"]), task("t2", "cancelled")]
        row = w.action_inbox(data, task_id="t1")["tasks"]["items"][0]
        self.assertEqual(row["depends_waiting"]["items"][0]["state"], "cancelled")
        self.assertIsNone(row["human_decision_required"])

    def test_done_dependency_is_not_blocker(self):
        data = envelope(); data["plan"]["tasks"] = [task(depends=["t2"]), task("t2", "done")]
        row = w.action_inbox(data, task_id="t1")["tasks"]["items"][0]
        self.assertEqual(row["depends_waiting"]["total"], 0)
        self.assertEqual(row["reasons"][0]["code"], "ready")

    def test_unknown_dependency_and_cycle_are_bounded(self):
        data = envelope(); data["plan"]["actionable"] = []
        data["plan"]["tasks"] = [task(depends=["t2", "gone"]), task("t2", depends=["t1"])]
        rows = w.action_inbox(data, limit=1)["tasks"]
        self.assertEqual(rows["omitted"], 1)
        self.assertEqual(rows["items"][0]["depends_waiting"]["omitted"], 1)
        data["plan"]["tasks"][0]["depends"] = ["gone"]
        row = w.action_inbox(data)["tasks"]["items"][0]
        self.assertEqual(row["depends_waiting"]["items"][0]["reason"], "dependency-missing")

    def test_contract_blocker_and_contradictory_actionable(self):
        data = envelope(); data["plan"]["contract"] = {"bound": True, "satisfied": False, "blocker": "spec-changed"}
        row = w.action_inbox(data)["tasks"]["items"][0]
        self.assertTrue(row["conflicting_projection"])
        self.assertEqual(row["reasons"][0]["detail"], "spec-changed")

    def test_unknown_reason_and_invalid_target(self):
        data = envelope(); data["plan"]["actionable"] = []
        row = w.action_inbox(data)["tasks"]["items"][0]
        self.assertEqual(row["reasons"][0]["code"], "not-actionable-reason-unreported")
        with self.assertRaises(w.WorkViewError):
            w.action_inbox(data, task_id="unknown")

    def test_action_authority_is_preserved_not_executed(self):
        data = envelope(); data["next_actions"][0]["command"] = "echo never-executed"
        row = w.action_inbox(data)["actions"]["items"][0]
        self.assertEqual(row["authority"], "worktree_write")
        self.assertIsNone(row["human_decision_required"])
        self.assertIn("not executed", w.render(w.action_inbox(data)))


class BriefTests(unittest.TestCase):
    def test_initial_brief_does_not_claim_changes_since_missing_baseline(self):
        report = w.return_brief(envelope())
        self.assertEqual(report["baseline"], "absent")
        self.assertNotIn("changes", report)
        self.assertFalse(report["history_complete"])

    def test_state_and_proof_changes_are_both_visible(self):
        before, current = envelope(), envelope()
        current["criteria"][0]["status"] = "stale"
        current["plan"]["tasks"][0]["verify_digest"] = "f" * 64
        current["generated_at"] = "2026-11-10T01:00:00Z"
        current["sources"][0]["sha256"] = "e" * 64
        current["state_digest"] = "f" * 64
        report = w.return_brief(current, before)
        self.assertTrue(report["recorded_scope_matches"])
        self.assertFalse(report["head_changed"])
        self.assertEqual(report["source_changes"]["total"], 1)
        self.assertEqual(report["changes"]["tasks"]["changed"]["items"][0]["changed_fields"], ["verify_digest"])
        self.assertEqual(report["changes"]["criteria"]["changed"]["items"][0]["after"], "stale")

    def test_plan_generation_change_never_links_reused_ids(self):
        before, current = envelope(), envelope()
        current["plan"]["plan_id"] = "plan_" + "2" * 32
        current["plan"]["tasks"][0]["state"] = "done"
        report = w.return_brief(current, before)
        self.assertFalse(report["recorded_scope_matches"])
        self.assertTrue(report["changes"]["tasks"]["transitions_suppressed"])
        self.assertEqual(report["changes"]["tasks"]["changed"]["total"], 0)

    def test_add_remove_are_not_claimed_completions(self):
        before, current = envelope(), envelope()
        current["plan"]["tasks"] = [task("new")]; current["plan"]["actionable"] = ["new"]
        changes = w.return_brief(current, before)["changes"]["tasks"]
        self.assertEqual(changes["removed"]["items"], ["t1"])
        self.assertEqual(changes["added"]["items"], ["new"])
        self.assertEqual(changes["changed"]["total"], 0)

    def test_future_baseline_and_naive_timestamp(self):
        before, current = envelope(), envelope()
        before["generated_at"] = "2026-11-11T00:00:00Z"
        self.assertIn("baseline-in-future", w.return_brief(current, before)["mismatch_reasons"])
        before["generated_at"] = "2026-11-10T00:00:00"
        with self.assertRaises(w.WorkViewError):
            w.return_brief(current, before)

    def test_no_recorded_identity_is_not_same_scope(self):
        data = envelope(); data["plan"] = {"present": False, "tasks": []}
        data["criteria"] = []
        report = w.return_brief(data, data)
        self.assertFalse(report["recorded_scope_matches"])
        self.assertTrue(report["same_snapshot_digest"])

    def test_identical_snapshots_do_not_prove_no_intermediate_events(self):
        report = w.return_brief(envelope(), envelope())
        self.assertTrue(report["same_snapshot_digest"])
        self.assertFalse(report["history_complete"])
        self.assertEqual(report["changes"]["criteria"]["changed"]["total"], 0)

    def test_same_timezone_instant_is_not_future(self):
        before, current = envelope(), envelope()
        before["generated_at"] = "2026-11-10T09:00:00+09:00"
        self.assertNotIn("baseline-in-future", w.return_brief(current, before)["mismatch_reasons"])


class BoundaryTests(unittest.TestCase):
    def test_bad_schema_duplicates_and_types_fail_closed(self):
        mutations = [lambda d: d.update(schema=True), lambda d: d.update(schema=1),
                     lambda d: d.update(state_digest=""), lambda d: d["plan"].update(actionable=["ghost"]),
                     lambda d: d["criteria"].append(deepcopy(d["criteria"][0])),
                     lambda d: d["plan"]["tasks"].append(deepcopy(d["plan"]["tasks"][0])),
                     lambda d: d["criteria"][0].update(status="success"),
                     lambda d: d["next_actions"][0].update(priority=True),
                     lambda d: d["plan"].update(present=False),
                     lambda d: d["plan"]["tasks"][0].update(depends=["x", "x"]),
                     lambda d: d["sources"].append(deepcopy(d["sources"][0]))]
        for mutation in mutations:
            data = envelope(); mutation(data)
            with self.subTest(mutation=mutation), self.assertRaises(w.WorkViewError):
                w.evidence_matrix(data)

    def test_limits_and_nested_caps(self):
        for limit in (False, 0, -1, 101, "20"):
            with self.subTest(limit=limit), self.assertRaises(w.WorkViewError):
                w.return_brief(envelope(), limit=limit)
        data = envelope(); data["criteria"] = [criterion()] * (w.MAX_ROWS + 1)
        with self.assertRaises(w.WorkViewError):
            w.action_inbox(data)

    def test_pure_views_do_not_mutate_input_or_touch_io(self):
        data = envelope(); baseline = deepcopy(data)
        with patch("builtins.open", side_effect=AssertionError("unexpected file read")), \
                patch("subprocess.run", side_effect=AssertionError("unexpected process")):
            for report in (w.evidence_matrix(data), w.action_inbox(data), w.return_brief(data, data)):
                self.assertTrue(w.render(report))
                json.dumps(report, allow_nan=False)
        self.assertEqual(data, baseline)

    def test_display_controls_neutralized_and_unicode_preserved(self):
        data = envelope(); data["plan"]["tasks"][0]["title"] = "한글\x1b[31m\u202e\u2028"
        data["criteria"][0]["text"] = "요건\x00"
        text = w.render(w.return_brief(data))
        self.assertIn("한글", text)
        self.assertIn("요건", text)
        for control in ("\x1b", "\x00", "\u202e", "\u2028"):
            self.assertNotIn(control, text)



if CLI:
    class RuntimeTests(unittest.TestCase):
        def test_canonical_runtime_and_safe_saved_envelope_read(self):
            with tempfile.TemporaryDirectory(prefix="oms-work-view-") as folder:
                root = Path(folder)
                repo = root / "repo"; repo.mkdir()
                env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_CONFIG_")}
                env.update(PYTHONDONTWRITEBYTECODE="1", XDG_CACHE_HOME=str(root / "cache"))
                env.pop("OMS_HARNESS_CHILD", None)
                subprocess.run(["git", "init", "-q", str(repo)], check=True, env=env)
                project = repo / "PROJECT.md"
                project.write_text("# Fixture\n\n## Goal\nTest read-only views.\n\n## Success criteria\n- [id:req1] A result is checked.\n", encoding="utf-8")
                subprocess.run(["git", "-C", str(repo), "add", "PROJECT.md"], check=True, env=env)
                subprocess.run(["git", "-C", str(repo), "-c", "user.name=Fixture", "-c",
                                "user.email=fixture@example.invalid", "commit", "-qm", "fixture"], check=True, env=env)
                def call(*args, child=False):
                    return subprocess.run(["bash", str(ROOT / "scripts/runtime.sh"), "--repo", str(repo), *args],
                                          env=dict(env, **({"OMS_HARNESS_CHILD": "1"} if child else {})),
                                          capture_output=True, text=True, timeout=90)
                original = call("envelope", "show")
                self.assertEqual(original.returncode, 0, original.stderr)
                saved = repo / "before.json"; saved.write_text(original.stdout, encoding="utf-8")
                before = {f.relative_to(repo).as_posix(): f.read_bytes() for f in repo.rglob("*")
                          if f.is_file() and ".git" not in f.relative_to(repo).parts}
                for args in (("evidence", "matrix"), ("next", "--explain"), ("brief", "--before", "before.json")):
                    result = call(*args, child=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(json.loads(result.stdout)["read_only"])
                    text = call(*args, "--format", "text")
                    self.assertEqual(text.returncode, 0, text.stderr)
                    self.assertIn("read-only", text.stdout)
                old_next = call("next")
                self.assertEqual(set(json.loads(old_next.stdout)), {"schema", "state_digest", "actions"})
                self.assertEqual(call("next", "--format", "text").returncode, 2)
                self.assertEqual(call("evidence", "matrix", "--limit", "0").returncode, 2)
                self.assertEqual(call("brief", "--format", "text", "--pretty").returncode, 2)
                after = {f.relative_to(repo).as_posix(): f.read_bytes() for f in repo.rglob("*")
                         if f.is_file() and ".git" not in f.relative_to(repo).parts}
                self.assertEqual(before, after)
                (repo / "linked.json").symlink_to(saved)
                self.assertEqual(call("brief", "--before", "linked.json").returncode, 2)
                (root / "outside.json").write_text(original.stdout, encoding="utf-8")
                self.assertEqual(call("brief", "--before", "../outside.json").returncode, 2)
                project.write_text(project.read_text() + "- [id:req2] Another condition.\n", encoding="utf-8")
                delta = call("brief", "--before", "before.json")
                self.assertEqual(delta.returncode, 0, delta.stderr)
                self.assertGreater(json.loads(delta.stdout)["source_changes"]["total"], 0)

if __name__ == "__main__":
    unittest.main(verbosity=2)
