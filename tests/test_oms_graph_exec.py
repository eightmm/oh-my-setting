from __future__ import annotations

import copy
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))

from oms_graph.errors import GraphError
from oms_graph import events
from oms_graph.facts import collect_facts
from oms_graph.route import evaluate, run_fixture, state_from_outcomes
from oms_graph.spec import load_spec, normalize_spec, spec_digest
from oms_graph.validate import ERROR_CODES, validate_spec
from oms_runtime.common import append_jsonl


def simple_spec():
    return {
        "schema": 1,
        "id": "simple",
        "entry": "work",
        "budget": {"max_steps": 8, "max_repeats": 2},
        "stop_facts": [],
        "nodes": {
            "work": {"kind": "tool", "command": "true"},
            "done": {"kind": "terminal"},
        },
        "edges": [{"from": "work", "to": "done", "outcomes": ["completed"]}],
    }


def branched_spec(join="all"):
    return {
        "schema": 1,
        "id": "branch",
        "entry": "start",
        "budget": {"max_steps": 20, "max_repeats": 1},
        "stop_facts": [],
        "nodes": {
            "start": {"kind": "tool", "command": "true"},
            "left": {"kind": "tool", "command": "true"},
            "right": {"kind": "tool", "command": "true"},
            "join": {"kind": "tool", "command": "true", "join": join},
            "done": {"kind": "terminal"},
        },
        "edges": [
            {"from": "start", "to": "left", "outcomes": ["completed"], "fanout": True},
            {"from": "start", "to": "right", "outcomes": ["completed"], "fanout": True},
            {"from": "left", "to": "join", "outcomes": ["completed"]},
            {"from": "right", "to": "join", "outcomes": ["completed"]},
            {"from": "join", "to": "done", "outcomes": ["completed"]},
        ],
    }


