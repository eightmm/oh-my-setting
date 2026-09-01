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
        value = simple_spec(); value["nodes"]["work"] = {"kind": "agent", "plan_task": "t1", "plan_task_from": "item"}
        cases["invalid_task_binding"] = value
        value = simple_spec(); value["nodes"]["work"] = {"kind": "agent", "plan_task_from": "ghost"}
        cases["unknown_task_binding"] = value
        value = simple_spec(); value["nodes"]["work"] = {"kind": "agent", "plan_task": "next", "bind_task": "item"}; value["nodes"]["twin"] = {"kind": "agent", "plan_task": "next", "bind_task": "item"}; value["edges"] = [{"from": "work", "to": "twin", "outcomes": ["completed"]}, {"from": "twin", "to": "done", "outcomes": ["completed"]}]
        cases["duplicate_task_binding_writer"] = value
        value = simple_spec(); value["nodes"]["work"] = {"kind": "agent", "plan_task_from": "item"}; value["nodes"]["binder"] = {"kind": "agent", "plan_task": "next", "bind_task": "item"}; value["edges"] = [{"from": "work", "to": "binder", "outcomes": ["completed"]}, {"from": "binder", "to": "done", "outcomes": ["completed"]}]
        cases["unreachable_task_binding"] = value
        value = simple_spec(); value["nodes"]["work"] = {"kind": "agent", "plan_task": "t1", "context": {"task": 3, "max_files": 0}}
        cases["invalid_context"] = value
        value = simple_spec(); value["nodes"]["work"] = {"kind": "tool", "tool": "teleport"}
        cases["unknown_tool_capability"] = value

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

    def test_tool_capabilities(self):
        from oms_graph import capabilities
        # A capability node needs no command and takes its effect from the registry.
        graph = simple_spec(); graph["nodes"]["work"] = {"kind": "tool", "tool": "plan_acceptance"}
        verdict = validate_spec(graph)
        self.assertTrue(verdict["ok"], verdict["errors"])
        self.assertEqual(normalize_spec(graph)["nodes"]["work"]["effect"], "read")
        self.assertNotIn("unverified_effect_declaration", {item["code"] for item in verdict["warnings"]})
        # A raw command with a read declaration is legal but unverified.
        raw = simple_spec(); raw["nodes"]["work"] = {"kind": "tool", "effect": "read", "command": "echo one"}
        self.assertIn("unverified_effect_declaration", {item["code"] for item in validate_spec(raw)["warnings"]})
        wrote = simple_spec(); wrote["nodes"]["work"] = {"kind": "tool", "effect": "write", "command": "touch x"}
        self.assertNotIn("unverified_effect_declaration", {item["code"] for item in validate_spec(wrote)["warnings"]})
        # Both a capability and a command, a conflicting effect, a non-cacheable
        # capability marked cacheable, a bad parameter, and a commit without a writer.
        for node, code in (
            ({"kind": "tool", "tool": "plan_acceptance", "command": "true"}, "invalid_command"),
            ({"kind": "tool", "tool": "plan_acceptance", "effect": "write"}, "invalid_effect"),
            ({"kind": "tool", "tool": "plan_acceptance", "cacheable": True}, "invalid_command"),
            ({"kind": "tool", "tool": "project_context", "max_files": 0}, "invalid_command"),
            ({"kind": "tool", "tool": "project_context", "task": 7}, "invalid_command"),
            ({"kind": "tool", "tool": "commit_bound"}, "invalid_command"),
            ({"kind": "tool", "tool": "commit_bound", "binding": "no such"}, "invalid_task_binding"),
            ({"kind": "tool", "tool": "commit_bound", "binding": "orphan"}, "unknown_task_binding"),
        ):
            graph = simple_spec(); graph["nodes"]["work"] = node
            self.assertIn(code, {item["code"] for item in validate_spec(graph)["errors"]}, node)
        # The argv is the registry's, built without a shell.
        repo = Path("/tmp/repo")
        self.assertEqual(capabilities.argv(repo, {"kind": "tool", "tool": "plan_acceptance"})[-3:], ["--repo", "/tmp/repo", "accept"])
        context = capabilities.argv(repo, {"kind": "tool", "tool": "project_context", "task": "fix ${goal}", "max_files": 3}, goal="lease")
        self.assertEqual(context[-7:], ["project", "context", "--task", "fix lease", "--max-files", "3", "--json"])
        commit = capabilities.argv(repo, {"kind": "tool", "tool": "commit_bound", "binding": "work_item"}, run_id="run-x")
        self.assertEqual(commit[-6:], ["exec", "commit", "--binding", "work_item", "--run", "run-x"])
        self.assertTrue(all(item.endswith((".sh", "accept", "context", "commit")) or not item.endswith(".py") for item in commit))
        with self.assertRaises(GraphError):
            capabilities.argv(repo, {"kind": "tool", "tool": "teleport"})
        self.assertEqual(capabilities.render_goal("${goal}", "fix parser", "fallback"), "fix parser")
        self.assertEqual(capabilities.render_goal("", "", "fallback"), "fallback")

    def test_capabilities_name_only_their_three_front_doors(self):
        import ast as ast_module
        from oms_graph import capabilities
        source = Path(capabilities.__file__).read_text(encoding="utf-8")
        scripts = {node.value for node in ast_module.walk(ast_module.parse(source))
                   if isinstance(node, ast_module.Constant) and isinstance(node.value, str) and node.value.endswith(".sh")}
        self.assertEqual(scripts, {"agent-plan.sh", "graph.sh"})
        for number, line in enumerate(source.splitlines(), 1):
            if "agent-plan" not in line:
                continue
            for verb in ("land", "finish", "claim", "review", "start", "release", "block"):
                self.assertNotIn(verb, line, "line %d names agent-plan %s" % (number, verb))
        self.assertEqual(capabilities.names(), ["commit_bound", "plan_acceptance", "project_context"])
        # The agent-plan verb is a fixed literal, never a parameter: no registry
        # entry accepts a verb, and the only agent-plan argv ends in `accept`.
        for entry in capabilities.REGISTRY.values():
            self.assertNotIn("verb", entry["params"])
        for name in capabilities.names():
            node = {"kind": "tool", "tool": name, "binding": "work_item", "task": "x", "max_files": 2}
            command = capabilities.argv(Path("/tmp/repo"), node, run_id="run-x")
            if command[1].endswith("agent-plan.sh"):
                self.assertEqual(command[2:], ["--repo", "/tmp/repo", "accept"])
            else:
                self.assertTrue(command[1].endswith("graph.sh"), command)
                self.assertIn(command[4], ("project", "exec"))

    def test_bundled_specs_use_capabilities_only(self):
        for name in ("coding-change", "goal-drive"):
            graph = load_spec(name)
            for node_id, node in graph["nodes"].items():
                if node.get("kind") == "tool":
                    self.assertIn("tool", node, "%s.%s is a raw command node" % (name, node_id))
                    self.assertNotIn("command", node)
            self.assertNotIn("unverified_effect_declaration", {item["code"] for item in validate_spec(graph)["warnings"]})

    def test_binding_rules(self):
        # A repeating writer may rebind its own name; a binder on a static task is legal;
        # a selector on a write node is legal; a non-agent binder is not.
        graph = simple_spec()
        graph["nodes"]["work"] = {"kind": "agent", "effect": "write", "plan_task": "next", "bind_task": "item"}
        graph["nodes"]["after"] = {"kind": "agent", "plan_task_from": "item", "mode": "land"}
        graph["edges"] = [{"from": "work", "to": "after", "outcomes": ["completed"]},
                          {"from": "work", "to": "work", "outcomes": ["failed"], "kind": "repeat"},
                          {"from": "after", "to": "done", "outcomes": ["completed"]}]
        self.assertTrue(validate_spec(graph)["ok"], validate_spec(graph)["errors"])
        graph["nodes"]["work"]["plan_task"] = "t1"
        self.assertTrue(validate_spec(graph)["ok"])
        graph["nodes"]["done"]["bind_task"] = "other"
        self.assertIn("invalid_task_binding", {item["code"] for item in validate_spec(graph)["errors"]})
        for bad in ("", "no spaces", "a.b", "-lead"):
            graph = simple_spec(); graph["nodes"]["work"] = {"kind": "agent", "plan_task": "t1", "bind_task": bad}
            self.assertIn("invalid_task_binding", {item["code"] for item in validate_spec(graph)["errors"]}, bad)


