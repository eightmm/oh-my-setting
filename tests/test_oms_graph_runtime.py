"""Graph Runtime v2 over the real front doors: task binding, selection races,
concurrent fan-out, workspace-aware caching, and crash reconciliation.

The two correctness proofs this phase exists for are recorded as evidence in
the test output itself (see `GoalDriveBindingTest` and `FanOutTest`):

  A. implement selected t1, the plan's `next` became t2, land still landed t1
  B. two disjoint workers passed a concurrency barrier under --jobs 2
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from oms_graph import capabilities, events, runner, commit as exec_commit
from oms_graph.adapters import plan as plan_adapter
from oms_graph.errors import GraphError
from oms_graph.spec import load_spec
from test_oms_graph_adapter import FAKE_CODEX, agent_node
from test_oms_graph_integration import GraphRepoFixture, _spec

HAVE_GIT = shutil.which("git") is not None

# `oms agent-plan accept` / `oms graph exec commit` inside a tool node must reach
# the source checkout under test, never an installed copy.
OMS_SHIM = """#!/usr/bin/env bash
set -euo pipefail
verb="${1:-}"; shift || true
case "$verb" in
  agent-plan) exec bash "%(root)s/scripts/agent-plan.sh" --repo "$PWD" "$@" ;;
  graph) exec bash "%(root)s/scripts/graph.sh" --repo "$PWD" "$@" ;;
  *) echo "oms shim: unsupported verb $verb" >&2; exit 64 ;;
esac
"""

ACCEPT_ALL = """#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  t1) grep -Fxq one delegated.txt ;;
  t2) grep -Fxq two delegated2.txt ;;
  all) grep -Fxq one delegated.txt && grep -Fxq two delegated2.txt ;;
  *) exit 2 ;;
esac
"""

# A worker that proves overlap with a two-phase rendezvous: mark started,
# wait for the sibling's start, mark seen, wait for the sibling's seen. Both
# marks require the sibling to be alive at the same time, so a sequential
# runner can never satisfy the exchange (a stale start mark from a finished
# sibling is not enough); a concurrent one satisfies both.
BARRIER_CODEX = """#!/usr/bin/env bash
case "${1:-}" in
  --version) printf 'codex 1.0\\n'; exit 0 ;;
  exec) if [ "${2:-}" = "--help" ]; then printf 'usage: codex exec\\n'; exit 0; fi ;;
esac
prompt="$(cat)"
dir="${BARRIER_DIR:?}"
me="${OMS_TASK_ID:?}"
case "$me" in t1) other=t2 ;; *) other=t1 ;; esac
wait_for() {
  deadline=$(( $(date +%s) + ${BARRIER_WAIT:-6} ))
  while [ ! -f "$dir/$1" ]; do
    if [ "$(date +%s)" -ge "$deadline" ]; then echo "barrier timeout waiting for $1" >&2; exit 9; fi
    sleep 0.2
  done
}
: > "$dir/$me.started"
wait_for "$other.started"
: > "$dir/$me.seen"
wait_for "$other.seen"
case "$me" in
  t2) printf 'two\\n' > delegated2.txt ;;
  *) printf 'one\\n' > delegated.txt ;;
