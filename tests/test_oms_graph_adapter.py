from __future__ import annotations
import ast
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))

from oms_graph import scheduler
from oms_graph.adapters import plan as plan_adapter
from oms_graph.errors import GraphError

HAVE_GIT = shutil.which("git") is not None

# The gate scrubs the invoking session's own harness identity (scripts/check.sh);
# a fixture that inherits OMS_HARNESS_CHILD=1 loses agent-plan init/add and
# fails from inside a harness-mediated run while passing from an operator shell.
SCRUBBED = (
    "OMS_HARNESS_CHILD", "OMS_HARNESS_ORIGIN", "OMS_HARNESS_PARENT_AGENT",
    "OMS_HARNESS_CALL_ID", "OMS_STATE_REPO", "OMS_ATTEMPT_ID", "OMS_PLAN_LEASE_ID",
    "OMS_LEASE_ID", "OMS_EXECUTOR_ID", "OMS_SOUL_SHA256", "OMS_APPROVAL_ID",
    "OMS_LANDING_ID", "OMS_WORKER_AUTHORITY_EXCLUSIVE",
)

CHECK_SCRIPT = """#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  t1) grep -Fxq one delegated.txt ;;
  t2) grep -Fxq two delegated2.txt ;;
  *) exit 2 ;;
esac
"""

# The autonomy-plan-run-smoke.sh provider stub: a write-capable transport that
# answers discovery, then reads the brief from stdin and edits its one file.
FAKE_CODEX = """#!/usr/bin/env bash
case "${1:-}" in
  --version)
    printf 'codex 1.0\\n'
    exit 0
    ;;
  exec)
    if [ "${2:-}" = "--help" ]; then
      printf 'usage: codex exec\\n'
      exit 0
    fi
    ;;
esac
prompt="$(cat)"
[ -z "${FAIL_TASK:-}" ] || [ "${OMS_TASK_ID:-}" != "$FAIL_TASK" ] || {
  echo worker-failed >&2
  exit 9
}
case "${OMS_TASK_ID:-}:$prompt" in
  t2:*) printf 'two\\n' > delegated2.txt ;;
  *) printf 'one\\n' > delegated.txt ;;
esac
echo worker-ok
"""


def agent_node(task_id="implement", mode="run", **extra):
    node = {"kind": "agent", "effect": "write", "plan_task": task_id, "mode": mode}
    node.update(extra)
    return node


class NextTaskCommandTest(unittest.TestCase):
    def test_next_selects_plan_run_next(self):
        argv = plan_adapter.build_command(Path("/repo"), {"kind": "agent", "plan_task": "next", "mode": "run"}, provider="codex")
        self.assertIn("--next", argv)
        self.assertNotIn("--id", argv)
        landing = plan_adapter.build_command(Path("/repo"), {"kind": "agent", "plan_task": "next", "mode": "land"}, provider="codex")
        self.assertEqual(landing[-2:], ["--next", "--land"])

    def test_result_line_resolves_the_selected_task(self):
        found = plan_adapter.RESULT_TASK_RE.search("plan-run: result task=t7 state=review artifact=- patch=- next=review-or-land\n")
        self.assertEqual(found.group(1), "t7")