class GraphRouteTest(unittest.TestCase):
    def setUp(self):
        self.graph = load_spec("coding-change")

    def route(self, outcomes=None, facts=None, gates=None, repeats=None, bindings=None):
        # The bundled spec binds its selection as `work_item`; fixtures name the task `implement`.
        bound = {"work_item": "implement"} if bindings is None else bindings
        state = state_from_outcomes(self.graph, outcomes or {}, gates=gates or {}, repeats=repeats or {}, bindings=bound)
        return evaluate(self.graph, state, facts or {})

    def test_a_reader_without_its_binding_is_a_named_missing_resource(self):
        facts = {"plan.task.implement.patch_present": True, "plan.task.implement.state": "review"}
        route = self.route({"inspect": "completed", "implement": "completed"}, facts, {"review": "approved"}, bindings={})
        self.assertEqual((route["status"], route["primary"]), ("blocked", "land"))
        self.assertEqual(route["required_resources"], [{"kind": "task_binding", "name": "work_item"}])
        self.assertIn("missing task binding: work_item", route["reason"])

    def test_actionable_resources_name_the_concrete_task_or_the_selector(self):
        selector = self.route({"inspect": "completed"})
        self.assertEqual(selector["required_resources"], [{"kind": "plan_task", "id": "next", "selector": True}])
        facts = {"plan.task.implement.patch_present": True, "plan.task.implement.state": "review"}
        bound = self.route({"inspect": "completed", "implement": "completed"}, facts, {"review": "approved"})
        self.assertEqual(bound["required_resources"], [{"kind": "plan_task", "id": "implement", "binding": "work_item"}])

    def test_changes_requested_reaches_replan_not_an_invented_repair(self):
        facts = {"plan.task.implement.patch_present": True, "plan.task.implement.state": "review"}
        route = self.route({"inspect": "completed", "implement": "completed"}, facts, {"review": "changes_requested"})
        self.assertEqual((route["status"], route["primary"]), ("terminal", "replan"))

    def test_a_terminal_reached_by_alternative_failure_edges_is_not_a_join(self):
        facts = {"plan.task.implement.state": "blocked"}
        route = self.route({"inspect": "completed", "implement": "blocked"}, facts)
        self.assertEqual((route["status"], route["primary"]), ("terminal", "parked"))

    def _sequenced(self, seqs, outcomes, gates=None):
        """A projection whose node seqs order the latest attempts."""
        state = state_from_outcomes(self.graph, outcomes, gates=gates or {}, bindings={"work_item": "implement"})
        for node_id, seq in seqs.items():
            state["nodes"][node_id]["seq"] = seq
        return state

    def test_a_repeat_that_already_happened_is_walked_past(self):
        # acceptance failed (seq 12) then implement re-ran (seq 15): the repeat is history,
        # and land (seq 9) is stale relative to its upstream, so land is due — without budget.
        facts = {"plan.task.implement.patch_present": True, "plan.task.implement.state": "review",
                 "receipt.land.implement.present": True, "receipt.acceptance.latest": "fail", "receipt.acceptance.fresh": True}
        state = self._sequenced({"inspect": 3, "implement": 15, "review": 7, "land": 9, "acceptance": 12},
                                {"inspect": "completed", "implement": "completed", "land": "completed", "acceptance": "failed"},
                                gates={"review": "approved"})
        route = evaluate(self.graph, state, facts)
        self.assertEqual((route["status"], route["primary"]), ("gate", "review"), route)
        # The review gate is stale as well: it awaits a fresh decision, and no repeat budget was spent on it.
        self.assertEqual(route["budget"]["repeats"].get("review->land", 0), 0)

    def test_a_stale_downstream_node_is_due_again(self):
        facts = {"plan.task.implement.patch_present": True, "plan.task.implement.state": "review"}
        state = self._sequenced({"inspect": 3, "implement": 15, "review": 17, "land": 9},
                                {"inspect": "completed", "implement": "completed", "land": "completed"},
                                gates={"review": "approved"})
        route = evaluate(self.graph, state, facts)
        self.assertEqual((route["status"], route["primary"]), ("actionable", "land"))
        self.assertEqual(route["downgrades"], [])

    def test_order_free_states_keep_the_repeat_semantics(self):
        facts = {"plan.task.implement.state": "done", "plan.task.implement.patch_present": True, "git.dirty": False,
                 "receipt.land.implement.present": True, "receipt.acceptance.latest": "fail", "receipt.acceptance.fresh": True}
        route = self.route({"inspect": "completed", "implement": "completed", "land": "completed", "commit": "completed", "acceptance": "failed"},
                           facts, {"review": "approved"})
        self.assertEqual((route["status"], route["primary"]), ("actionable", "implement"))
        self.assertEqual(route["budget"]["repeats"]["acceptance->implement"], 1)

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

    def _binding_spec(self):
        graph = simple_spec()
        graph["nodes"]["work"] = {"kind": "agent", "effect": "write", "plan_task": "next", "bind_task": "item"}
        graph["nodes"]["after"] = {"kind": "agent", "plan_task_from": "item", "mode": "land"}
        graph["edges"] = [{"from": "work", "to": "after", "outcomes": ["completed"]},
                          {"from": "after", "to": "work", "outcomes": ["failed"], "kind": "repeat"},
                          {"from": "after", "to": "done", "outcomes": ["completed"]}]
        return graph

    def test_binding_is_projected_from_node_started_and_survives_replay(self):
        graph = self._binding_spec()
        rows = [{"schema": 1, "seq": 1, "run_id": "r", "event": "node_started", "node": "work", "attempt": 1,
                 "task_id": "t1", "binding": "item", "idempotency_key": "start:work:1"}]
        projection = events.project(rows, graph)
        self.assertEqual(projection["bindings"], {"item": {"task_id": "t1", "node": "work", "attempt": 1}})
        self.assertEqual(projection["nodes"]["work"]["task_id"], "t1")
        self.assertEqual(projection["nodes"]["work"]["seq"], 1)
        # The binding survives the outcome row and duplicate rows alike.
        rows.append({"schema": 1, "seq": 2, "run_id": "r", "event": "node_outcome", "node": "work", "attempt": 1,
                     "outcome": "completed", "claimed_outcome": "completed", "task_id": "t1", "idempotency_key": "outcome:work:1"})
        once = events.project(rows, graph)
        twice = events.project(rows + [copy.deepcopy(rows[0])], graph)
        self.assertEqual(once["bindings"]["item"]["task_id"], "t1")
        self.assertEqual(once, twice)
        self.assertEqual(once["nodes"]["work"]["seq"], 2)

    def test_a_later_attempt_overwrites_the_binding(self):
        graph = self._binding_spec()
        rows = [{"schema": 1, "seq": 1, "run_id": "r", "event": "node_started", "node": "work", "attempt": 1, "task_id": "t1", "binding": "item"},
                {"schema": 1, "seq": 2, "run_id": "r", "event": "node_outcome", "node": "work", "attempt": 1, "outcome": "completed", "task_id": "t1"},
                {"schema": 1, "seq": 3, "run_id": "r", "event": "node_started", "node": "after", "attempt": 1, "task_id": "t1"},
                {"schema": 1, "seq": 4, "run_id": "r", "event": "node_outcome", "node": "after", "attempt": 1, "outcome": "failed", "task_id": "t1"},
                {"schema": 1, "seq": 5, "run_id": "r", "event": "node_started", "node": "work", "attempt": 2, "task_id": "t2", "binding": "item"}]
        projection = events.project(rows, graph)
        self.assertEqual(projection["bindings"]["item"], {"task_id": "t2", "node": "work", "attempt": 2})
        # Rows without a binding name never bind, and old rows without task ids still project.
        legacy = events.project([{"schema": 1, "seq": 1, "run_id": "r", "event": "node_outcome", "node": "work", "attempt": 1, "outcome": "failed"}], graph)
        self.assertEqual(legacy["bindings"], {})
        self.assertEqual(legacy["nodes"]["work"]["status"], "finished")

    def test_binding_facts_are_derived_not_stored(self):
        from oms_graph import binding
        graph = self._binding_spec()
        projection = {"bindings": {"item": {"task_id": "t1", "node": "work", "attempt": 1}}}
        facts = {"plan.task.t1.state": "review", "plan.task.t1.patch_present": True, "receipt.land.t1.present": False,
                 "receipt.admit.t1.latest": "verified", "plan.task.t2.state": "ready"}
        derived = binding.augment_binding_facts(graph, projection, facts)
        self.assertEqual(derived["binding.item.task_id"], "t1")
        self.assertEqual(derived["binding.item.state"], "review")
        self.assertTrue(derived["binding.item.patch_present"])
        self.assertFalse(derived["binding.item.receipt.land.present"])
        self.assertEqual(derived["binding.item.receipt.admit.latest"], "verified")
        self.assertNotIn("binding.item.t2", derived)
        self.assertEqual(binding.augment_binding_facts(graph, {"bindings": {}}, facts), facts)
        # The concrete node the adapter sees names the task, not the binding.
        concrete = binding.effective_node({"kind": "agent", "plan_task_from": "item", "mode": "land",
                                           "proof": ["binding.item.receipt.land.present", "binding.item.state=done", "!binding.item.reason"]}, "t1")
        self.assertEqual(concrete["plan_task"], "t1")
        self.assertNotIn("plan_task_from", concrete)
        self.assertEqual(concrete["proof"], ["receipt.land.t1.present", "plan.task.t1.state=done", "!plan.task.t1.reason"])
        with self.assertRaises(GraphError):
            binding.effective_node({"kind": "agent", "plan_task": "next"}, "next")

    def test_detail_scrubbing(self):
        with self.assertRaises(GraphError):
            events.append_event(self.repo, self.run_id, "note", detail="api_" "key=supersecretvalue12345")


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


