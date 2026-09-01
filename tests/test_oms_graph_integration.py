"""End-to-end `oms graph exec run` over the real plan-run / patch-land front doors.

Every case drives one temporary repository with a fake `codex` transport on
PATH (the `autonomy-plan-run-smoke.sh` pattern the adapter tests already use).
The regression guards are the point: no lease, scope, admission, review, or
acceptance bypass; no fabricated landing receipt; no `done` from prose.
"""

from __future__ import annotations

import ast
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from oms_graph import events, runner, shadow
from oms_graph.errors import GraphError
from test_oms_graph_adapter import CHECK_SCRIPT, FAKE_CODEX, SCRUBBED, agent_node

HAVE_GIT = shutil.which("git") is not None


def _spec(entry, nodes, edges, **budget):
    graph = {"schema": 1, "id": "integration", "entry": entry, "stop_facts": [],
             "budget": {"max_steps": budget.get("max_steps", 8), "max_repeats": budget.get("max_repeats", 2)},
             "nodes": nodes, "edges": edges}
    return graph


def implement_land_spec(**budget):
    """implement (run t1) -> land (land t1) -> done, with a bounded retry."""
    return _spec("implement", {
        "implement": agent_node("t1", proof=["plan.task.t1.patch_present", "plan.task.t1.state=review"]),
        "land": agent_node("t1", mode="land", proof=["receipt.land.t1.present"]),
        "done": {"kind": "terminal"},
    }, [
        {"from": "implement", "to": "land", "outcomes": ["completed"]},
        {"from": "implement", "to": "implement", "outcomes": ["failed", "unverified"], "kind": "repeat"},
        {"from": "land", "to": "done", "outcomes": ["completed"]},
        {"from": "land", "to": "land", "outcomes": ["partial"], "kind": "repeat"},
    ], **budget)


def gated_spec():
    return _spec("implement", {
        "implement": agent_node("t1", proof=["plan.task.t1.patch_present", "plan.task.t1.state=review"]),
        "review": {"kind": "gate", "authority": "parent", "decisions": ["approved", "changes_requested"]},
        "land": agent_node("t1", mode="land", proof=["receipt.land.t1.present"]),
        "done": {"kind": "terminal"},
    }, [
        {"from": "implement", "to": "review", "outcomes": ["completed"]},
        {"from": "implement", "to": "implement", "outcomes": ["failed", "unverified"], "kind": "repeat"},
        {"from": "review", "to": "land", "outcomes": ["approved"]},
        {"from": "review", "to": "implement", "outcomes": ["changes_requested"]},
        {"from": "land", "to": "done", "outcomes": ["completed"]},
    ])


def two_task_spec(commit_command):
    """Both patches are built before either lands, so the second applies from a moved base."""
    return _spec("implement1", {
        "implement1": agent_node("t1", proof=["plan.task.t1.patch_present"]),
        "implement2": agent_node("t2", proof=["plan.task.t2.patch_present"]),
        "land1": agent_node("t1", mode="land", proof=["receipt.land.t1.present"]),
        "commit1": {"kind": "tool", "effect": "write", "command": commit_command},
        "land2": agent_node("t2", mode="land", proof=["receipt.land.t2.present"]),
        "done": {"kind": "terminal"},
    }, [
        {"from": "implement1", "to": "implement2", "outcomes": ["completed"]},
        {"from": "implement2", "to": "land1", "outcomes": ["completed"]},
        {"from": "land1", "to": "commit1", "outcomes": ["completed"]},
        {"from": "commit1", "to": "land2", "outcomes": ["completed"]},
        {"from": "land2", "to": "done", "outcomes": ["completed"]},
    ], max_steps=12)


