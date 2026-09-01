"""Pure execution-graph routing and deterministic fixture evaluation."""

from __future__ import annotations

from typing import Any, Dict, List, Mapping, Optional, Sequence, Set, Tuple

from .errors import GraphError
from .predicates import holds, missing
from .spec import load_spec, normalize_spec
from .validate import validate_spec


def effective_outcome(node_spec: Mapping[str, Any], claimed: str, facts: Mapping[str, Any]) -> Tuple[str, List[str]]:
    """A claimed completed/approved stands only when every proof predicate holds."""
    if claimed not in ("completed", "approved"):
        return claimed, []
    absent = missing(node_spec.get("proof", []), facts)
    return ("unverified", absent) if absent else (claimed, [])


def state_from_outcomes(spec: Mapping[str, Any], outcomes: Mapping[str, str], *, gates: Optional[Mapping[str, str]] = None, repeats: Optional[Mapping[str, int]] = None) -> Dict[str, Any]:
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
    for node_id, node in nodes.items():
        node_state = node_states.get(node_id, {}) if isinstance(node_states.get(node_id, {}), Mapping) else {}
        claimed = node_state.get("claimed_outcome", node_state.get("outcome"))
        if claimed is None and node_id in gates:
            claimed = gates[node_id]
        if node_state.get("status") == "finished" and claimed is not None:
            outcome, absent = effective_outcome(node, str(claimed), facts)
            effective[node_id] = outcome
            if absent:
                downgrades.append({"node": node_id, "claimed": str(claimed), "effective": outcome, "missing": absent})

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
        source_state = node_states.get(source, {}) if isinstance(node_states.get(source, {}), Mapping) else {}
        if source_state.get("status") != "finished" or source not in effective:
            return False
        return effective[source] in edge.get("outcomes", []) and all(holds(item, facts) for item in edge.get("when", []))

    def join_ready(node_id: str) -> bool:
        edges = [edge for edge in incoming.get(node_id, [])
                 if edge.get("kind", "normal") == "normal" and edge["from"] != node_id and
                 edge["from"] not in reachable.get(node_id, set())]
        if len(edges) < 2:
            return True
        matches = [source_matches(edge) for edge in edges]
        return any(matches) if nodes[node_id].get("join", "all") == "any" else all(matches)

    frontier: List[Dict[str, Any]] = []
    prospective_repeats = dict(repeat_counts)

    def push(kind: str, node_id: str, reason: str = "") -> None:
        frontier.append({"kind": kind, "node": node_id, "reason": reason})

    def follow_from(node_id: str, outcome: str, ancestry: Set[str]) -> None:
        selected = selected_edges(node_id, outcome)
        if not selected:
            push("blocked", node_id, "no route for outcome %s from %s" % (outcome, node_id))
            return
        for edge in selected:
            target = edge["to"]
            trace.append("%s --%s--> %s" % (node_id, outcome, target))
            if edge.get("kind") == "repeat":
                key = "%s->%s" % (node_id, target)
                next_count = prospective_repeats.get(key, 0) + 1
                if next_count > max_repeats:
                    push("exhausted", target, "repeat budget exhausted for %s" % key)
                else:
                    prospective_repeats[key] = next_count
                    push("candidate", target)
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
        if node_state.get("status") == "finished" and node_id in effective:
            if node.get("kind") == "terminal":
                push("terminal", node_id)
            else:
                follow_from(node_id, effective[node_id], ancestry)
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
        if node.get("kind") == "agent" and node.get("plan_task"):
            resources.append({"kind": "plan_task", "id": str(node["plan_task"])})
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
        return result("blocked", blocked[0]["node"], [], blocked[0]["reason"])
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
    if not all(isinstance(value, Mapping) for value in (facts, outcomes, gates, repeats)):
        raise GraphError("fixture facts, outcomes, gates, and repeats must be objects")
    actual_route = evaluate(graph, state_from_outcomes(graph, outcomes, gates=gates, repeats=repeats), facts)
    expected = dict(fixture.get("expect", {})) if isinstance(fixture.get("expect", {}), Mapping) else {}
    actual: Dict[str, Any] = {}
    diff: List[Dict[str, Any]] = []
    for key, wanted in expected.items():
        if key == "downgrades":
            got = [item.get("node") for item in actual_route.get("downgrades", [])]
        else:
            got = actual_route.get(key)
        actual[key] = got
        if got != wanted:
            diff.append({"key": key, "expected": wanted, "actual": got})
    return not diff, {"expected": expected, "actual": actual, "diff": diff}