class GraphValidationTest(unittest.TestCase):
    def test_every_validator_error_code(self):
        cases = {}

        value = simple_spec(); value["schema"] = 2
        cases["invalid_schema"] = value
        value = simple_spec(); value["nodes"] = [{"id": "work", "kind": "tool", "command": "true"}, {"id": "work", "kind": "terminal"}]
        cases["duplicate_node"] = value
        value = simple_spec(); value["edges"][0]["to"] = "ghost"
        cases["unknown_endpoint"] = value
        value = simple_spec(); value["entry"] = "ghost"
        cases["missing_entry"] = value
        value = simple_spec(); value["nodes"]["orphan"] = {"kind": "terminal"}
        cases["unreachable_node"] = value
        value = simple_spec(); value["nodes"].pop("done"); value["edges"] = []
        cases["missing_terminal"] = value
        value = simple_spec(); value["edges"].append({"from": "done", "to": "work", "outcomes": ["completed"], "kind": "repeat"})
        cases["terminal_outgoing_edge"] = value
        value = simple_spec(); value["edges"][0]["outcomes"] = ["mystery"]
        cases["invalid_outcome"] = value
        value = simple_spec(); value["edges"][0]["kind"] = "again"
        cases["invalid_repeat_edge"] = value
        value = simple_spec(); value["nodes"]["loop"] = {"kind": "tool", "command": "true"}; value["edges"] = [{"from": "work", "to": "loop", "outcomes": ["completed"]}, {"from": "loop", "to": "work", "outcomes": ["completed"]}, {"from": "loop", "to": "done", "outcomes": ["failed"]}]
        cases["unbounded_cycle"] = value
        value = simple_spec(); value["nodes"]["work"] = {"kind": "subgraph", "graph": "missing"}
        cases["unknown_subgraph"] = value
        value = simple_spec(); value["nodes"]["work"] = {"kind": "subgraph", "graph": "child"}; value["subgraphs"] = {"child": {"entry": "nested", "nodes": {"nested": {"kind": "subgraph", "graph": "grand"}, "end": {"kind": "terminal"}}, "edges": [{"from": "nested", "to": "end", "outcomes": ["completed"]}], "subgraphs": {}}}
        cases["recursive_subgraph"] = value
        value = simple_spec(); value["nodes"]["work"]["requires"] = ["bad key"]
        cases["invalid_fact_reference"] = value
        value = simple_spec(); value["nodes"]["work"] = {"kind": "agent", "effect": "write"}
        cases["invalid_plan_task_reference"] = value
        value = simple_spec(); value["nodes"]["other"] = {"kind": "terminal"}; value["edges"].append({"from": "work", "to": "other", "outcomes": ["completed"]})
        cases["ambiguous_routes"] = value
        value = simple_spec(); value["nodes"]["work"]["effect"] = "network"
        cases["invalid_effect"] = value
        value = simple_spec(); value["nodes"]["work"]["kind"] = "model"
        cases["invalid_kind"] = value
        value = simple_spec(); value["nodes"]["work"]["join"] = "some"
        cases["invalid_join"] = value
        value = simple_spec(); value["nodes"]["work"]["command"] = ""
        cases["invalid_command"] = value
        value = simple_spec(); value["budget"]["max_steps"] = 0
        cases["invalid_budget"] = value
        value = simple_spec(); value["nodes"]["work"] = {"kind": "gate", "authority": "agent", "decisions": []}
        cases["invalid_gate"] = value

        self.assertEqual(set(cases), set(ERROR_CODES))
        for code, graph in cases.items():
            with self.subTest(code=code):
                found = {item["code"] for item in validate_spec(graph)["errors"]}
                self.assertIn(code, found)

    def test_bundled_specs_validate_cleanly(self):
        for name in ("coding-change", "goal-drive"):
            with self.subTest(name=name):
                graph = load_spec(name)
                self.assertTrue(validate_spec(graph)["ok"])
                self.assertEqual(spec_digest(graph), spec_digest(normalize_spec(graph)))

    def test_unknown_endpoint(self):
        graph = simple_spec(); graph["edges"][0]["to"] = "unknown"
        self.assertIn("unknown_endpoint", {item["code"] for item in validate_spec(graph)["errors"]})

    def test_unreachable_node(self):
        graph = simple_spec(); graph["nodes"]["lost"] = {"kind": "terminal"}
        self.assertIn("unreachable_node", {item["code"] for item in validate_spec(graph)["errors"]})

    def test_cycle_without_repeat_or_stop_fact_rejected(self):
        graph = simple_spec(); graph["edges"] = [{"from": "work", "to": "work", "outcomes": ["completed"]}]
        self.assertIn("unbounded_cycle", {item["code"] for item in validate_spec(graph)["errors"]})

    def test_recursive_subgraph_rejected(self):
        graph = simple_spec(); graph["nodes"]["work"] = {"kind": "subgraph", "graph": "child"}
        graph["subgraphs"] = {"child": {"entry": "again", "nodes": {"again": {"kind": "subgraph", "graph": "child"}, "end": {"kind": "terminal"}}, "edges": [{"from": "again", "to": "end", "outcomes": ["completed"]}]}}
        self.assertIn("recursive_subgraph", {item["code"] for item in validate_spec(graph)["errors"]})