class ScopeOverlapTest(unittest.TestCase):
    """Conservative by design: an unknown scope conflicts with everything."""

    def test_literal_scopes_overlap_only_when_one_contains_the_other(self) -> None:
        self.assertTrue(scheduler.scopes_overlap(["src"], ["src"]))
        self.assertTrue(scheduler.scopes_overlap(["src"], ["src/app"]))
        self.assertTrue(scheduler.scopes_overlap(["src/app"], ["src"]))
        self.assertFalse(scheduler.scopes_overlap(["src"], ["docs"]))
        self.assertFalse(scheduler.scopes_overlap(["delegated.txt"], ["delegated2.txt"]))

    def test_glob_overlaps_a_literal_it_could_select(self) -> None:
        self.assertTrue(scheduler.scopes_overlap(["src/*.py"], ["src"]))
        self.assertTrue(scheduler.scopes_overlap(["src"], ["src/*.py"]))
        self.assertTrue(scheduler.scopes_overlap(["src/lib/*.py"], ["src/lib/a.py"]))
        self.assertFalse(scheduler.scopes_overlap(["src/*.py"], ["docs"]))

    def test_two_globs_overlap_unless_their_static_prefixes_diverge(self) -> None:
        self.assertTrue(scheduler.scopes_overlap(["src/*.py"], ["src/**"]))
        self.assertFalse(scheduler.scopes_overlap(["src/*.py"], ["docs/*.md"]))
        # A leading glob has no static prefix and can select anywhere.
        self.assertTrue(scheduler.scopes_overlap(["*.py"], ["docs/*.md"]))

    def test_unknown_or_invalid_scope_conflicts_with_everything(self) -> None:
        self.assertTrue(scheduler.scopes_overlap([], ["src"]))
        self.assertTrue(scheduler.scopes_overlap(["src"], []))
        self.assertTrue(scheduler.scopes_overlap([], []))
        self.assertTrue(scheduler.scopes_overlap(["../etc"], ["src"]))
        self.assertTrue(scheduler.scopes_overlap(["/abs/path"], ["src"]))

    def test_static_prefix_stops_at_the_last_directory_separator(self) -> None:
        self.assertEqual(scheduler._static_prefix("src/lib/*.py"), "src/lib")
        self.assertEqual(scheduler._static_prefix("src/li*b/x"), "src")
        self.assertEqual(scheduler._static_prefix("*.py"), "")


class EligibilityTest(unittest.TestCase):

    SPEC = {
        "nodes": {
            "inspect": {"kind": "tool", "effect": "read"},
            "implement": agent_node("implement"),
            "docs": agent_node("docs"),
            "review": {"kind": "gate", "authority": "parent"},
            "land": agent_node("implement", mode="land"),
            "publish": {"kind": "tool", "effect": "write"},
            "guarded": {"kind": "tool", "effect": "read", "requires": ["plan.present"]},
            "orphan": agent_node("missing-task"),
        }
    }
    SCOPES = {
        "implement": {"allowed": ["src"], "forbidden": []},
        "docs": {"allowed": ["docs"], "forbidden": []},
    }

    def run_eligible(self, primary, alternatives=(), *, status="actionable", facts=None, **kwargs):
        route = {"status": status, "primary": primary, "alternatives": list(alternatives)}
        return scheduler.eligible(
            self.SPEC, {}, facts or {}, route=route, task_scopes=self.SCOPES, **kwargs
        )

    def test_a_route_that_is_not_actionable_yields_nothing(self) -> None:
        result = self.run_eligible("implement", status="blocked")
        self.assertEqual(result["eligible"], [])
        self.assertEqual(result["deferred"], [{"node": "implement", "reason": "route:blocked"}])

    def test_capacity_bounds_the_selection(self) -> None:
        result = self.run_eligible("implement", ["docs"], capacity=1)
        self.assertEqual(result["eligible"], ["implement"])
        self.assertEqual(result["deferred"], [{"node": "docs", "reason": "capacity"}])
        wider = self.run_eligible("implement", ["docs"], capacity=2)
        self.assertEqual(wider["eligible"], ["implement", "docs"])
        self.assertEqual(wider["deferred"], [])

    def test_a_gate_is_never_scheduled(self) -> None:
        result = self.run_eligible("review", ["implement"], capacity=2)
        self.assertEqual(result["eligible"], ["implement"])
        self.assertEqual(result["deferred"], [{"node": "review", "reason": "gate"}])

    def test_unmet_requires_defers_with_the_failing_predicate(self) -> None:
        result = self.run_eligible("guarded")
        self.assertEqual(result["eligible"], [])
        self.assertEqual(result["deferred"], [{"node": "guarded", "reason": "requires:plan.present"}])
        met = self.run_eligible("guarded", facts={"plan.present": True})
        self.assertEqual(met["eligible"], ["guarded"])

    def test_overlapping_write_scopes_conflict_instead_of_racing(self) -> None:
        spec = {"nodes": dict(self.SPEC["nodes"], sibling=agent_node("sibling"))}
        scopes = dict(self.SCOPES, sibling={"allowed": ["src/app"], "forbidden": []})
        route = {"status": "actionable", "primary": "implement", "alternatives": ["sibling"]}
        result = scheduler.eligible(spec, {}, {}, route=route, task_scopes=scopes, capacity=2)
        self.assertEqual(result["eligible"], ["implement"])
        self.assertEqual(
            result["conflicts"], [{"node": "sibling", "with": "implement", "reason": "scope-overlap"}]
        )

    def test_an_active_write_scope_also_blocks_a_new_writer(self) -> None:
        result = self.run_eligible("implement", capacity=2, active=("docs",))
        self.assertEqual(result["eligible"], ["implement"])
        blocked = self.run_eligible("docs", capacity=2, active=("docs",))
        self.assertEqual(blocked["eligible"], [])
        self.assertEqual(blocked["conflicts"], [{"node": "docs", "with": "docs", "reason": "scope-overlap"}])

    def test_an_agent_node_without_a_known_scope_is_a_conflict(self) -> None:
        result = self.run_eligible("orphan")
        self.assertEqual(result["eligible"], [])
        self.assertEqual(result["conflicts"], [{"node": "orphan", "with": "", "reason": "unknown-scope"}])

    def test_a_land_node_runs_alone(self) -> None:
        alone = self.run_eligible("land")
        self.assertEqual(alone["eligible"], ["land"])

        with_active = self.run_eligible("land", active=("docs",), capacity=2)
        self.assertEqual(with_active["eligible"], [])
        self.assertEqual(with_active["deferred"], [{"node": "land", "reason": "exclusive"}])

        beside_another = self.run_eligible("docs", ["land"], capacity=2)
        self.assertEqual(beside_another["eligible"], ["docs"])
        self.assertEqual(beside_another["deferred"], [{"node": "land", "reason": "exclusive"}])

        nothing_joins = self.run_eligible("land", ["docs"], capacity=2)
        self.assertEqual(nothing_joins["eligible"], ["land"])
        self.assertEqual(nothing_joins["deferred"], [{"node": "docs", "reason": "exclusive"}])

    def test_a_write_capable_tool_is_exclusive_and_needs_no_plan_scope(self) -> None:
        alone = self.run_eligible("publish")
        self.assertEqual(alone["eligible"], ["publish"])
        self.assertEqual(alone["conflicts"], [])
        beside = self.run_eligible("docs", ["publish"], capacity=2)
        self.assertEqual(beside["eligible"], ["docs"])
        self.assertEqual(beside["deferred"], [{"node": "publish", "reason": "exclusive"}])

    def test_eligibility_is_pure_and_repeatable(self) -> None:
        first = self.run_eligible("implement", ["docs", "review"], capacity=2)
        second = self.run_eligible("implement", ["docs", "review"], capacity=2)
        self.assertEqual(first, second)