esac
echo worker-ok
"""


class RuntimeFixture(GraphRepoFixture):
    """Two disjoint tasks, an acceptance that needs both, and an `oms` shim."""

    TASKS = (("t1", "delegated.txt"), ("t2", "delegated2.txt"))
    ACCEPT = "bash scripts/check.sh all"
    CODEX = FAKE_CODEX

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="oms-graph-runtime."))
        self.addCleanup(shutil.rmtree, str(self.tmp), True)
        self.repo = self.tmp / "repo"
        self.bin = self.tmp / "bin"
        self.home = self.tmp / "home"
        for path in (self.repo, self.bin, self.home):
            path.mkdir(parents=True)
        (self.repo / "scripts").mkdir()
        self._write(self.repo / "scripts" / "check.sh", ACCEPT_ALL)
        (self.repo / "README.md").write_text("base\n", encoding="utf-8")
        self._write(self.bin / "codex", self.CODEX)
        self._write(self.bin / "oms", OMS_SHIM % {"root": ROOT})

        self._saved_env = dict(os.environ)
        self.addCleanup(self._restore_env)

        self.git("init", "-q")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "user.name", "Test")
        self.git("add", "README.md", "scripts/check.sh")
        self.git("commit", "-qm", "base")

        self.plan("init", "--goal", "g", "--accept", self.ACCEPT)
        for task_id, allowed in self.TASKS:
            self.plan("add", "--id", task_id, "--title", "task %s" % task_id, "--allowed", allowed,
                      "--verify", "bash scripts/check.sh %s" % task_id)

    def plan_json(self, *args: str):
        argv = ["bash", str(ROOT / "scripts" / "agent-plan.sh"), "--repo", str(self.repo)] + list(args)
        out = subprocess.run(argv, env=self._env(), cwd=str(self.repo), capture_output=True, text=True)
        return out.returncode, (json.loads(out.stdout) if out.stdout.strip().startswith("{") else out.stdout)

    def next_task_id(self) -> str:
        code, view = self.plan_json("next", "--json")
        return str(view.get("id", "")) if code == 0 and isinstance(view, dict) else ""

    def started(self, run_id, node=None):
        rows = [row for row in self.rows(run_id) if row["event"] == "node_started"]
        return [row for row in rows if node is None or row["node"] == node]

    def bindings(self, run_id):
        return self.drive(lambda: events.project(events.read_events(self.repo, run_id), events.load_run_spec(self.repo, run_id))["bindings"])

    def resume_graph(self, run_id, **kwargs):
        env_extra = kwargs.pop("env_extra", None)
        return self.drive(runner.resume, self.repo, run_id, worker="codex", env_extra=env_extra, **kwargs)


def bound_spec(**budget):
    """implement (next -> work_item) -> land (from work_item) -> done; no repeats."""
    return _spec("implement", {
        "implement": {"kind": "agent", "effect": "write", "plan_task": "next", "bind_task": "work_item", "mode": "run",
                      "proof": ["binding.work_item.patch_present", "binding.work_item.state=review"]},
        "land": {"kind": "agent", "effect": "write", "plan_task_from": "work_item", "mode": "land",
                 "proof": ["binding.work_item.receipt.land.present"]},
        "done": {"kind": "terminal"},
    }, [
        {"from": "implement", "to": "land", "outcomes": ["completed"]},
        {"from": "land", "to": "done", "outcomes": ["completed"]},
    ], **budget)


def fanout_spec(**extra):
    """start (router) => a (t1) + b (t2) in one wave, then join, then done."""
    nodes = {
        "start": {"kind": "router"},
        "a": agent_node("t1", proof=["plan.task.t1.patch_present"]),
        "b": agent_node("t2", proof=["plan.task.t2.patch_present"]),
        "join": {"kind": "tool", "effect": "read", "command": "true", "join": "all"},
        "done": {"kind": "terminal"},
    }
    nodes.update(extra)
    return _spec("start", nodes, [
        {"from": "start", "to": "a", "outcomes": ["completed"], "fanout": True},
        {"from": "start", "to": "b", "outcomes": ["completed"], "fanout": True},
        {"from": "a", "to": "join", "outcomes": ["completed"]},
        {"from": "b", "to": "join", "outcomes": ["completed"]},
        {"from": "join", "to": "done", "outcomes": ["completed"]},
    ], max_steps=10)


@unittest.skipUnless(HAVE_GIT, "git is required to drive plan-run")
class GoalDriveBindingTest(RuntimeFixture, unittest.TestCase):
    """P0: the bundled goal-drive on a real two-task plan."""

    def test_next_becoming_t2_cannot_redirect_the_landing_of_t1(self) -> None:
        # Phase 1: acceptance fails, implement resolves `next` to t1 and binds it.
        first = self.run_graph("goal-drive", max_steps=2)
        run_id = first["run_id"]
        self.assertEqual(first["status"], "exhausted", first)
        implement_1 = self.started(run_id, "implement")[0]
        self.assertEqual((implement_1["task_id"], implement_1["binding"]), ("t1", "work_item"))
        self.assertEqual(self.bindings(run_id)["work_item"]["task_id"], "t1")
        self.assertEqual(self.task_state("t1"), "review")
        self.assertEqual(self.task_state("t2"), "ready")
        # The plan's own `next` has moved on: t1 is in review, so t2 is what
        # `plan-run --next` would claim now.
        self.assertEqual(self.next_task_id(), "t2")
        print("\nEVIDENCE-A: implement#1 selected t1 (binding work_item=t1); plan next is now t2; "
              "resuming so land must use the binding, not next")

        # Phase 2: resume. Land must land t1 — the binding — not t2.
        second = self.resume_graph(run_id)
        self.assertEqual(second["status"], "terminal", second)
        self.assertEqual(second["primary"], "done")
        land_rows = self.started(run_id, "land")
        self.assertEqual([row["task_id"] for row in land_rows], ["t1", "t2"])
        implement_rows = self.started(run_id, "implement")
        self.assertEqual([row["task_id"] for row in implement_rows], ["t1", "t2"])
        self.assertEqual([row["binding"] for row in implement_rows], ["work_item", "work_item"])
        outcomes = {(row["node"], row["attempt"]): row for row in self.outcomes(run_id)}
        self.assertEqual(outcomes[("land", 1)]["task_id"], "t1")
        self.assertEqual(outcomes[("land", 1)]["outcome"], "completed")
        self.assertEqual(outcomes[("land", 2)]["task_id"], "t2")
        # Landing order in the artifact index is t1 then t2.
        landed = [json.loads(line) for line in (self.repo / ".oms" / "artifacts" / "index.jsonl").read_text(encoding="utf-8").splitlines() if line.strip()]
        landed = [row["task_id"] for row in landed if row.get("kind") == "patch-land" and row.get("exit") in (0, "0")]
        self.assertEqual(landed, ["t1", "t2"])
        # The binding was rewritten by the second implement attempt and survives replay.
        final = self.bindings(run_id)["work_item"]
        self.assertEqual((final["task_id"], final["node"], final["attempt"]), ("t2", "implement", 2))
        self.assertEqual(second["bindings"]["work_item"]["task_id"], "t2")
        self.assertEqual(self.task_state("t1"), "done")
        self.assertEqual(self.task_state("t2"), "done")
        # Each landing was committed exactly: two commits, a clean tree, both files present.
        self.assertEqual(self.head_count(), 3)
        self.assertEqual(subprocess.run(["git", "-C", str(self.repo), "status", "--porcelain"], env=self._env(),
                                        capture_output=True, text=True, check=True).stdout, "")
        self.assertEqual((self.repo / "delegated.txt").read_text(encoding="utf-8"), "one\n")
        self.assertEqual((self.repo / "delegated2.txt").read_text(encoding="utf-8"), "two\n")
        commits = [row for row in self.outcomes(run_id) if row["node"] == "commit"]
        self.assertEqual([row["outcome"] for row in commits], ["completed", "completed"])
        print("EVIDENCE-A: land#1 task_id=%s land#2 task_id=%s; patch-land order=%s; final binding=%s"
              % (outcomes[("land", 1)]["task_id"], outcomes[("land", 2)]["task_id"], landed, final["task_id"]))

    def test_the_status_surface_shows_the_binding(self) -> None:
        first = self.run_graph("goal-drive", max_steps=2)
        argv = ["bash", str(ROOT / "scripts" / "graph.sh"), "--repo", str(self.repo), "exec", "status", "--run", first["run_id"]]
        out = subprocess.run(argv, env=self._env(), capture_output=True, text=True, check=True).stdout
        self.assertIn("bindings:", out)
        self.assertIn("work_item -> t1 (bound by implement#1)", out)
        self.assertIn("implement (agent) [completed] [work_item=t1]", out)
        self.assertIn("land (agent) [task=work_item→t1]", out)
        payload = json.loads(subprocess.run(argv + ["--json"], env=self._env(), capture_output=True, text=True, check=True).stdout)
        self.assertEqual(payload["projection"]["bindings"]["work_item"]["task_id"], "t1")

    def test_a_selector_with_no_actionable_task_is_a_recorded_verdict(self) -> None:
        self.plan("block", "--id", "t1", "--reason", "parked")
        self.plan("block", "--id", "t2", "--reason", "parked")
        result = self.run_graph("goal-drive")
        self.assertEqual((result["status"], result["primary"]), ("terminal", "parked"), result)
        implement = [row for row in self.outcomes(result["run_id"]) if row["node"] == "implement"]
        self.assertEqual([row["outcome"] for row in implement], ["blocked"])
        self.assertEqual(implement[0]["detail"], "no-actionable-task")
        self.assertEqual(self.bindings(result["run_id"]), {})


@unittest.skipUnless(HAVE_GIT, "git is required to drive plan-run")
class SelectionRaceTest(RuntimeFixture, unittest.TestCase):

    def test_a_task_claimed_between_peek_and_execution_is_never_silently_replaced(self) -> None:
        real_peek = plan_adapter.peek_next_task
        fixture = self

        def peek_then_lose_the_race(repo):
            view = real_peek(repo)
            # Another session (a different registered transport) claims t1
            # right after the graph chose it.
            fixture.plan("claim", "--id", "t1", "--provider", "claude")
            return view

        with mock.patch.object(plan_adapter, "peek_next_task", peek_then_lose_the_race):
            result = self.run_graph(bound_spec())
        run_id = result["run_id"]
        started = self.started(run_id)
        self.assertEqual([(row["node"], row["task_id"]) for row in started], [("implement", "t1")])
        implement = self.outcomes(run_id)[0]
        self.assertEqual(implement["task_id"], "t1")
        self.assertIn(implement["outcome"], ("unverified", "failed"))
        self.assertEqual(result["status"], "blocked", result)
        # No substitution: t2 was never touched and t1 still belongs to the other session.
        self.assertEqual(self.task_state("t2"), "ready")
        code, view = self.plan_json("show", "--id", "t1")
        self.assertEqual((view["state"], view["provider"]), ("claimed", "claude"))
        self.assertEqual(self.bindings(run_id)["work_item"]["task_id"], "t1")


@unittest.skipUnless(HAVE_GIT, "git is required to drive plan-run")
class FanOutTest(RuntimeFixture, unittest.TestCase):

    CODEX = BARRIER_CODEX

    def setUp(self) -> None:
        super().setUp()
        self.barrier = self.tmp / "barrier"
        self.barrier.mkdir()

    def test_jobs_2_runs_two_disjoint_workers_at_the_same_time(self) -> None:
        # The window only has to outlast the second plan-run's own start-up
        # (claim, worktree, provider launch) under gate load; the proof is the
        # exchange itself, not the elapsed time.
        result = self.run_graph(fanout_spec(), jobs=2, env_extra={"BARRIER_DIR": str(self.barrier), "BARRIER_WAIT": "120"})
        self.assertEqual(result["status"], "terminal", result)
        run_id = result["run_id"]
        recorded = {row["node"]: row["outcome"] for row in self.outcomes(run_id)}
        self.assertEqual(recorded, {"a": "completed", "b": "completed", "join": "completed"})
        wave = {row["node"]: (row["wave_id"], row["wave_index"]) for row in self.started(run_id) if row["node"] in ("a", "b")}
        self.assertEqual(wave["a"][0], wave["b"][0])
        self.assertEqual(sorted(index for _, index in wave.values()), [0, 1])
        self.assertTrue((self.barrier / "t1.started").exists() and (self.barrier / "t2.started").exists())
        self.assertEqual(self.task_state("t1"), "review")
        self.assertEqual(self.task_state("t2"), "review")
        print("\nEVIDENCE-B: --jobs 2 wave %s ran a(t1) and b(t2) together; both passed the barrier: %s"
              % (wave["a"][0], recorded))

    def test_jobs_1_cannot_satisfy_the_barrier(self) -> None:
        # Sequential: each worker waits alone for a sibling that is not
        # running yet, so neither passes; the two attempts are separate waves.
        result = self.run_graph(fanout_spec(), jobs=1, env_extra={"BARRIER_DIR": str(self.barrier), "BARRIER_WAIT": "3"})
        self.assertNotEqual(result["status"], "terminal")
        recorded = {row["node"]: row["outcome"] for row in self.outcomes(result["run_id"])}
        self.assertEqual(recorded, {"a": "failed", "b": "failed"})
        waves = [row["wave_id"] for row in self.started(result["run_id"])]
        self.assertEqual(len(set(waves)), 2, waves)
        self.assertEqual(self.task_state("t1"), "ready")
        self.assertEqual(self.task_state("t2"), "ready")

    def test_the_same_task_and_a_land_never_share_a_wave(self) -> None:
        spec = fanout_spec(b=agent_node("t1", proof=["plan.task.t1.patch_present"]))
        dry = self.drive(runner.run, self.repo, spec, worker="codex", jobs=2, dry_run=True)
        self.assertEqual(dry["eligible"], ["a"])
        self.assertEqual(dry["conflicts"], [{"node": "b", "with": "a", "reason": "same-task"}])
        landing = fanout_spec(b=agent_node("t2", mode="land", proof=["receipt.land.t2.present"]))
        dry = self.drive(runner.run, self.repo, landing, worker="codex", jobs=2, dry_run=True)
        self.assertEqual(dry["eligible"], ["a"])
        self.assertEqual(dry["deferred"], [{"node": "b", "reason": "exclusive"}])

    def test_two_selectors_resolve_once_and_run_one_at_a_time(self) -> None:
        spec = fanout_spec(a={"kind": "agent", "effect": "write", "plan_task": "next", "mode": "run"},
                           b={"kind": "agent", "effect": "write", "plan_task": "next", "mode": "run"})
        dry = self.drive(runner.run, self.repo, spec, worker="codex", jobs=2, dry_run=True)
        self.assertEqual(dry["resolved_tasks"], {"a": "t1", "b": "t1"})
        self.assertEqual(dry["eligible"], ["a"])
        self.assertEqual(dry["conflicts"], [{"node": "b", "with": "a", "reason": "same-task"}])


@unittest.skipUnless(HAVE_GIT, "git is required to drive plan-run")
class WorkspaceCacheTest(RuntimeFixture, unittest.TestCase):

    def cacheable(self):
        return _spec("inspect", {
            "inspect": {"kind": "tool", "effect": "read", "command": "echo one", "cacheable": True},
            "done": {"kind": "terminal"},
        }, [{"from": "inspect", "to": "done", "outcomes": ["completed"]}])

    def cached(self, **env_extra) -> bool:
        result = self.run_graph(self.cacheable(), env_extra=env_extra or None)
        self.assertEqual(result["status"], "terminal", result)
        return bool(self.outcomes(result["run_id"])[0]["cached"])

    def test_uncommitted_work_invalidates_the_read_cache(self) -> None:
        self.assertFalse(self.cached())
        self.assertTrue(self.cached())
        (self.repo / "README.md").write_text("edited\n", encoding="utf-8")
        self.assertFalse(self.cached())          # tracked, unstaged
        self.assertTrue(self.cached())
        self.git("add", "README.md")
        self.assertFalse(self.cached())          # staged, same bytes
        (self.repo / "notes.txt").write_text("new\n", encoding="utf-8")
        self.assertFalse(self.cached())          # untracked, non-ignored
        self.assertTrue(self.cached())           # unchanged workspace
        (self.repo / "notes.txt").unlink()
        self.assertTrue(self.cached())           # back to the staged state seen before: content-addressed
        (self.repo / "notes.txt").write_text("other\n", encoding="utf-8")
        self.assertFalse(self.cached())          # a new state again

    def test_an_unsafe_workspace_disables_the_cache_instead_of_falling_back_to_head(self) -> None:
        cache = self.repo / ".oms" / "graph" / "cache"
        os.symlink("README.md", str(self.repo / "link.md"))
        self.assertFalse(self.cached())
        self.assertFalse(self.cached())
        self.assertEqual(list(cache.glob("*.json")), [])


@unittest.skipUnless(HAVE_GIT, "git is required to drive plan-run")
class ResumeReconciliationTest(RuntimeFixture, unittest.TestCase):

    def spec(self):
        return _spec("implement", {
            "implement": agent_node("t1", proof=["plan.task.t1.patch_present", "plan.task.t1.state=review"]),
            "land": agent_node("t1", mode="land", proof=["receipt.land.t1.present"]),
            "done": {"kind": "terminal"},
        }, [
            {"from": "implement", "to": "land", "outcomes": ["completed"]},
            {"from": "land", "to": "done", "outcomes": ["completed"]},
        ])

    def crashed_after(self, node, task_id="t1", spec=None, finished=()):
        started = self.drive(events.start_run, self.repo, spec or self.spec())
        run_id = started["run_id"]
        for done_node in finished:
            self.drive(events.append_event, self.repo, run_id, "node_started", node=done_node, attempt=1,
                       idempotency_key="start:%s:1" % done_node, task_id=task_id)
            self.drive(events.append_event, self.repo, run_id, "node_outcome", node=done_node, attempt=1,
                       idempotency_key="outcome:%s:1" % done_node, task_id=task_id,
                       claimed_outcome="completed", outcome="completed")
        self.drive(events.append_event, self.repo, run_id, "node_started", node=node, attempt=1,
                   idempotency_key="start:%s:1" % node, task_id=task_id)
        return run_id

    def reconciled(self, run_id, node):
        rows = [row for row in self.outcomes(run_id) if row["node"] == node and row["attempt"] == 1]
        return rows[0] if rows else None

    def test_review_with_a_patch_reconstructs_completed(self) -> None:
        self.drive(plan_adapter.execute, self.repo, agent_node("t1"), provider="codex", timeout=600)
        self.assertEqual(self.task_state("t1"), "review")
        run_id = self.crashed_after("implement")
        result = self.resume_graph(run_id)
        row = self.reconciled(run_id, "implement")
        self.assertEqual((row["outcome"], row["task_id"], row["actor"]["name"]), ("completed", "t1", "resume"))
        self.assertEqual(result["status"], "terminal", result)

    def test_done_with_a_landing_receipt_reconstructs_completed(self) -> None:
        self.drive(plan_adapter.execute, self.repo, agent_node("t1"), provider="codex", timeout=600)
        self.drive(plan_adapter.execute, self.repo, agent_node("t1", mode="land"), provider="codex", timeout=600)
        self.assertEqual(self.task_state("t1"), "done")
        run_id = self.crashed_after("land", finished=("implement",))
        result = self.resume_graph(run_id)
        self.assertEqual(self.reconciled(run_id, "land")["outcome"], "completed")
        self.assertEqual(result["status"], "terminal", result)

    def test_blocked_reconciles_to_blocked(self) -> None:
        self.plan("block", "--id", "t1", "--reason", "parked")
        run_id = self.crashed_after("implement")
        result = self.resume_graph(run_id)
        self.assertEqual(self.reconciled(run_id, "implement")["outcome"], "blocked")
        self.assertEqual(result["status"], "blocked")

    def test_a_live_claim_keeps_the_node_active_and_the_run_waiting(self) -> None:
        self.plan("claim", "--id", "t1", "--provider", "codex")
        run_id = self.crashed_after("implement")
        result = self.resume_graph(run_id)
        self.assertIsNone(self.reconciled(run_id, "implement"))
        self.assertEqual((result["status"], result["primary"]), ("waiting", "implement"), result)
        self.assertEqual(self.task_state("t1"), "claimed")

    def test_an_expired_claim_reconciles_to_unverified_for_the_recovery_route(self) -> None:
        self.plan("claim", "--id", "t1", "--provider", "codex", "--ttl", "1")
        time.sleep(2)
        run_id = self.crashed_after("implement")
        result = self.resume_graph(run_id)
        row = self.reconciled(run_id, "implement")
        self.assertEqual(row["outcome"], "unverified")
        self.assertIn("claim-expired", row["detail"])
        # The graph released nothing: the lease is still the plan's to reclaim.
        code, view = self.plan_json("show", "--id", "t1")
        self.assertEqual(view["state"], "claimed")
        self.assertEqual(result["status"], "blocked")

    def test_a_dead_write_tool_is_blocked_not_rerun(self) -> None:
        spec = _spec("publish", {
            "publish": {"kind": "tool", "effect": "write", "command": "touch published"},
            "done": {"kind": "terminal"},
        }, [{"from": "publish", "to": "done", "outcomes": ["completed"]}])
        started = self.drive(events.start_run, self.repo, spec)
        run_id = started["run_id"]
        self.drive(events.append_event, self.repo, run_id, "node_started", node="publish", attempt=1, idempotency_key="start:publish:1")
        result = self.resume_graph(run_id)
        self.assertEqual(self.reconciled(run_id, "publish")["outcome"], "blocked")
        self.assertEqual(self.reconciled(run_id, "publish")["detail"], "resumed-write-tool-uncertain")
        self.assertFalse((self.repo / "published").exists())
        self.assertEqual(result["status"], "blocked")

    def test_a_bound_selector_keeps_its_binding_across_resume(self) -> None:
        self.drive(plan_adapter.execute, self.repo, agent_node("t1"), provider="codex", timeout=600)
        started = self.drive(events.start_run, self.repo, bound_spec())
        run_id = started["run_id"]
        self.drive(events.append_event, self.repo, run_id, "node_started", node="implement", attempt=1,
                   idempotency_key="start:implement:1", task_id="t1", binding="work_item")
        # t2 is what `next` offers now; the binding must still say t1.
        self.assertEqual(self.next_task_id(), "t2")
        result = self.resume_graph(run_id)
        self.assertEqual(result["status"], "terminal", result)
        self.assertEqual([row["task_id"] for row in self.started(run_id, "land")], ["t1"])
        self.assertEqual(self.task_state("t1"), "done")
        self.assertEqual(self.task_state("t2"), "ready")


@unittest.skipUnless(HAVE_GIT, "git is required to drive plan-run")
class ExactCommitTest(RuntimeFixture, unittest.TestCase):

    def test_commit_stages_only_the_landed_patch_and_refuses_strangers(self) -> None:
        first = self.run_graph(bound_spec())
        self.assertEqual(first["status"], "terminal", first)
        run_id = first["run_id"]
        (self.repo / "stray.txt").write_text("stray\n", encoding="utf-8")
        with self.assertRaises(GraphError):
            self.drive(exec_commit.commit_bound, self.repo, binding="work_item", run_id=run_id)
        self.assertEqual(self.head_count(), 1)
        (self.repo / "stray.txt").unlink()
        # A path whitelist alone cannot prove the bytes are the landed patch.
        (self.repo / "delegated.txt").write_text("one\nextra\n", encoding="utf-8")
        with self.assertRaises(GraphError):
            self.drive(exec_commit.commit_bound, self.repo, binding="work_item", run_id=run_id)
        (self.repo / "delegated.txt").write_text("one\n", encoding="utf-8")
        (self.repo / "README.md").write_text("other session\n", encoding="utf-8")
        self.git("add", "README.md")
        staged = (self.repo / ".git/index").read_bytes()
        with self.assertRaises(GraphError):
            self.drive(exec_commit.commit_bound, self.repo, binding="work_item", run_id=run_id)
        self.assertEqual((self.repo / ".git/index").read_bytes(), staged)
        self.git("reset", "-q", "HEAD", "--", "README.md")
        (self.repo / "README.md").write_text("base\n", encoding="utf-8")
        # Neither post-commit hooks nor another pending task gain authority.
        self._write(self.repo / ".git/hooks/post-commit", "#!/bin/sh\ntouch hook-ran\n")
        result = self.drive(exec_commit.commit_bound, self.repo, binding="work_item", run_id=run_id)
        self.assertEqual((result["status"], result["task_id"], result["paths"]), ("committed", "t1", ["delegated.txt"]))
        self.assertEqual(self.head_count(), 2)
        self.assertFalse((self.repo / "hook-ran").exists())
        self.assertEqual(self.task_state("t2"), "ready")
        again = self.drive(exec_commit.commit_bound, self.repo, binding="work_item", run_id=run_id)
        self.assertEqual(again["status"], "clean")
        with self.assertRaises(GraphError):
            self.drive(exec_commit.commit_bound, self.repo, binding="nobody", run_id=run_id)

    def test_commit_refuses_a_task_that_did_not_land(self) -> None:
        started = self.drive(events.start_run, self.repo, bound_spec())
        run_id = started["run_id"]
        self.drive(events.append_event, self.repo, run_id, "node_started", node="implement", attempt=1,
                   idempotency_key="start:implement:1", task_id="t1", binding="work_item")
        with self.assertRaises(GraphError):
            self.drive(exec_commit.commit_bound, self.repo, binding="work_item", run_id=run_id)
        self.assertEqual(exec_commit.patch_paths("diff --git a/x.txt b/x.txt\n--- a/x.txt\n+++ b/x.txt\n"), ["x.txt"])
        with self.assertRaises(GraphError):
            exec_commit.patch_paths('diff --git "a/we ird" "b/we ird"\n')

    def test_exact_commit_recovers_after_ref_publication_without_another_commit(self) -> None:
        first = self.run_graph(bound_spec())
        run_id = first["run_id"]
        with self.assertRaises(GraphError):
            self.drive(exec_commit.commit_bound, self.repo, binding="work_item", run_id=run_id,
                       env_extra={"OMS_GOAL_DRIVE_TEST_STOP_AFTER_REF": "1"})
        self.assertEqual(self.head_count(), 2)
        result = self.drive(exec_commit.commit_bound, self.repo, binding="work_item", run_id=run_id)
        self.assertEqual(result["status"], "clean")
        self.assertEqual(self.head_count(), 2)
        self.assertEqual(subprocess.check_output(["git", "-C", str(self.repo), "status", "--porcelain"]), b"")
        intents = [json.loads(line) for line in (self.repo / ".oms/plan/progress.jsonl").read_text().splitlines()
                   if json.loads(line).get("kind") == "commit-intent"]
        self.assertEqual(intents[-1]["phase"], "committed")
        self.assertEqual(self.task_state("t2"), "ready")


PROMPT_DUMP_CODEX = FAKE_CODEX.replace('prompt="$(cat)"\n', 'prompt="$(cat)"\n[ -z "${PROMPT_DUMP:-}" ] || printf \'%s\' "$prompt" > "$PROMPT_DUMP"\n')


@unittest.skipUnless(HAVE_GIT, "git is required to drive plan-run")
class ContextBridgeTest(RuntimeFixture, unittest.TestCase):

    CODEX = PROMPT_DUMP_CODEX

    def setUp(self) -> None:
        super().setUp()
        (self.repo / "target.py").write_text("def target():\n    return 1\n", encoding="utf-8")
        (self.repo / "tests").mkdir()
        (self.repo / "tests" / "test_target.py").write_text("from target import target\n\ndef test_target():\n    assert target() == 1\n", encoding="utf-8")
        self.git("add", "target.py", "tests/test_target.py")
        self.git("commit", "-qm", "add target")

    def test_the_project_graph_pack_reaches_the_worker_brief_without_widening_scope(self) -> None:
        dump = self.tmp / "prompt.txt"
        spec = _spec("implement", {
            "implement": {"kind": "agent", "effect": "write", "plan_task": "t1", "mode": "run",
                          "context": {"task": "${goal}", "max_files": 6},
                          "proof": ["plan.task.t1.patch_present"]},
            "done": {"kind": "terminal"},
        }, [{"from": "implement", "to": "done", "outcomes": ["completed"]}])
        result = self.run_graph(spec, goal="change target and its test", env_extra={"PROMPT_DUMP": str(dump)})
        self.assertEqual(result["status"], "terminal", result)
        started = self.started(result["run_id"], "implement")[0]
        self.assertRegex(started["context_pack_sha256"], r"^[0-9a-f]{64}$")
        self.assertGreaterEqual(started["context_file_count"], 1)
        self.assertTrue(started["project_graph_revision"])
        prompt = dump.read_text(encoding="utf-8")
        self.assertIn("## Project Graph orientation", prompt)
        self.assertIn("target.py", prompt)
        self.assertIn("tests/test_target.py", prompt)
        self.assertNotIn("def target():", prompt)  # orientation names files; it never inlines source
        # The pack expanded nothing: the task's write scope is untouched and the brief still states it.
        code, view = self.plan_json("show", "--id", "t1")
        self.assertEqual(view["allowed_paths"], ["delegated.txt"])
        self.assertIn("allowed_paths: delegated.txt", prompt)

    def test_an_unbuildable_pack_is_recorded_not_fatal(self) -> None:
        spec = _spec("implement", {
            "implement": {"kind": "agent", "effect": "write", "plan_task": "t1", "mode": "run",
                          "context": {"task": "anything", "max_files": 3}, "proof": ["plan.task.t1.patch_present"]},
            "done": {"kind": "terminal"},
        }, [{"from": "implement", "to": "done", "outcomes": ["completed"]}])
        with mock.patch.object(runner.project_build, "ensure", side_effect=OSError("disk full")):
            result = self.run_graph(spec)
        self.assertEqual(result["status"], "terminal", result)
        started = self.started(result["run_id"], "implement")[0]
        self.assertEqual(started["context"]["status"], "unavailable")
        self.assertIn("disk full", started["context"]["reason"])
        self.assertNotIn("context_pack_sha256", started)


@unittest.skipUnless(HAVE_GIT, "git is required to drive plan-run")
class CapabilityToolTest(RuntimeFixture, unittest.TestCase):

    def test_a_capability_node_runs_the_registry_argv_without_a_shell_or_oms_on_path(self) -> None:
        (self.bin / "oms").unlink()  # the registry reaches the checkout's scripts directly
        spec = _spec("inspect", {
            "inspect": {"kind": "tool", "tool": "project_context", "task": "${goal}", "max_files": 4, "cacheable": True},
            "accept": {"kind": "tool", "tool": "plan_acceptance", "proof": ["receipt.acceptance.latest"]},
            "done": {"kind": "terminal"},
        }, [{"from": "inspect", "to": "accept", "outcomes": ["completed"]},
            {"from": "accept", "to": "done", "outcomes": ["completed", "failed"]}])
        result = self.run_graph(spec, goal="check script")
        self.assertEqual(result["status"], "terminal", result)
        run_id = result["run_id"]
        recorded = {row["node"]: row for row in self.outcomes(run_id)}
        self.assertEqual(recorded["inspect"]["outcome"], "completed")
        pack = json.loads((events.run_dir(self.repo, run_id) / "artifacts" / "inspect-1.txt").read_text(encoding="utf-8"))
        self.assertEqual(pack["task"], "check script")
        self.assertIn("scripts/check.sh", pack["files"])
        # The acceptance contract ran and left its receipt (it fails: nothing is delegated yet).
        self.assertEqual(recorded["accept"]["outcome"], "failed")
        self.assertEqual(recorded["accept"]["facts"].get("receipt.acceptance.latest"), "fail")
        # A second run replays the cached orientation pack.
        again = self.run_graph(spec, goal="check script")
        self.assertTrue({row["node"]: row for row in self.outcomes(again["run_id"])}["inspect"]["cached"])


class BundledSpecShapeTest(unittest.TestCase):
    def test_bundled_specs_bind_selection_and_commit_exactly(self) -> None:
        for name in ("goal-drive", "coding-change"):
            spec = load_spec(name)
            self.assertEqual(spec["nodes"]["implement"]["bind_task"], "work_item")
            self.assertEqual(spec["nodes"]["land"]["plan_task_from"], "work_item")
            self.assertEqual((spec["nodes"]["commit"]["tool"], spec["nodes"]["commit"]["binding"]), ("commit_bound", "work_item"))
            self.assertEqual(spec["nodes"]["commit"]["effect"], "write")
            self.assertEqual(spec["nodes"]["acceptance"]["tool"], "plan_acceptance")
        self.assertEqual(capabilities.render_goal("${goal}", "fix parser", "fallback"), "fix parser")
        self.assertEqual(capabilities.render_goal("", "", "fallback"), "fallback")


if __name__ == "__main__":
    unittest.main()