class GraphRouteTest(unittest.TestCase):
    def setUp(self):
        self.graph = load_spec("coding-change")

    def route(self, outcomes=None, facts=None, gates=None, repeats=None):
        state = state_from_outcomes(self.graph, outcomes or {}, gates=gates or {}, repeats=repeats or {})
        return evaluate(self.graph, state, facts or {})

    def test_initial_route(self):
        route = self.route()
        self.assertEqual((route["status"], route["primary"]), ("actionable", "inspect"))

    def test_completed_transition(self):
        route = self.route({"inspect": "completed"})
        self.assertEqual((route["status"], route["primary"]), ("actionable", "implement"))

    def test_failed_transition(self):
        route = self.route({"inspect": "failed"})
        self.assertEqual(route["primary"], "implement")

    def test_unverified_downgrade(self):
        route = self.route({"inspect": "completed", "implement": "completed"})
        self.assertEqual((route["status"], route["primary"]), ("actionable", "implement"))
        self.assertEqual([item["node"] for item in route["downgrades"]], ["implement"])

    def test_partial_repeat(self):
        graph = simple_spec(); graph["edges"] = [{"from": "work", "to": "work", "outcomes": ["partial"], "kind": "repeat"}, {"from": "work", "to": "done", "outcomes": ["completed"]}]
        route = evaluate(graph, state_from_outcomes(graph, {"work": "partial"}), {})
        self.assertEqual((route["status"], route["primary"]), ("actionable", "work"))
        self.assertEqual(route["budget"]["repeats"]["work->work"], 1)

    def test_repeat_budget_exhaustion(self):
        graph = simple_spec(); graph["edges"] = [{"from": "work", "to": "work", "outcomes": ["partial"], "kind": "repeat"}, {"from": "work", "to": "done", "outcomes": ["completed"]}]
        state = state_from_outcomes(graph, {"work": "partial"}, repeats={"work->work": 2})
        self.assertEqual(evaluate(graph, state, {})["status"], "exhausted")

    def test_max_steps_exhaustion(self):
        state = state_from_outcomes(self.graph, {})
        state["steps"] = self.graph["budget"]["max_steps"]
        self.assertEqual(evaluate(self.graph, state, {})["status"], "exhausted")

    def test_terminal_correctness(self):
        graph = simple_spec()
        route = evaluate(graph, state_from_outcomes(graph, {"work": "completed"}), {})
        self.assertEqual((route["status"], route["primary"]), ("terminal", "done"))

    def test_join_all(self):
        graph = branched_spec("all")
        incomplete = evaluate(graph, state_from_outcomes(graph, {"start": "completed", "left": "completed"}), {})
        self.assertEqual(incomplete["primary"], "right")
        complete = evaluate(graph, state_from_outcomes(graph, {"start": "completed", "left": "completed", "right": "completed"}), {})
        self.assertEqual(complete["primary"], "join")

    def test_join_any(self):
        graph = branched_spec("any")
        route = evaluate(graph, state_from_outcomes(graph, {"start": "completed", "left": "completed"}), {})
        self.assertEqual((route["status"], route["primary"]), ("actionable", "join"))

    def test_subgraph_completion(self):
        graph = simple_spec()
        graph["nodes"]["work"] = {"kind": "subgraph", "graph": "child"}
        graph["subgraphs"] = {"child": {"entry": "inner", "nodes": {"inner": {"kind": "tool", "command": "true"}, "end": {"kind": "terminal"}}, "edges": [{"from": "inner", "to": "end", "outcomes": ["completed"]}]}}
        route = evaluate(graph, state_from_outcomes(graph, {"work.inner": "completed"}), {})
        self.assertEqual((route["status"], route["primary"]), ("terminal", "done"))

    def test_missing_required_fact_is_blocked(self):
        graph = simple_spec(); graph["nodes"]["work"]["requires"] = ["ready"]
        route = evaluate(graph, state_from_outcomes(graph, {}), {})
        self.assertEqual(route["status"], "blocked")
        self.assertIn("ready", route["reason"])

    def test_fact_backed_completion(self):
        facts = {"plan.task.implement.patch_present": True, "plan.task.implement.state": "review"}
        route = self.route({"inspect": "completed", "implement": "completed"}, facts)
        self.assertEqual((route["status"], route["primary"]), ("gate", "review"))
        self.assertEqual(route["downgrades"], [])

    def test_gate_waiting_then_decision(self):
        facts = {"plan.task.implement.patch_present": True, "plan.task.implement.state": "review"}
        waiting = self.route({"inspect": "completed", "implement": "completed"}, facts)
        self.assertEqual(waiting["status"], "gate")
        decided = self.route({"inspect": "completed", "implement": "completed"}, facts, {"review": "approved"})
        self.assertEqual(decided["primary"], "land")

    def test_run_fixture_inline(self):
        ok, detail = run_fixture({"spec": simple_spec(), "facts": {}, "outcomes": {}, "expect": {"status": "actionable", "primary": "work", "alternatives": [], "downgrades": []}})
        self.assertTrue(ok, detail)


class GraphEventsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.repo = Path(self.temp.name)
        self.old_lock_dir = os.environ.get("OMS_LOCK_DIR")
        os.environ["OMS_LOCK_DIR"] = str(self.repo / "locks")
        self.started = events.start_run(self.repo, simple_spec())
        self.run_id = self.started["run_id"]

    def tearDown(self):
        if self.old_lock_dir is None:
            os.environ.pop("OMS_LOCK_DIR", None)
        else:
            os.environ["OMS_LOCK_DIR"] = self.old_lock_dir
        self.temp.cleanup()

    def test_events_append_and_read(self):
        row = events.append_event(self.repo, self.run_id, "node_started", node="work", attempt=1, idempotency_key="start:work:1")
        self.assertEqual(row["seq"], 2)
        self.assertEqual(len(events.read_events(self.repo, self.run_id)), 2)
        self.assertRegex(self.run_id, r"^run-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{8}$")
        self.assertEqual(events.latest_run_id(self.repo), self.run_id)

    def test_duplicate_idempotency_key_refused(self):
        events.append_event(self.repo, self.run_id, "note", idempotency_key="note:one")
        with self.assertRaises(GraphError):
            events.append_event(self.repo, self.run_id, "note", idempotency_key="note:one")

    def test_projection_idempotent_under_duplicate_rows(self):
        row = {"schema": 1, "seq": 2, "run_id": self.run_id, "event": "node_outcome", "node": "work", "attempt": 1, "claimed_outcome": "completed", "outcome": "completed", "idempotency_key": "outcome:work:1"}
        once = events.project([row], simple_spec())
        twice = events.project([row, copy.deepcopy(row)], simple_spec())
        self.assertEqual(once, twice)
        self.assertEqual(once["steps"], 1)

    def test_resume_state_from_started_event(self):
        rows = [{"schema": 1, "seq": 1, "run_id": self.run_id, "event": "node_started", "node": "work", "attempt": 1}]
        projection = events.project(rows, simple_spec())
        self.assertEqual(projection["nodes"]["work"]["status"], "active")
        self.assertEqual(evaluate(simple_spec(), projection, {})["status"], "waiting")

    def test_detail_scrubbing(self):
        with self.assertRaises(GraphError):
            events.append_event(self.repo, self.run_id, "note", detail="api_key=supersecretvalue12345")


class GraphFactsTest(unittest.TestCase):
    def test_facts_from_real_temporary_plan_and_acceptance_receipt(self):
        with tempfile.TemporaryDirectory() as raw:
            repo = Path(raw)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / "file.txt").write_text("base\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(repo), "add", "file.txt"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
            environment = dict(os.environ)
            environment.pop("OMS_HARNESS_CHILD", None)
            environment["OMS_LOCK_DIR"] = str(repo / "locks")
            script = ROOT / "scripts" / "agent-plan.sh"
            subprocess.run(["bash", str(script), "--repo", str(repo), "init", "--goal", "g", "--accept", "true"], check=True, env=environment, stdout=subprocess.DEVNULL)
            subprocess.run(["bash", str(script), "--repo", str(repo), "add", "--id", "implement", "--title", "t", "--allowed", "src/", "--verify", "true"], check=True, env=environment, stdout=subprocess.DEVNULL)
            head = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
            old_lock_dir = os.environ.get("OMS_LOCK_DIR")
            os.environ["OMS_LOCK_DIR"] = environment["OMS_LOCK_DIR"]
            try:
                append_jsonl(repo / ".oms" / "plan" / "progress.jsonl", {"schema": 1, "kind": "acceptance", "status": "pass", "base_sha": head, "ts": "2026-09-01T00:00:00Z", "accept_sha256": "a" * 64})
                facts = collect_facts(repo)
            finally:
                if old_lock_dir is None:
                    os.environ.pop("OMS_LOCK_DIR", None)
                else:
                    os.environ["OMS_LOCK_DIR"] = old_lock_dir
            self.assertTrue(facts["plan.present"])
            self.assertEqual(facts["plan.task.implement.state"], "ready")
            self.assertIn("implement", facts["plan.actionable"])
            self.assertEqual(facts["receipt.acceptance.latest"], "pass")
            self.assertTrue(facts["receipt.acceptance.fresh"])


if __name__ == "__main__":
    unittest.main()