class BuildCommandTest(unittest.TestCase):

    PLAN_RUN = str(ROOT / "scripts" / "plan-run.sh")

    def test_a_run_node_builds_the_plain_plan_run_argv(self) -> None:
        argv = plan_adapter.build_command(Path("/tmp/repo"), agent_node("t1"), provider="codex")
        self.assertEqual(
            argv,
            ["bash", self.PLAN_RUN, "--repo", "/tmp/repo", "--to", "codex", "--id", "t1"],
        )

    def test_a_land_node_adds_land_and_the_optional_flags(self) -> None:
        argv = plan_adapter.build_command(
            Path("/tmp/repo"),
            agent_node("t1", mode="land"),
            provider="codex",
            model="gpt-5.1-codex",
            reasoning_effort="high",
            repair=2,
        )
        self.assertEqual(
            argv,
            [
                "bash", self.PLAN_RUN, "--repo", "/tmp/repo", "--to", "codex", "--id", "t1",
                "--land", "--repair", "2", "--model", "gpt-5.1-codex",
                "--reasoning-effort", "high",
            ],
        )

    def test_dry_run_is_plan_runs_own_flag(self) -> None:
        argv = plan_adapter.build_command(
            Path("/tmp/repo"), agent_node("t1"), provider="codex", dry_run=True
        )
        self.assertEqual(argv[-1], "--dry-run")

    def test_only_a_well_formed_agent_node_builds_a_command(self) -> None:
        repo = Path("/tmp/repo")
        with self.assertRaises(GraphError):
            plan_adapter.build_command(repo, {"kind": "tool", "plan_task": "t1"}, provider="codex")
        with self.assertRaises(GraphError):
            plan_adapter.build_command(repo, agent_node(""), provider="codex")
        with self.assertRaises(GraphError):
            plan_adapter.build_command(repo, agent_node("t1", mode="finish-it"), provider="codex")
        with self.assertRaises(GraphError):
            plan_adapter.build_command(repo, agent_node("t1"), provider="")
        with self.assertRaises(GraphError):
            plan_adapter.build_command(repo, agent_node("t1"), provider="--land")
        with self.assertRaises(GraphError):
            plan_adapter.build_command(repo, agent_node("t1"), provider="codex", repair=4)
        with self.assertRaises(GraphError):
            plan_adapter.build_command(repo, agent_node("t1"), provider="codex", model="--repo")