class PureMappingTest(unittest.TestCase):
    """No repository: the record-shaping helpers the runner and resume share."""

    def test_transport_verdicts_outrank_a_mis_mapped_adapter_result(self) -> None:
        held = runner.agent_outcome({"exit": 75, "claimed_outcome": "completed", "outcome": "completed"})
        self.assertEqual((held["claimed_outcome"], held["outcome"]), ("partial", "partial"))
        self.assertIn("landing-lock-held", held["detail"])
        idle = runner.agent_outcome({"exit": 3, "claimed_outcome": "failed", "outcome": "failed"})
        self.assertEqual(idle["outcome"], "blocked")

    def test_a_timeout_is_absence_of_evidence_not_a_failure(self) -> None:
        record = runner.agent_outcome({"exit": -1, "reason": "timeout", "claimed_outcome": "failed", "outcome": "failed"})
        self.assertEqual((record["claimed_outcome"], record["outcome"]), ("unverified", "unverified"))

    def test_an_outcome_free_result_never_becomes_a_completion(self) -> None:
        record = runner.agent_outcome({"exit": 0})
        self.assertEqual(record["outcome"], "unverified")
        self.assertIn("adapter-returned-no-outcome", record["detail"])

    def test_proof_missing_is_named_in_the_detail(self) -> None:
        record = runner.agent_outcome({"exit": 0, "outcome": "unverified", "claimed_outcome": "completed",
                                       "proof_missing": ["receipt.land.t1.present"]})
        self.assertIn("receipt.land.t1.present", record["detail"])

    def test_alias_facts_mirrors_the_resolved_task_onto_next(self) -> None:
        spec = {"schema": 1, "id": "g", "entry": "implement",
                "nodes": {"implement": agent_node("next"), "done": {"kind": "terminal"}},
                "edges": [{"from": "implement", "to": "done", "outcomes": ["completed"]}]}
        rows = [{"event": "node_outcome", "node": "implement", "task_id": "t1", "outcome": "completed"}]
        facts = {"plan.task.t1.state": "review", "plan.task.t1.patch_present": True,
                 "receipt.land.t1.present": False, "plan.task.other.state": "ready"}
        aliased = runner.alias_facts(facts, rows, spec)
        self.assertEqual(aliased["plan.task.next.state"], "review")
        self.assertTrue(aliased["plan.task.next.patch_present"])
        self.assertFalse(aliased["receipt.land.next.present"])
        self.assertNotIn("plan.task.next.other", aliased)
        # Without a recorded task id there is nothing to alias, and no guess.
        self.assertNotIn("plan.task.next.state", runner.alias_facts(facts, [], spec))

    def test_the_latest_recorded_task_wins(self) -> None:
        spec = {"schema": 1, "id": "g", "entry": "implement",
                "nodes": {"implement": agent_node("next"), "done": {"kind": "terminal"}},
                "edges": [{"from": "implement", "to": "done", "outcomes": ["completed"]}]}
        rows = [{"event": "node_outcome", "node": "implement", "task_id": "t1"},
                {"event": "node_outcome", "node": "implement", "task_id": "t2"}]
        aliased = runner.alias_facts({"plan.task.t1.state": "done", "plan.task.t2.state": "review"}, rows, spec)
        self.assertEqual(aliased["plan.task.next.state"], "review")


class FrontDoorGuardTest(unittest.TestCase):
    """The runner must reach plan state only through the audited adapter.

    `agent-plan land/finish/claim/review/start` are compare-and-set transitions
    fenced by receipts only `patch-land.sh` can compute, so the runner names no
    control-plane script at all: agent work goes through `adapters.plan`, and
    the one script literal here is the fail-open Work Journal observer.
    """

    SOURCE = Path(runner.__file__).read_text(encoding="utf-8")
    TREE = ast.parse(SOURCE)
    FENCED_SCRIPTS = ("agent-plan.sh", "plan-run.sh", "patch-land.sh", "peer-delegate.sh", "agent-executor.sh")

    def test_no_control_plane_script_is_named(self) -> None:
        scripts = {node.value for node in ast.walk(self.TREE)
                   if isinstance(node, ast.Constant) and isinstance(node.value, str) and node.value.endswith(".sh")}
        self.assertEqual(scripts, {runner.JOURNAL_OBSERVER})
        for name in self.FENCED_SCRIPTS:
            self.assertNotIn(name, self.SOURCE)

    def test_no_source_line_pairs_agent_plan_with_a_fenced_verb(self) -> None:
        seen = 0
        for number, line in enumerate(self.SOURCE.splitlines(), 1):
            if "agent-plan" not in line:
                continue
            seen += 1
            for verb in ("land", "finish", "claim", "review", "start"):
                self.assertNotIn(verb, line, "line %d names agent-plan %s" % (number, verb))
        self.assertTrue(seen, "the runner no longer mentions agent-plan; keep the guard meaningful")

    def test_agent_execution_goes_through_the_adapter(self) -> None:
        calls = {ast.dump(node.func) for node in ast.walk(self.TREE) if isinstance(node, ast.Call)}
        self.assertTrue(any("plan_adapter" in dump and "execute" in dump for dump in calls),
                        "no adapters.plan.execute call site; the guard would be vacuous")


