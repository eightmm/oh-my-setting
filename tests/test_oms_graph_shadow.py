"""Shadow reconstruction: where the graph would stand against current reality.

Pure tests over the bundled `goal-drive` spec and synthetic facts — no plan,
no provider, no disk. The runtime-facing half (the hook line, the ledger row)
is covered by graph-smoke and the integration suite.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))

from oms_graph import shadow
from oms_graph.spec import load_spec


def facts(**overrides):
    base = {
        "git.head": "abc", "git.dirty": False, "git.branch": "main",
        "plan.present": True, "plan.all_done": False, "plan.has_unfinished": True,
        "plan.actionable": [], "plan.contract.satisfied": False,
    }
    base.update(overrides)
    return base


def task(task_id, state, **extra):
    prefix = "plan.task.%s." % task_id
    row = {prefix + "state": state, prefix + "patch_present": False, prefix + "artifact_present": False,
           prefix + "lease_present": False, prefix + "claim_expired": False, prefix + "repair_count": 0,
           prefix + "reason": ""}
    for key, value in extra.items():
        row[prefix + key] = value
    return row


class RealityTaskTest(unittest.TestCase):
    def test_in_flight_task_wins_over_the_plan_fifo_next(self) -> None:
        view = facts(**{"plan.actionable": ["t3"]}, **task("t1", "review"), **task("t2", "claimed"), **task("t3", "ready"))
        self.assertEqual(shadow.reality_task(view), "t1")

    def test_falls_back_to_the_first_actionable_task(self) -> None:
        self.assertEqual(shadow.reality_task(facts(**{"plan.actionable": ["t2", "t1"]}, **task("t2", "ready"))), "t2")
        self.assertEqual(shadow.reality_task(facts()), "")


class ReconstructTest(unittest.TestCase):
    def setUp(self) -> None:
        self.spec = load_spec("goal-drive")

    def test_an_unproven_check_is_assumed_failed_and_the_ready_task_is_bound(self) -> None:
        result = shadow.reconstruct(self.spec, facts(**{"plan.actionable": ["t1"]}, **task("t1", "ready")))
        self.assertEqual(result["assumed_failed"], ["acceptance"])
        self.assertEqual(result["bindings"], {"work_item": "t1"})
        self.assertEqual(result["frontier"], "implement")
        self.assertEqual(result["frontier_kind"], "agent")
        self.assertEqual(result["stop"], "unproven")
        self.assertEqual(result["completed"], [])

    def test_fresh_passing_acceptance_reaches_the_done_terminal(self) -> None:
        result = shadow.reconstruct(self.spec, facts(**{
            "receipt.acceptance.latest": "pass", "receipt.acceptance.fresh": True, "plan.all_done": True}))
        self.assertEqual(result["completed"], ["acceptance"])
        self.assertEqual(result["route"]["status"], "terminal")
        self.assertEqual(result["frontier"], "done")
        self.assertEqual(result["assumed_failed"], [])

    def test_a_task_in_review_settles_implement_so_land_is_the_frontier(self) -> None:
        view = facts(**{"receipt.acceptance.latest": "fail", "receipt.acceptance.fresh": True},
                     **task("t1", "review", patch_present=True))
        result = shadow.reconstruct(self.spec, view)
        self.assertEqual(result["bindings"], {"work_item": "t1"})
        self.assertEqual(result["completed"], ["implement"])
        self.assertEqual(result["frontier"], "land")
        self.assertEqual(result["stop"], "unproven")

    def test_a_stale_acceptance_with_no_task_left_owes_the_check_itself(self) -> None:
        view = facts(**{"plan.all_done": True, "plan.has_unfinished": False,
                        "receipt.acceptance.latest": "pass", "receipt.acceptance.fresh": False},
                     **task("t1", "done"))
        result = shadow.reconstruct(self.spec, view)
        self.assertEqual(result["frontier"], "acceptance")
        self.assertEqual(result["stop"], "check")
        self.assertEqual(result["bindings"], {})

    def test_a_write_node_is_never_assumed_failed(self) -> None:
        # implement is unproven for a ready task: the reconstruction stops there
        # instead of assuming the work failed and routing to `parked`.
        result = shadow.reconstruct(self.spec, facts(**{"plan.actionable": ["t1"]}, **task("t1", "ready")))
        self.assertNotIn("implement", result["assumed_failed"])
        self.assertEqual(result["route"]["status"], "actionable")


class CompareTest(unittest.TestCase):
    def setUp(self) -> None:
        self.spec = load_spec("goal-drive")

    def test_exact_frontier_agreement(self) -> None:
        recon = shadow.reconstruct(self.spec, facts(**{"receipt.acceptance.latest": "fail", "receipt.acceptance.fresh": True},
                                                    **task("t1", "review", patch_present=True)))
        verdict = shadow.compare(recon, "review_or_land_patch")
        self.assertEqual((verdict["agree"], verdict["basis"], verdict["mapped"]), (True, "frontier", "land"))

    def test_a_ready_task_agrees_with_execute_ready_task_at_the_implement_frontier(self) -> None:
        recon = shadow.reconstruct(self.spec, facts(**{"plan.actionable": ["t1"]}, **task("t1", "ready")))
        self.assertEqual(shadow.compare(recon, "execute_ready_task")["basis"], "frontier")
        self.assertFalse(shadow.compare(recon, "verify_active_task")["agree"])

    def test_an_owed_check_agrees_with_verify_and_with_its_effectful_successor(self) -> None:
        recon = shadow.reconstruct(self.spec, facts(**{"plan.all_done": True, "receipt.acceptance.latest": "pass",
                                                       "receipt.acceptance.fresh": False}, **task("t1", "done")))
        self.assertEqual(shadow.compare(recon, "verify_active_task")["basis"], "frontier")
        self.assertEqual(shadow.compare(recon, "record_verified_completion")["basis"], "successor")

    def test_an_undecidable_tool_frontier_agrees_with_its_successor(self) -> None:
        recon = shadow.reconstruct(load_spec("coding-change"), facts(**{"plan.actionable": ["t1"]}, **task("t1", "ready")))
        self.assertEqual((recon["frontier"], recon["stop"]), ("inspect", "undecidable"))
        self.assertEqual(shadow.compare(recon, "execute_ready_task")["basis"], "successor")

    def test_a_write_frontier_does_not_borrow_successor_agreement(self) -> None:
        recon = shadow.reconstruct(self.spec, facts(**{"receipt.acceptance.latest": "fail", "receipt.acceptance.fresh": True},
                                                    **task("t1", "review", patch_present=True)))
        verdict = shadow.compare(recon, "execute_ready_task")
        self.assertFalse(verdict["agree"])
        self.assertEqual(verdict["basis"], "")

    def test_unknown_or_blocker_actions(self) -> None:
        recon = shadow.reconstruct(self.spec, facts(**{"plan.actionable": ["t1"]}, **task("t1", "ready")))
        # A blocker the control plane sees and the graph's facts do not: a disagreement worth recording.
        self.assertEqual(shadow.compare(recon, "resolve_blocker"), {"action": "resolve_blocker", "mapped": "blocked", "agree": False, "basis": ""})
        self.assertEqual(shadow.compare(recon, "")["mapped"], "")
        self.assertFalse(shadow.compare(recon, "")["agree"])


if __name__ == "__main__":
    unittest.main()