class FrontDoorTest(unittest.TestCase):
    """The adapter must have no code path to a fenced agent-plan lifecycle verb.

    `land`, `finish`, `claim`, `review` and `start` are compare-and-set
    transitions whose expected-value receipts only `patch-land.sh` can compute.
    """

    FENCED = ("land", "finish", "claim", "review", "start")
    SOURCE = Path(plan_adapter.__file__).read_text(encoding="utf-8")
    TREE = ast.parse(SOURCE)

    def test_the_read_verb_allowlist_is_frozen(self) -> None:
        self.assertEqual(plan_adapter.PLAN_READ_VERBS, ("status", "show", "evidence-snapshot"))

    def test_every_fenced_verb_is_refused_at_runtime(self) -> None:
        for verb in self.FENCED:
            with self.assertRaises(GraphError):
                plan_adapter._plan_argv(Path("/tmp/repo"), verb, "--id", "t1")

    def test_only_the_two_front_door_scripts_are_named(self) -> None:
        scripts = {
            node.value
            for node in ast.walk(self.TREE)
            if isinstance(node, ast.Constant) and isinstance(node.value, str) and node.value.endswith(".sh")
        }
        self.assertEqual(scripts, {"agent-plan.sh", "plan-run.sh"})

    def test_every_plan_invocation_passes_a_literal_allowlisted_verb(self) -> None:
        seen = []
        for node in ast.walk(self.TREE):
            if not isinstance(node, ast.Call):
                continue
            name = node.func.id if isinstance(node.func, ast.Name) else ""
            if name != "_plan_argv":
                continue
            self.assertGreaterEqual(len(node.args), 2, "_plan_argv needs a literal verb")
            verb = node.args[1]
            self.assertIsInstance(verb, ast.Constant, "the agent-plan verb must be a literal")
            self.assertIn(verb.value, plan_adapter.PLAN_READ_VERBS)
            seen.append(verb.value)
        self.assertTrue(seen, "no agent-plan call sites found; the guard would be vacuous")

    def test_no_source_line_pairs_agent_plan_with_a_fenced_verb(self) -> None:
        for number, line in enumerate(self.SOURCE.splitlines(), 1):
            if "agent-plan" not in line:
                continue
            for verb in self.FENCED:
                self.assertNotIn(verb, line, "line %d names agent-plan %s" % (number, verb))