class GraphRepoFixture:
    """One temporary repository per test, driven through the real front doors."""

    TASKS = (("t1", "delegated.txt"),)

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="oms-graph-integration."))
        self.addCleanup(shutil.rmtree, str(self.tmp), True)
        self.repo = self.tmp / "repo"
        self.bin = self.tmp / "bin"
        self.home = self.tmp / "home"
        for path in (self.repo, self.bin, self.home):
            path.mkdir(parents=True)
        (self.repo / "scripts").mkdir()
        self._write(self.repo / "scripts" / "check.sh", CHECK_SCRIPT)
        (self.repo / "README.md").write_text("base\n", encoding="utf-8")
        self._write(self.bin / "codex", FAKE_CODEX)

        self._saved_env = dict(os.environ)
        self.addCleanup(self._restore_env)

        self.git("init", "-q")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "user.name", "Test")
        self.git("add", "README.md", "scripts/check.sh")
        self.git("commit", "-qm", "base")

        self.plan("init", "--goal", "g", "--accept", "bash scripts/check.sh t1")
        for task_id, allowed in self.TASKS:
            self.plan("add", "--id", task_id, "--title", task_id, "--allowed", allowed,
                      "--verify", "bash scripts/check.sh %s" % task_id)

    def _restore_env(self) -> None:
        os.environ.clear()
        os.environ.update(self._saved_env)

    @staticmethod
    def _write(path: Path, body: str) -> None:
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)

    def _env(self) -> dict:
        env = dict(self._saved_env)
        for name in SCRUBBED:
            env.pop(name, None)
        env["HOME"] = str(self.home)
        # peer-delegate loads NVM_DIR before execution; keep discovery hermetic.
        env["NVM_DIR"] = str(self.home / ".nvm")
        env["OMS_LOCK_DIR"] = str(self.tmp / "locks")
        # The journal mirror is best-effort and its store is not under test.
        env["OMS_WORK_JOURNAL"] = "0"
        return env

    def git(self, *args: str) -> None:
        subprocess.run(["git", "-C", str(self.repo)] + list(args), check=True,
                       env=self._env(), stdout=subprocess.DEVNULL)

    def plan(self, *args: str) -> None:
        argv = ["bash", str(ROOT / "scripts" / "agent-plan.sh"), "--repo", str(self.repo)] + list(args)
        subprocess.run(argv, check=True, env=self._env(), cwd=str(self.repo), stdout=subprocess.DEVNULL)

    def task_state(self, task_id: str) -> str:
        argv = ["bash", str(ROOT / "scripts" / "agent-plan.sh"), "--repo", str(self.repo), "show", "--id", task_id]
        out = subprocess.run(argv, env=self._env(), cwd=str(self.repo), capture_output=True, text=True, check=True)
        return str(json.loads(out.stdout).get("state", ""))

    def worker_env(self, **extra) -> None:
        """Swap in the fixture environment; every child inherits this process's."""
        env = self._env()
        env["PATH"] = "%s:/usr/bin:/bin" % self.bin
        env.update(extra)
        os.environ.clear()
        os.environ.update(env)

    def drive(self, call, *args, **kwargs):
        env_extra = kwargs.pop("env_extra", None) or {}
        self.worker_env(**env_extra)
        try:
            return call(*args, **kwargs)
        finally:
            self._restore_env()

    def run_graph(self, spec, **kwargs):
        env_extra = kwargs.pop("env_extra", None)
        return self.drive(runner.run, self.repo, spec, worker="codex", env_extra=env_extra, **kwargs)

    def rows(self, run_id):
        return self.drive(events.read_events, self.repo, run_id)

    def outcomes(self, run_id):
        return [row for row in self.rows(run_id) if row["event"] == "node_outcome"]

    def artifact_kinds(self):
        path = self.repo / ".oms" / "artifacts" / "index.jsonl"
        if not path.is_file():
            return []
        return [json.loads(line).get("kind") for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

    def head_count(self) -> int:
        out = subprocess.run(["git", "-C", str(self.repo), "rev-list", "--count", "HEAD"],
                             env=self._env(), capture_output=True, text=True, check=True)
        return int(out.stdout.strip())


@unittest.skipUnless(HAVE_GIT, "git is required to drive plan-run")
class GraphRunTest(GraphRepoFixture, unittest.TestCase):

    # -- 1 ------------------------------------------------------------------

    def test_run_reaches_terminal_through_plan_run_and_patch_land(self) -> None:
        result = self.run_graph(implement_land_spec())
        self.assertEqual(result["status"], "terminal", result)
        run_id = result["run_id"]

        started = {row["node"] for row in self.rows(run_id) if row["event"] == "node_started"}
        self.assertEqual(started, {"implement", "land"})
        recorded = {row["node"]: row["outcome"] for row in self.outcomes(run_id)}
        self.assertEqual(recorded, {"implement": "completed", "land": "completed"})
        self.assertTrue((events.run_dir(self.repo, run_id) / "projection.json").is_file())
        self.assertTrue((events.run_dir(self.repo, run_id) / "artifacts" / "land-1.txt").is_file())
        finished = [row for row in self.rows(run_id) if row["event"] == "run_finished"]
        self.assertEqual([row["status"] for row in finished], ["terminal"])
        # The landing is real: the file exists and the plan says done.
        self.assertEqual((self.repo / "delegated.txt").read_text(encoding="utf-8"), "one\n")
        self.assertEqual(self.task_state("t1"), "done")
        self.assertIn("patch-land", self.artifact_kinds())

    # -- 2 and 3 ------------------------------------------------------------

    def test_a_claimed_completion_without_a_receipt_lands_nothing(self) -> None:
        spec = _spec("claim", {
            "claim": {"kind": "tool", "effect": "read", "command": "echo completed",
                      "proof": ["receipt.land.t1.present"]},
            "done": {"kind": "terminal"},
        }, [{"from": "claim", "to": "done", "outcomes": ["completed"]}])
        result = self.run_graph(spec)

        self.assertEqual(result["status"], "blocked", result)
        recorded = self.outcomes(result["run_id"])
        self.assertEqual([row["outcome"] for row in recorded], ["unverified"])
        self.assertEqual([row["claimed_outcome"] for row in recorded], ["completed"])
        self.assertEqual([item["node"] for item in result["route"]["downgrades"]], ["claim"])
        # No fabricated receipt, and the plan task never moved.
        self.assertNotIn("patch-land", self.artifact_kinds())
        self.assertEqual(self.task_state("t1"), "ready")
        self.assertFalse((self.repo / "delegated.txt").exists())

    # -- 4 ------------------------------------------------------------------

    def test_a_failed_worker_repeats_once_then_exhausts(self) -> None:
        result = self.run_graph(implement_land_spec(max_repeats=1), env_extra={"FAIL_TASK": "t1"})

        self.assertEqual(result["status"], "exhausted", result)
        recorded = self.outcomes(result["run_id"])
        self.assertEqual([row["outcome"] for row in recorded], ["failed", "failed"])
        self.assertEqual([row["attempt"] for row in recorded], [1, 2])
        # A failed delegation releases the claim: the task is runnable again.
        self.assertEqual(self.task_state("t1"), "ready")
        self.assertNotIn("patch-land", self.artifact_kinds())

    # -- 5 ------------------------------------------------------------------

    def test_resume_reconciles_an_active_node_and_continues(self) -> None:
        spec = implement_land_spec()
        started = self.drive(events.start_run, self.repo, spec)
        run_id = started["run_id"]
        self.drive(events.append_event, self.repo, run_id, "node_started",
                   node="implement", attempt=1, idempotency_key="start:implement:1")

        result = self.drive(runner.resume, self.repo, run_id, worker="codex")

        self.assertEqual(result["status"], "terminal", result)
        reconciled = [row for row in self.outcomes(run_id) if row["node"] == "implement" and row["attempt"] == 1]
        self.assertEqual(len(reconciled), 1, reconciled)
        self.assertEqual(reconciled[0]["actor"], {"kind": "runner", "name": "resume"})
        # Nothing ran, so the reconciliation reads the plan, not a claim.
        self.assertEqual(reconciled[0]["outcome"], "failed")
        self.assertEqual(self.task_state("t1"), "done")

    # -- 6 ------------------------------------------------------------------

    def test_a_gate_stops_the_run_until_the_parent_decides(self) -> None:
        result = self.run_graph(gated_spec())
        run_id = result["run_id"]
        self.assertEqual((result["status"], result["primary"]), ("gate", "review"))
        self.assertEqual([row["event"] for row in self.rows(run_id) if row["event"] == "run_finished"], [])

        with self.assertRaises(GraphError):
            self.drive(runner.decide, self.repo, run_id, "implement", "approved")
        with self.assertRaises(GraphError):
            self.drive(runner.decide, self.repo, run_id, "review", "shipped")

        decision = self.drive(runner.decide, self.repo, run_id, "review", "approved", note="parent reviewed")
        self.assertEqual(decision["event"]["actor"], {"kind": "parent", "name": "decide"})
        self.assertEqual(decision["route"]["primary"], "land")

        resumed = self.drive(runner.resume, self.repo, run_id, worker="codex")
        self.assertEqual(resumed["status"], "terminal", resumed)
        self.assertEqual(self.task_state("t1"), "done")

    # -- 8 ------------------------------------------------------------------

    def test_a_cacheable_read_replays_only_a_proved_completion(self) -> None:
        cache = self.repo / ".oms" / "graph" / "cache"

        def cacheable(command, proof):
            return _spec("inspect", {
                "inspect": {"kind": "tool", "effect": "read", "command": command,
                            "cacheable": True, "proof": proof},
                "done": {"kind": "terminal"},
            }, [{"from": "inspect", "to": "done", "outcomes": ["completed"]}])

        first = self.run_graph(cacheable("echo one", []))
        self.assertEqual(first["status"], "terminal", first)
        self.assertEqual([row["cached"] for row in self.outcomes(first["run_id"])], [False])
        self.assertEqual(len(list(cache.glob("*.json"))), 1)

        # Same node, same head, no upstream change: the second run replays it.
        second = self.run_graph(cacheable("echo one", []))
        self.assertEqual(second["status"], "terminal", second)
        replayed = self.outcomes(second["run_id"])[0]
        self.assertTrue(replayed["cached"])
        self.assertEqual(replayed["detail"], "cache-hit")
        self.assertEqual(replayed["outcome"], "completed")
        self.assertEqual(len(list(cache.glob("*.json"))), 1)

        # An unproved claim is never stored: a retry has to observe reality.
        downgraded = self.run_graph(cacheable("echo two", ["receipt.land.t1.present"]))
        self.assertEqual(downgraded["status"], "blocked", downgraded)
        self.assertEqual(self.outcomes(downgraded["run_id"])[0]["outcome"], "unverified")
        self.assertEqual(len(list(cache.glob("*.json"))), 1)

    # -- 9 ------------------------------------------------------------------

    def test_shadow_records_one_comparison_row(self) -> None:
        ledger = self.repo / ".oms" / "graph" / "shadow.jsonl"
        row = self.drive(shadow.shadow, self.repo)
        self.assertIsInstance(row["agree"], bool)
        self.assertEqual(row["kind"], "graph-route-shadow")
        self.assertEqual(row["spec_id"], "goal-drive")
        self.assertIn(row["control_plane"]["mapped"], set(shadow.ACTION_ROUTES.values()) | {""})
        lines = [line for line in ledger.read_text(encoding="utf-8").splitlines() if line.strip()]
        self.assertEqual(len(lines), 1)
        self.assertEqual(json.loads(lines[0])["kind"], "graph-route-shadow")


@unittest.skipUnless(HAVE_GIT, "git is required to drive plan-run")
class SequentialLandingTest(GraphRepoFixture, unittest.TestCase):
    """Two patches built from the same base, landed either side of a commit."""

    TASKS = (("t1", "delegated.txt"), ("t2", "delegated2.txt"))

    def test_two_patches_land_sequentially_from_different_bases(self) -> None:
        before = self.head_count()
        spec = two_task_spec("git add -A && git -c commit.gpgsign=false commit -qm land1")
        result = self.run_graph(spec)

        self.assertEqual(result["status"], "terminal", result)
        recorded = {row["node"]: row["outcome"] for row in self.outcomes(result["run_id"])}
        self.assertEqual(recorded, {"implement1": "completed", "implement2": "completed",
                                    "land1": "completed", "commit1": "completed", "land2": "completed"})
        self.assertEqual((self.repo / "delegated.txt").read_text(encoding="utf-8"), "one\n")
        self.assertEqual((self.repo / "delegated2.txt").read_text(encoding="utf-8"), "two\n")
        self.assertEqual(self.head_count(), before + 1)
        self.assertEqual(self.task_state("t1"), "done")
        self.assertEqual(self.task_state("t2"), "done")


if __name__ == "__main__":
    unittest.main()
