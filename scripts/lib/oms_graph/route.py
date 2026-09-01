"""Pure execution-graph routing and deterministic fixture evaluation."""

from __future__ import annotations

from typing import Any, Dict, List, Mapping, Optional, Sequence, Set, Tuple

from . import binding
from .errors import GraphError
from .predicates import holds, missing
from .spec import load_spec, normalize_spec
from .validate import validate_spec


def normalize_bindings(value: Any) -> Dict[str, Dict[str, Any]]:
    """Fixture shorthand `{"name": "t1"}` or projection rows -> projection rows."""
    result: Dict[str, Dict[str, Any]] = {}
    if not isinstance(value, Mapping):
        return result
    for raw_name in sorted(value, key=str):
        entry = value[raw_name]
        name = str(raw_name)
        if isinstance(entry, Mapping):
            task_id = str(entry.get("task_id", "") or "")
            if task_id:
                result[name] = {"task_id": task_id, "node": str(entry.get("node", "") or ""),
                                "attempt": entry.get("attempt", 1)}
        elif isinstance(entry, str) and entry:
            result[name] = {"task_id": entry, "node": "", "attempt": 1}
    return result


def effective_outcome(node_spec: Mapping[str, Any], claimed: str, facts: Mapping[str, Any]) -> Tuple[str, List[str]]:
    """A claimed completed/approved stands only when every proof predicate holds."""
    if claimed not in ("completed", "approved"):
        return claimed, []
    absent = missing(node_spec.get("proof", []), facts)
    return ("unverified", absent) if absent else (claimed, [])