class OutcomeMappingTest(unittest.TestCase):

    RUN = agent_node("t1")
    LAND = agent_node("t1", mode="land")

    def test_blocked_state_outranks_every_other_reading(self) -> None:
        task = {"id": "t1", "state": "blocked", "patch": "p.patch", "reason": "parked"}
        facts = {"receipt.land.t1.present": True}
        self.assertEqual(plan_adapter.outcome_from_task(self.RUN, task, facts), ("blocked", []))
        self.assertEqual(plan_adapter.outcome_from_task(self.LAND, task, facts), ("blocked", []))

    def test_a_run_node_completes_on_a_reviewed_patch(self) -> None:
        task = {"id": "t1", "state": "review", "patch": "p.patch"}
        self.assertEqual(plan_adapter.outcome_from_task(self.RUN, task, {}), ("completed", []))
        # review without a stored patch is not evidence of anything finished
        self.assertEqual(
            plan_adapter.outcome_from_task(self.RUN, {"id": "t1", "state": "review"}, {}), ("failed", [])
        )
        # the same state does not complete a landing node
        self.assertEqual(plan_adapter.outcome_from_task(self.LAND, task, {}), ("failed", []))

    def test_a_land_node_completes_only_with_a_landing_receipt(self) -> None:
        task = {"id": "t1", "state": "done", "patch": "p.patch"}
        with_receipt = {"receipt.land.t1.present": True}
        self.assertEqual(plan_adapter.outcome_from_task(self.LAND, task, with_receipt), ("completed", []))
        self.assertEqual(plan_adapter.outcome_from_task(self.LAND, task, {}), ("failed", []))

    def test_a_run_node_accepts_a_task_already_landed_elsewhere(self) -> None:
        task = {"id": "t1", "state": "done", "patch": "p.patch"}
        self.assertEqual(plan_adapter.outcome_from_task(self.RUN, task, {}), ("completed", []))

    def test_a_held_claim_is_unverified_not_failed(self) -> None:
        for state in ("claimed", "running"):
            task = {"id": "t1", "state": state, "patch": ""}
            self.assertEqual(plan_adapter.outcome_from_task(self.RUN, task, {}), ("unverified", []))
            self.assertEqual(plan_adapter.outcome_from_task(self.LAND, task, {}), ("unverified", []))

    def test_a_requeued_task_is_failed(self) -> None:
        self.assertEqual(
            plan_adapter.outcome_from_task(self.RUN, {"id": "t1", "state": "ready"}, {}), ("failed", [])
        )

    def test_a_claimed_completion_without_its_proof_is_unverified(self) -> None:
        node = agent_node("t1", proof=["plan.task.t1.patch_present"])
        task = {"id": "t1", "state": "review", "patch": "p.patch"}
        self.assertEqual(
            plan_adapter.outcome_from_task(node, task, {"plan.task.t1.patch_present": False}),
            ("unverified", ["plan.task.t1.patch_present"]),
        )
        self.assertEqual(
            plan_adapter.outcome_from_task(node, task, {"plan.task.t1.patch_present": True}),
            ("completed", []),
        )

    def test_scope_of_reads_the_plan_declaration(self) -> None:
        task = {"allowed_paths": ["src", "./docs"], "forbidden_paths": "secrets.txt"}
        self.assertEqual(
            plan_adapter.scope_of(task), {"allowed": ["docs", "src"], "forbidden": ["secrets.txt"]}
        )
        self.assertEqual(plan_adapter.scope_of({}), {"allowed": [], "forbidden": []})