class GraphFixtureCorpusTest(unittest.TestCase):
    def test_every_bundled_fixture_routes_as_expected(self):
        import json
        fixtures = sorted((ROOT / "tests" / "fixtures" / "graph-routes").glob("*.json"))
        self.assertGreaterEqual(len(fixtures), 14)
        for path in fixtures:
            with self.subTest(fixture=path.name):
                ok, detail = run_fixture(json.loads(path.read_text(encoding="utf-8")))
                self.assertTrue(ok, detail)

    def test_history_keeps_its_recorded_proof(self):
        # After landing, the task is done: implement's own proof (state=review)
        # no longer holds, but its completion is history, not the frontier.
        graph = load_spec("coding-change")
        facts = {"plan.task.implement.state": "done", "plan.task.implement.patch_present": True, "receipt.land.implement.present": True}
        state = state_from_outcomes(graph, {"inspect": "completed", "implement": "completed", "land": "completed", "commit": "completed"},
                                    gates={"review": "approved"}, bindings={"work_item": "implement"})
        route = evaluate(graph, dict(state), dict(facts, **{"git.dirty": False}))
        self.assertEqual((route["status"], route["primary"]), ("actionable", "acceptance"))
        self.assertEqual(route["downgrades"], [])

    def test_back_edge_spends_the_repeat_budget(self):
        graph = load_spec("coding-change")
        facts = {"plan.task.implement.state": "done", "plan.task.implement.patch_present": True, "git.dirty": False,
                 "receipt.land.implement.present": True, "receipt.acceptance.latest": "fail", "receipt.acceptance.fresh": True}
        state = state_from_outcomes(graph, {"inspect": "completed", "implement": "completed", "land": "completed", "commit": "completed", "acceptance": "failed"},
                                    gates={"review": "approved"}, repeats={"acceptance->implement": 3}, bindings={"work_item": "implement"})
        self.assertEqual(evaluate(graph, state, facts)["status"], "exhausted")

    def test_bundled_goal_drive_binds_its_selection(self):
        graph = load_spec("goal-drive")
        self.assertEqual(graph["nodes"]["implement"]["bind_task"], "work_item")
        self.assertEqual(graph["nodes"]["land"]["plan_task_from"], "work_item")
        self.assertNotIn("plan_task", graph["nodes"]["land"])
        # Before implement binds, land is a named missing resource, not an unknown scope.
        state = state_from_outcomes(graph, {"acceptance": "failed", "implement": "completed"})
        route = evaluate(graph, state, {"plan.task.t1.state": "review", "plan.task.t1.patch_present": True})
        self.assertEqual(route["status"], "actionable")
        self.assertEqual(route["primary"], "implement")  # unproved claim -> unverified -> repeat implement
        bound = state_from_outcomes(graph, {"acceptance": "failed", "implement": "completed"}, bindings={"work_item": "t1"})
        route = evaluate(graph, bound, {"plan.task.t1.state": "review", "plan.task.t1.patch_present": True})
        self.assertEqual((route["status"], route["primary"]), ("actionable", "land"))
        self.assertEqual(route["required_resources"], [{"kind": "plan_task", "id": "t1", "binding": "work_item"}])


if __name__ == "__main__":
    unittest.main()