def state_from_outcomes(spec: Mapping[str, Any], outcomes: Mapping[str, str], *, gates: Optional[Mapping[str, str]] = None, repeats: Optional[Mapping[str, int]] = None, bindings: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    """Build an evaluator state from recorded outcomes (fixtures, tests)."""
    normalized = normalize_spec(spec)
    nodes = {
        node_id: {"status": "pending", "outcome": None, "claimed_outcome": None, "attempts": 0}
        for node_id in normalized["nodes"]
    }
    child_outcomes: Dict[str, Dict[str, str]] = {}
    child_gates: Dict[str, Dict[str, str]] = {}
    subgraph_nodes = {node_id for node_id, node in normalized["nodes"].items() if node.get("kind") == "subgraph"}
    steps = 0
    for raw_node, outcome in outcomes.items():
        node_id = str(raw_node)
        parent, separator, child = node_id.partition(".")
        if separator and parent in subgraph_nodes:
            child_outcomes.setdefault(parent, {})[child] = str(outcome)
            steps += 1
        elif node_id in nodes:
            nodes[node_id] = {"status": "finished", "outcome": str(outcome), "claimed_outcome": str(outcome), "attempts": 1}
            steps += 1
    gate_values = {str(key): str(value) for key, value in (gates or {}).items()}
    for node_id, decision in gate_values.items():
        parent, separator, child = node_id.partition(".")
        if separator and parent in subgraph_nodes:
            child_gates.setdefault(parent, {})[child] = decision
        elif node_id in nodes and nodes[node_id]["status"] != "finished":
            nodes[node_id] = {"status": "finished", "outcome": decision, "claimed_outcome": decision, "attempts": 1}
            steps += 1
    child_states: Dict[str, Any] = {}
    for parent in sorted(set(child_outcomes) | set(child_gates)):
        graph_name = normalized["nodes"][parent].get("graph")
        child_spec = normalized.get("subgraphs", {}).get(graph_name)
        if child_spec:
            child_states[parent] = state_from_outcomes(
                child_spec, child_outcomes.get(parent, {}), gates=child_gates.get(parent, {})
            )
    return {
        "nodes": nodes,
        "steps": steps,
        "repeats": {str(key): int(value) for key, value in (repeats or {}).items()},
        "gates": gate_values,
        "bindings": normalize_bindings(bindings),
        "subgraphs": child_states,
    }


def evaluate(spec: Mapping[str, Any], state: Mapping[str, Any], facts: Mapping[str, Any], *, authority: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    """Pure: no subprocess, no disk. Output shape in docs/GRAPH-ENGINEERING.md."""
    validation = validate_spec(spec)
    try:
        graph = normalize_spec(spec)
    except GraphError as exc:
        validation = {"ok": False, "errors": [{"code": "invalid_schema", "where": "spec", "message": str(exc)}], "warnings": []}
        graph = {"nodes": {}, "edges": [], "budget": {"max_steps": 20, "max_repeats": 3}, "entry": "", "stop_facts": []}
    # `binding.<name>.*` is derived here, never stored, so every caller proves
    # against the same view of what a bound task currently is.
    facts = binding.augment_binding_facts(graph, state, facts)
    nodes = graph.get("nodes", {})
    node_states = state.get("nodes", {}) if isinstance(state.get("nodes", {}), Mapping) else {}
    steps = state.get("steps", 0)
    if isinstance(steps, bool) or not isinstance(steps, int) or steps < 0:
        steps = 0
    repeat_counts = {
        str(key): value for key, value in (state.get("repeats", {}) if isinstance(state.get("repeats", {}), Mapping) else {}).items()
        if isinstance(value, int) and not isinstance(value, bool) and value >= 0
    }
    max_steps = graph.get("budget", {}).get("max_steps", 20)
    max_repeats = graph.get("budget", {}).get("max_repeats", 3)
    budget = {"steps_used": steps, "max_steps": max_steps, "repeats": dict(sorted(repeat_counts.items())), "max_repeats": max_repeats}
    effective: Dict[str, str] = {}
    downgrades: List[Dict[str, Any]] = []
    trace: List[str] = []

    def result(status: str, primary: Optional[str], alternatives: Sequence[str], reason: str,
               required: Optional[List[Dict[str, str]]] = None, gate: Any = None) -> Dict[str, Any]:
        return {
            "schema": 1,
            "status": status,
            "primary": primary,
            "alternatives": list(alternatives),
            "reason": reason,
            "required_resources": required or [],
            "gate": gate,
            "effective_outcomes": dict(sorted(effective.items())),
            "downgrades": downgrades,
            "budget": budget,
            "trace": trace,
        }

    if not validation.get("ok"):
        codes = [item.get("code", "invalid") for item in validation.get("errors", [])]
        return result("invalid", None, [], "invalid graph spec: %s" % ", ".join(codes))

    stop = [predicate for predicate in graph.get("stop_facts", []) if holds(predicate, facts)]
    if stop:
        return result("blocked", None, [], "stop facts hold: %s" % ", ".join(stop))
    if steps >= max_steps:
        return result("exhausted", None, [], "step budget exhausted")

    gates = state.get("gates", {}) if isinstance(state.get("gates", {}), Mapping) else {}

    def node_state_of(node_id: str) -> Mapping[str, Any]:
        value = node_states.get(node_id, {})
        return value if isinstance(value, Mapping) else {}

    def node_seq(node_id: str) -> int:
        """Event order of the node's latest attempt; 0 when the state carries
        no order (fixtures), which keeps the order-free semantics."""
        value = node_state_of(node_id).get("seq", 0)
        return value if isinstance(value, int) and not isinstance(value, bool) and value > 0 else 0

    def recorded_outcome(node_id: str) -> Tuple[Optional[str], Optional[str]]:
        """(claimed, recorded): the worker's claim and the outcome recorded after proof."""
        node_state = node_state_of(node_id)
        claimed = node_state.get("claimed_outcome", node_state.get("outcome"))
        if claimed is None and node_id in gates:
            claimed = gates[node_id]
        recorded = node_state.get("outcome", claimed)
        return (None if claimed is None else str(claimed), None if recorded is None else str(recorded))

    outgoing: Dict[str, List[Mapping[str, Any]]] = {node_id: [] for node_id in nodes}
    incoming: Dict[str, List[Mapping[str, Any]]] = {node_id: [] for node_id in nodes}
    for edge in graph.get("edges", []):
        outgoing[edge["from"]].append(edge)
        incoming[edge["to"]].append(edge)

    reachable: Dict[str, Set[str]] = {}
    for start in nodes:
        seen: Set[str] = set()
        pending = [edge["to"] for edge in outgoing.get(start, [])]
        while pending:
            current = pending.pop()
            if current in seen:
                continue
            seen.add(current)
            pending.extend(edge["to"] for edge in outgoing.get(current, []))
        reachable[start] = seen

    def selected_edges(node_id: str, outcome: str) -> List[Mapping[str, Any]]:
        matches = [edge for edge in outgoing.get(node_id, [])
                   if outcome in edge.get("outcomes", []) and all(holds(item, facts) for item in edge.get("when", []))]
        if not matches:
            return []
        priority = min(edge.get("priority", 0) for edge in matches)
        selected = [edge for edge in matches if edge.get("priority", 0) == priority]
        return selected if len(selected) == 1 or all(edge.get("fanout") for edge in selected) else selected[:1]

    def source_matches(edge: Mapping[str, Any]) -> bool:
        source = edge["from"]
        if node_state_of(source).get("status") != "finished":
            return False
        outcome = effective.get(source)
        if outcome is None:
            outcome = recorded_outcome(source)[1]
        return outcome in edge.get("outcomes", []) and all(holds(item, facts) for item in edge.get("when", []))

    def settle(node_id: str, ancestry: Set[str]) -> Optional[str]:
        """Effective outcome of a finished node.

        Only the frontier is re-proved against current facts: a node whose
        recorded route already led to further finished work keeps the outcome
        the runner recorded after its own proof check. Re-checking history
        against today's facts would contradict the spec's own happy path (an
        implement node proved by `state=review` is still complete after the
        landing that moved the task to `done`). Work that finished *before*
        this node's latest attempt is not further work: the node is then the
        frontier again.
        """
        if node_id in effective:
            return effective[node_id]
        claimed, recorded = recorded_outcome(node_id)
        if claimed is None or recorded is None:
            return None
        targets = [edge["to"] for edge in selected_edges(node_id, recorded)]
        frontier = not targets or any(
            node_state_of(target).get("status") != "finished" or target in ancestry or target == node_id
            or node_seq(target) < node_seq(node_id)
            for target in targets
        )
        if frontier:
            outcome, absent = effective_outcome(nodes[node_id], claimed, facts)
            if absent:
                downgrades.append({"node": node_id, "claimed": claimed, "effective": outcome, "missing": absent})
        else:
            outcome = recorded
        effective[node_id] = outcome
        return outcome

    def join_ready(node_id: str) -> bool:
        # A terminal ends the run for whichever path reaches it first; several
        # failure edges converging on `parked` are alternatives, not a fan-in.
        if nodes[node_id].get("kind") == "terminal":
            return True
        edges = [edge for edge in incoming.get(node_id, [])
                 if edge.get("kind", "normal") == "normal" and edge["from"] != node_id and
                 edge["from"] not in reachable.get(node_id, set())]
        if len(edges) < 2:
            return True
        matches = [source_matches(edge) for edge in edges]
        return any(matches) if nodes[node_id].get("join", "all") == "any" else all(matches)

    frontier: List[Dict[str, Any]] = []
    prospective_repeats = dict(repeat_counts)

    def push(kind: str, node_id: str, reason: str = "", required: Optional[List[Dict[str, str]]] = None) -> None:
        item: Dict[str, Any] = {"kind": kind, "node": node_id, "reason": reason}
        if required:
            item["required"] = required
        frontier.append(item)

    def task_resource(node_id: str, node: Mapping[str, Any]) -> Optional[Dict[str, Any]]:
        """The plan task an agent node would occupy: literal, bound, or a selector."""
        if node.get("kind") != "agent":
            return None
        source = node.get("plan_task_from")
        if isinstance(source, str) and source:
            bound = binding.bound_task(state, source)
            return {"kind": "plan_task", "id": bound, "binding": source} if bound else None
        task = str(node.get("plan_task", "") or "")
        if not task:
            return None
        if binding.is_selector(task):
            return {"kind": "plan_task", "id": task, "selector": True}
        return {"kind": "plan_task", "id": task}

    def push_due(target: str) -> None:
        """A node that must run (again): a gate awaits a fresh decision."""
        if nodes[target].get("kind") == "gate":
            push("gate", target, "%s awaits a new gate decision" % target)
        else:
            push("candidate", target)

    def follow_from(node_id: str, outcome: str, ancestry: Set[str]) -> None:
        selected = selected_edges(node_id, outcome)
        if not selected:
            push("blocked", node_id, "no route for outcome %s from %s" % (outcome, node_id))
            return
        for edge in selected:
            target = edge["to"]
            trace.append("%s --%s--> %s" % (node_id, outcome, target))
            target_finished = node_state_of(target).get("status") == "finished"
            back_edge = edge.get("kind") == "repeat" or target in ancestry or target == node_id
            if target_finished and node_seq(target) > node_seq(node_id):
                # The target's latest attempt came after this outcome: the
                # repeat (or the plain continuation) already happened, so the
                # route continues past it instead of asking for it again.
                expand(target, ancestry | {node_id})
                continue
            # A back-edge into the current path re-runs finished work exactly
            # like a declared repeat edge, and spends the same budget.
            if back_edge:
                key = "%s->%s" % (node_id, target)
                next_count = prospective_repeats.get(key, 0) + 1
                if next_count > max_repeats:
                    push("exhausted", target, "repeat budget exhausted for %s" % key)
                else:
                    prospective_repeats[key] = next_count
                    push_due(target)
                continue
            if target_finished and node_seq(target) < node_seq(node_id):
                # Finished before its upstream re-ran: due again. The cycle's
                # own repeat edge already paid the budget for this pass.
                push_due(target)
                continue
            expand(target, ancestry | {node_id})

    def expand(node_id: str, ancestry: Set[str]) -> None:
        if node_id in ancestry:
            push("blocked", node_id, "route cycle reached without a repeat edge")
            return
        node = nodes[node_id]
        if not join_ready(node_id):
            push("join", node_id, "join %s is waiting for incoming sources" % node_id)
            return
        node_state = node_states.get(node_id, {}) if isinstance(node_states.get(node_id, {}), Mapping) else {}
        decision = gates.get(node_id)
        if node_state.get("status") == "active":
            push("active", node_id, "%s is active without an outcome" % node_id)
            return
        if node_state.get("status") == "finished":
            outcome = settle(node_id, ancestry)
            if outcome is None:
                push("active", node_id, "%s finished without a recorded outcome" % node_id)
            elif node.get("kind") == "terminal":
                push("terminal", node_id)
            else:
                follow_from(node_id, outcome, ancestry)
            return
        if decision is not None:
            gate_outcome, absent = effective_outcome(node, str(decision), facts)
            effective[node_id] = gate_outcome
            if absent:
                downgrades.append({"node": node_id, "claimed": str(decision), "effective": gate_outcome, "missing": absent})
            follow_from(node_id, gate_outcome, ancestry)
            return
        absent_requirements = missing(node.get("requires", []), facts)
        if absent_requirements:
            push("blocked", node_id, "%s requires: %s" % (node_id, ", ".join(absent_requirements)))
            return
        kind = node.get("kind")
        if kind == "router":
            effective[node_id] = "completed"
            follow_from(node_id, "completed", ancestry)
        elif kind == "subgraph":
            graph_name = node.get("graph")
            child_spec = graph.get("subgraphs", {}).get(graph_name)
            child_states = state.get("subgraphs", {}) if isinstance(state.get("subgraphs", {}), Mapping) else {}
            child_state = child_states.get(node_id)
            if not isinstance(child_state, Mapping):
                child_state = state_from_outcomes(child_spec, {})
            child_route = evaluate(child_spec, child_state, facts, authority=authority)
            for key, value in child_route.get("effective_outcomes", {}).items():
                effective["%s.%s" % (node_id, key)] = value
            for item in child_route.get("downgrades", []):
                copy = dict(item)
                copy["node"] = "%s.%s" % (node_id, item.get("node"))
                downgrades.append(copy)
            if child_route.get("status") == "terminal":
                effective[node_id] = "completed"
                follow_from(node_id, "completed", ancestry)
            else:
                primary = child_route.get("primary")
                push("subgraph", "%s.%s" % (node_id, primary) if primary else node_id, child_route.get("reason", ""))
                frontier[-1]["route"] = child_route
        elif kind == "terminal":
            push("terminal", node_id)
        elif kind == "gate":
            push("gate", node_id, "%s awaits a gate decision" % node_id)
        elif kind == "agent" and isinstance(node.get("plan_task_from"), str) and not binding.bound_task(state, str(node["plan_task_from"])):
            # An unbound reader is a missing resource, named as such: never a
            # scheduler-internal "unknown scope".
            name = str(node["plan_task_from"])
            push("blocked", node_id, "missing task binding: %s" % name, [{"kind": "task_binding", "name": name}])
        else:
            push("candidate", node_id)

    expand(graph["entry"], set())
    budget["repeats"] = dict(sorted(prospective_repeats.items()))

    active = [item for item in frontier if item["kind"] == "active"]
    if active:
        return result("waiting", active[0]["node"], [], active[0]["reason"])
    exhausted = [item for item in frontier if item["kind"] == "exhausted"]
    candidates = [item for item in frontier if item["kind"] in ("candidate", "subgraph")]
    gate_items = [item for item in frontier if item["kind"] == "gate"]
    if candidates:
        primary = candidates[0]["node"]
        alternatives = []
        for item in candidates[1:]:
            if item["node"] not in alternatives and item["node"] != primary:
                alternatives.append(item["node"])
        first = candidates[0]
        if first["kind"] == "subgraph":
            child_route = first["route"]
            status = child_route.get("status", "blocked")
            child_alts = [first["node"].split(".", 1)[0] + "." + item for item in child_route.get("alternatives", [])]
            return result(status, primary, child_alts + alternatives, first.get("reason", ""), child_route.get("required_resources", []), child_route.get("gate"))
        node_id = primary.split(".", 1)[0]
        node = nodes.get(node_id, {})
        resources = []
        resource = task_resource(node_id, node)
        if resource:
            resources.append(resource)
        return result("actionable", primary, alternatives, "%s is actionable" % primary, resources)
    if gate_items:
        node_id = gate_items[0]["node"]
        node = nodes[node_id]
        gate = {"node": node_id, "authority": node.get("authority", "parent"), "decisions": list(node.get("decisions", []))}
        return result("gate", node_id, [item["node"] for item in gate_items[1:]], gate_items[0]["reason"], gate=gate)
    if exhausted:
        return result("exhausted", exhausted[0]["node"], [], exhausted[0]["reason"])
    terminal = [item for item in frontier if item["kind"] == "terminal"]
    joins = [item for item in frontier if item["kind"] == "join"]
    blocked = [item for item in frontier if item["kind"] == "blocked"]
    if terminal and not joins and not blocked:
        return result("terminal", terminal[0]["node"], [], "terminal %s reached" % terminal[0]["node"])
    if joins and not blocked:
        return result("waiting", joins[0]["node"], [], joins[0]["reason"])
    if blocked:
        return result("blocked", blocked[0]["node"], [], blocked[0]["reason"], blocked[0].get("required"))
    if terminal:
        return result("terminal", terminal[0]["node"], [], "terminal %s reached" % terminal[0]["node"])
    return result("blocked", None, [], "no actionable route")


def run_fixture(fixture: Mapping[str, Any]) -> Tuple[bool, Dict[str, Any]]:
    if not isinstance(fixture, Mapping):
        raise GraphError("route fixture must be an object")
    if "spec" in fixture:
        graph = load_spec(fixture["spec"])
    elif fixture.get("spec_ref"):
        graph = load_spec(str(fixture["spec_ref"]))
    else:
        raise GraphError("route fixture requires spec or spec_ref")
    facts = fixture.get("facts", {})
    outcomes = fixture.get("outcomes", {})
    gates = fixture.get("gates", {})
    repeats = fixture.get("repeats", {})
    bindings = fixture.get("bindings", {})
    if not all(isinstance(value, Mapping) for value in (facts, outcomes, gates, repeats, bindings)):
        raise GraphError("fixture facts, outcomes, gates, repeats, and bindings must be objects")
    state = state_from_outcomes(graph, outcomes, gates=gates, repeats=repeats, bindings=bindings)
    steps = fixture.get("steps")
    if isinstance(steps, int) and not isinstance(steps, bool) and steps >= 0:
        state["steps"] = steps
    actual_route = evaluate(graph, state, facts)
    expected = dict(fixture.get("expect", {})) if isinstance(fixture.get("expect", {}), Mapping) else {}
    actual: Dict[str, Any] = {}
    diff: List[Dict[str, Any]] = []
    for key, wanted in expected.items():
        if key == "downgrades":
            got = [item.get("node") for item in actual_route.get("downgrades", [])]
        elif key == "required_resources":
            got = actual_route.get("required_resources", [])
        else:
            got = actual_route.get(key)
        actual[key] = got
        if got != wanted:
            diff.append({"key": key, "expected": wanted, "actual": got})
    return not diff, {"expected": expected, "actual": actual, "diff": diff}