@unittest.skipUnless(HAVE_GIT, "git is required to drive plan-run")
class AdapterEndToEndTest(unittest.TestCase):
    """One temporary repository driven through the real plan-run front door."""

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="oms-graph-adapter."))
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

        self.git("init", "-q")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "user.name", "Test")
        self.git("add", "README.md", "scripts/check.sh")
        self.git("commit", "-qm", "base")

        self.plan("init", "--goal", "g", "--accept", "bash scripts/check.sh t1")
        self.plan("add", "--id", "t1", "--title", "t", "--allowed", "delegated.txt",
                  "--verify", "bash scripts/check.sh t1")
        self.plan("add", "--id", "t2", "--title", "t2", "--allowed", "delegated2.txt",
                  "--verify", "bash scripts/check.sh t2")

        self._saved_env = dict(os.environ)
        self.addCleanup(self._restore_env)

    def _restore_env(self) -> None:
        os.environ.clear()
        os.environ.update(self._saved_env)

    @staticmethod
    def _write(path: Path, body: str) -> None:
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)

    def _env(self) -> dict:
        env = dict(self._saved_env) if hasattr(self, "_saved_env") else dict(os.environ)
        for name in SCRUBBED:
            env.pop(name, None)
        env["HOME"] = str(self.home)
        # peer-delegate loads NVM_DIR before execution; keep discovery hermetic.
        env["NVM_DIR"] = str(self.home / ".nvm")
        return env

    def git(self, *args: str) -> None:
        subprocess.run(["git", "-C", str(self.repo)] + list(args), check=True,
                       env=self._env(), stdout=subprocess.DEVNULL)

    def plan(self, *args: str) -> None:
        argv = ["bash", str(ROOT / "scripts" / "agent-plan.sh"), "--repo", str(self.repo)] + list(args)
        subprocess.run(argv, check=True, env=self._env(), cwd=str(self.repo), stdout=subprocess.DEVNULL)

    def worker_env(self, **extra):
        """Swap in the fixture environment; plan-run inherits this process's."""
        env = self._env()
        env["PATH"] = "%s:/usr/bin:/bin" % self.bin
        env.update(extra)
        os.environ.clear()
        os.environ.update(env)

    def execute(self, node, *, env_extra=None, **kwargs):
        """Run one node with the fixture's private PATH visible to plan-run."""
        self.worker_env(**(env_extra or {}))
        try:
            options = {"provider": "codex", "timeout": 600}
            options.update(kwargs)
            return plan_adapter.execute(self.repo, node, **options)
        finally:
            self._restore_env()

    def test_run_then_land_then_a_failed_worker(self) -> None:
        run_node = agent_node("t1", proof=["plan.task.t1.patch_present"])
        result = self.execute(run_node)
        self.assertEqual(result["outcome"], "completed", result["stderr_tail"])
        self.assertEqual(result["claimed_outcome"], "completed")
        self.assertEqual(result["proof_missing"], [])
        self.assertEqual(result["exit"], 0)
        self.assertEqual(result["task"]["state"], "review")
        self.assertTrue(result["facts"]["plan.task.t1.patch_present"])
        self.assertFalse(result["facts"]["receipt.land.t1.present"])
        # A review-default run never touches the working tree.
        self.assertFalse((self.repo / "delegated.txt").exists())
        self.assertEqual(result["argv"][1], str(ROOT / "scripts" / "plan-run.sh"))

        land_node = agent_node("t1", mode="land", proof=["receipt.land.t1.present"])
        landed = self.execute(land_node)
        self.assertEqual(landed["outcome"], "completed", landed["stderr_tail"])
        self.assertEqual(landed["exit"], 0)
        self.assertEqual(landed["task"]["state"], "done")
        self.assertTrue(landed["facts"]["receipt.land.t1.present"])
        self.assertEqual(landed["facts"]["receipt.admit.t1.latest"], "verified")
        self.assertEqual((self.repo / "delegated.txt").read_text(encoding="utf-8"), "one\n")

        # Landing leaves reviewable working-tree bytes; a later admission must
        # not stack onto them.
        self.git("add", "delegated.txt")
        self.git("commit", "-qm", "test: commit the landed task")

        failed = self.execute(agent_node("t2"), env_extra={"FAIL_TASK": "t2"})
        self.assertEqual(failed["outcome"], "failed", failed["stderr_tail"])
        self.assertNotEqual(failed["exit"], 0)
        self.assertTrue(failed["reason"].startswith("plan-run-exit-"), failed["reason"])
        self.assertEqual(failed["task"]["state"], "ready")
        self.assertFalse(failed["facts"]["plan.task.t2.patch_present"])

    def test_dry_run_asks_plan_run_and_records_no_outcome(self) -> None:
        result = self.execute(agent_node("t1"), timeout=120, dry_run=True)
        self.assertEqual(result["outcome"], "skipped")
        self.assertEqual(result["claimed_outcome"], "skipped")
        self.assertEqual(result["reason"], "dry-run")
        self.assertIn("--dry-run", result["argv"])
        self.assertEqual(result["task"]["state"], "ready")

    def test_plan_status_and_task_view_read_the_real_plan(self) -> None:
        self.worker_env()
        try:
            status = plan_adapter.plan_status(self.repo)
            view = plan_adapter.task_view(self.repo, "t1")
            with self.assertRaises(GraphError):
                plan_adapter.task_view(self.repo, "no-such-task")
        finally:
            self._restore_env()
        self.assertTrue(status["present"])
        self.assertEqual(sorted(status["actionable"]), ["t1", "t2"])
        self.assertEqual(view["state"], "ready")
        self.assertEqual(plan_adapter.scope_of(view)["allowed"], ["delegated.txt"])


if __name__ == "__main__":
    unittest.main()
