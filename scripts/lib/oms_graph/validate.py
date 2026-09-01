"""Deterministic validation for execution graph specs."""

from __future__ import annotations

from typing import Any, Dict, List, Mapping, Optional, Sequence, Set, Tuple

from . import EDGE_KINDS, EFFECTS, EXEC_SCHEMA, GATE_AUTHORITIES, JOINS, NODE_KINDS, OUTCOMES
from . import binding
from . import capabilities
from .predicates import validate_all

ERROR_CODES = ("invalid_schema", "duplicate_node", "unknown_endpoint", "missing_entry", "unreachable_node", "missing_terminal", "terminal_outgoing_edge", "invalid_outcome", "invalid_repeat_edge", "unbounded_cycle", "unknown_subgraph", "recursive_subgraph", "invalid_fact_reference", "invalid_plan_task_reference", "ambiguous_routes", "invalid_effect", "invalid_kind", "invalid_join", "invalid_command", "invalid_budget", "invalid_gate", "invalid_task_binding", "unknown_task_binding", "duplicate_task_binding_writer", "unreachable_task_binding", "invalid_context", "unknown_tool_capability")
WARNING_CODES = ("unrouted_outcome", "dead_end", "unverified_effect_declaration")


def validate_spec(spec: Mapping[str, Any], *, plan_tasks: Optional[Sequence[str]] = None) -> Dict[str, Any]:
    """Return {"ok": bool, "errors": [{"code","where","message"}], "warnings": [...]}."""
    errors: List[Dict[str, str]] = []
    warnings: List[Dict[str, str]] = []
    seen_items: Set[Tuple[str, str, str]] = set()

    def add(target: List[Dict[str, str]], code: str, where: str, message: str) -> None:
        key = (code, where, message)
        if key not in seen_items:
            seen_items.add(key)
            target.append({"code": code, "where": where, "message": message})

    def node_map(raw: Any, prefix: str) -> Dict[str, Mapping[str, Any]]:
        result: Dict[str, Mapping[str, Any]] = {}
        if isinstance(raw, Mapping):
            for raw_id, value in raw.items():
                node_id = str(raw_id)
                result[node_id] = value if isinstance(value, Mapping) else {}
            return result
        if isinstance(raw, (list, tuple)):
            for index, value in enumerate(raw):
                if not isinstance(value, Mapping):
                    add(errors, "invalid_kind", "%s.nodes[%d]" % (prefix, index), "node must be an object")
                    continue
                node_id = str(value.get("id", ""))
                if node_id in result:
                    add(errors, "duplicate_node", "%s.nodes[%d]" % (prefix, index), "duplicate node id %s" % node_id)
                else:
                    result[node_id] = value
            return result
        add(errors, "invalid_kind", "%s.nodes" % prefix, "nodes must be an object")
        return result

    def malformed_facts(value: Any, where: str) -> None:
        if not isinstance(value, (list, tuple)) or any(not isinstance(item, str) for item in value):
            add(errors, "invalid_fact_reference", where, "fact predicates must be a list of strings")
            return
        for predicate, message in validate_all(value).items():
            add(errors, "invalid_fact_reference", where, "%s: %s" % (predicate, message))

    def check_graph(raw: Any, prefix: str, inherited_schema: Any, nested: bool) -> None:
        if not isinstance(raw, Mapping):
            add(errors, "invalid_schema", prefix, "graph must be an object")
            return
        schema = raw.get("schema", inherited_schema)
        if isinstance(schema, bool) or schema != EXEC_SCHEMA:
            add(errors, "invalid_schema", "%s.schema" % prefix, "schema must be %d" % EXEC_SCHEMA)

        nodes = node_map(raw.get("nodes", {}), prefix)
        edges_raw = raw.get("edges", [])
        edges = list(edges_raw) if isinstance(edges_raw, (list, tuple)) else []
        if not isinstance(edges_raw, (list, tuple)):
            add(errors, "unknown_endpoint", "%s.edges" % prefix, "edges must be a list")

        entry = raw.get("entry")
        if not isinstance(entry, str) or entry not in nodes:
            add(errors, "missing_entry", "%s.entry" % prefix, "entry must name an existing node")

        budget = raw.get("budget", {})
        if not isinstance(budget, Mapping):
            add(errors, "invalid_budget", "%s.budget" % prefix, "budget must be an object")
            budget = {}
        max_steps = budget.get("max_steps", 20)
        max_repeats = budget.get("max_repeats", 3)
        if isinstance(max_steps, bool) or not isinstance(max_steps, int) or max_steps <= 0:
            add(errors, "invalid_budget", "%s.budget.max_steps" % prefix, "max_steps must be a positive integer")
        if isinstance(max_repeats, bool) or not isinstance(max_repeats, int) or max_repeats < 0:
            add(errors, "invalid_budget", "%s.budget.max_repeats" % prefix, "max_repeats must be a non-negative integer")
        malformed_facts(raw.get("stop_facts", []), "%s.stop_facts" % prefix)

        terminals = []
        subgraphs = raw.get("subgraphs", {})
        if not isinstance(subgraphs, Mapping):
            subgraphs = {}
        allowed_tasks = set(str(item) for item in plan_tasks) if plan_tasks is not None else None
        for node_id, node in nodes.items():
            where = "%s.nodes.%s" % (prefix, node_id)
            kind = node.get("kind")
            if kind not in NODE_KINDS:
                add(errors, "invalid_kind", "%s.kind" % where, "unknown node kind %s" % kind)
            if kind == "terminal":
                terminals.append(node_id)
            effect = node.get("effect", "read")
            if kind in ("agent", "tool") and effect not in EFFECTS:
                add(errors, "invalid_effect", "%s.effect" % where, "unknown effect %s" % effect)
            join = node.get("join", "all")
            if join not in JOINS:
                add(errors, "invalid_join", "%s.join" % where, "join must be all or any")
            malformed_facts(node.get("requires", []), "%s.requires" % where)
            if kind in ("agent", "tool"):
                malformed_facts(node.get("proof", []), "%s.proof" % where)
            if kind == "agent":
                task = node.get("plan_task")
                source = node.get("plan_task_from")
                has_task = isinstance(task, str) and bool(task)
                has_source = source is not None
                if has_task and has_source:
                    add(errors, "invalid_task_binding", where, "plan_task and plan_task_from are mutually exclusive")
                elif not has_task and not has_source:
                    add(errors, "invalid_plan_task_reference", "%s.plan_task" % where, "agent node must name a plan task or a plan_task_from binding")
                elif has_task and allowed_tasks is not None and not binding.is_selector(task) and task not in allowed_tasks:
                    add(errors, "invalid_plan_task_reference", "%s.plan_task" % where, "unknown plan task %s" % task)
                if has_source and not binding.valid_name(source):
                    add(errors, "invalid_task_binding", "%s.plan_task_from" % where, "plan_task_from must be a binding identifier")
                bound = node.get("bind_task")
                if bound is not None and not binding.valid_name(bound):
                    add(errors, "invalid_task_binding", "%s.bind_task" % where, "bind_task must be a binding identifier")
                if node.get("mode", "run") not in ("run", "land"):
                    add(errors, "invalid_plan_task_reference", "%s.mode" % where, "agent mode must be run or land")
                context = node.get("context")
                if context is not None:
                    if not isinstance(context, Mapping):
                        add(errors, "invalid_context", "%s.context" % where, "context must be an object")
                    else:
                        if not isinstance(context.get("task", ""), str):
                            add(errors, "invalid_context", "%s.context.task" % where, "context.task must be a string")
                        max_files = context.get("max_files", 12)
                        if isinstance(max_files, bool) or not isinstance(max_files, int) or max_files <= 0 or max_files > 200:
                            add(errors, "invalid_context", "%s.context.max_files" % where, "context.max_files must be an integer from 1 to 200")
                        unknown = sorted(set(context) - {"task", "max_files"})
                        if unknown:
                            add(errors, "invalid_context", "%s.context" % where, "unknown context fields: %s" % ", ".join(unknown))
            elif node.get("bind_task") is not None:
                add(errors, "invalid_task_binding", "%s.bind_task" % where, "only an agent node may bind a task")
            if kind == "tool":
                has_command = isinstance(node.get("command"), str) and bool(node.get("command", "").strip())
                if capabilities.is_capability_node(node):
                    for code, field, message in capabilities.validate_node(node):
                        add(errors, code, "%s.%s" % (where, field), message)
                elif node.get("tool") is not None:
                    add(errors, "unknown_tool_capability", "%s.tool" % where, "tool must name a capability: %s" % ", ".join(capabilities.names()))
                elif not has_command:
                    add(errors, "invalid_command", "%s.command" % where, "tool command must be a non-empty string, or tool must name a capability")
                elif effect == "read":
                    # Nothing verifies that shell text honours a read
                    # declaration; a capability node carries that guarantee.
                    add(warnings, "unverified_effect_declaration", "%s.command" % where,
                        "a command node's effect is declared, not verified; prefer a tool capability")
            if kind == "gate":
                decisions = node.get("decisions", ["approved", "changes_requested"])
                if (node.get("authority", "parent") not in GATE_AUTHORITIES or
                        not isinstance(decisions, (list, tuple)) or not decisions or
                        any(item not in OUTCOMES for item in decisions)):
                    add(errors, "invalid_gate", where, "gate authority or decisions are invalid")
            if kind == "subgraph":
                graph_name = node.get("graph")
                if not isinstance(graph_name, str) or graph_name not in subgraphs:
                    add(errors, "unknown_subgraph", "%s.graph" % where, "subgraph node names an unknown graph")
                if nested:
                    add(errors, "recursive_subgraph", where, "subgraphs may only be one level deep")

        if not terminals:
            add(errors, "missing_terminal", "%s.nodes" % prefix, "graph has no terminal node")

        adjacency: Dict[str, List[str]] = {node: [] for node in nodes}
        outgoing: Dict[str, int] = {node: 0 for node in nodes}
        route_groups: Dict[Tuple[str, str, int], List[Mapping[str, Any]]] = {}
        for index, raw_edge in enumerate(edges):
            where = "%s.edges[%d]" % (prefix, index)
            if not isinstance(raw_edge, Mapping):
                add(errors, "unknown_endpoint", where, "edge must be an object")
                continue
            source = raw_edge.get("from")
            target = raw_edge.get("to")
            if source not in nodes or target not in nodes:
                add(errors, "unknown_endpoint", where, "edge endpoint does not exist")
                continue
            outgoing[str(source)] += 1
            adjacency[str(source)].append(str(target))
            if nodes[str(source)].get("kind") == "terminal":
                add(errors, "terminal_outgoing_edge", where, "terminal nodes cannot have outgoing edges")
            outcomes = raw_edge.get("outcomes")
            if (not isinstance(outcomes, (list, tuple)) or not outcomes or
                    any(item not in OUTCOMES for item in outcomes)):
                add(errors, "invalid_outcome", "%s.outcomes" % where, "edge outcomes must be a non-empty list of known outcomes")
                outcomes = []
            kind = raw_edge.get("kind", "normal")
            if kind not in EDGE_KINDS:
                add(errors, "invalid_repeat_edge", "%s.kind" % where, "edge kind must be normal or repeat")
            priority = raw_edge.get("priority", 0)
            if isinstance(priority, bool) or not isinstance(priority, int):
                add(errors, "invalid_repeat_edge", "%s.priority" % where, "edge priority must be an integer")
                priority = 0
            if not isinstance(raw_edge.get("fanout", False), bool):
                add(errors, "ambiguous_routes", "%s.fanout" % where, "fanout must be boolean")
            malformed_facts(raw_edge.get("when", []), "%s.when" % where)
            for outcome in outcomes:
                route_groups.setdefault((str(source), str(outcome), priority), []).append(raw_edge)

        for (source, outcome, priority), group in sorted(route_groups.items()):
            if len(group) > 1 and not all(edge.get("fanout") is True for edge in group):
                add(errors, "ambiguous_routes", "%s.edges" % prefix,
                    "multiple routes from %s for %s at priority %d require fanout" % (source, outcome, priority))

        if entry in nodes:
            reachable: Set[str] = set()
            pending = [str(entry)]
            while pending:
                current = pending.pop()
                if current in reachable:
                    continue
                reachable.add(current)
                pending.extend(adjacency.get(current, ()))
            for node_id in sorted(set(nodes) - reachable):
                add(errors, "unreachable_node", "%s.nodes.%s" % (prefix, node_id), "node is unreachable from entry")

        # Task bindings: one writer per name (a repeating node may rewrite its
        # own binding), every reader names a written binding, and the reader
        # must be reachable from that writer — a binding is only ever set by
        # an attempt that ran before the reader.
        graph_view = {"nodes": nodes}
        binding_writers = binding.writers(graph_view)
        binding_readers = binding.readers(graph_view)
        for name in sorted(binding_writers):
            if len(binding_writers[name]) > 1:
                add(errors, "duplicate_task_binding_writer", "%s.nodes" % prefix,
                    "binding %s is written by more than one node: %s" % (name, ", ".join(binding_writers[name])))
        for node_id, node in nodes.items():
            if capabilities.is_capability_node(node) and node.get("tool") == "commit_bound":
                bound = node.get("binding")
                if binding.valid_name(bound) and bound not in binding_writers:
                    add(errors, "unknown_task_binding", "%s.nodes.%s.binding" % (prefix, node_id), "no node binds %s" % bound)
        for name in sorted(binding_readers):
            for reader in binding_readers[name]:
                where = "%s.nodes.%s.plan_task_from" % (prefix, reader)
                if name not in binding_writers:
                    add(errors, "unknown_task_binding", where, "no node binds %s" % name)
                    continue
                writer_reach: Set[str] = set()
                pending = list(adjacency.get(binding_writers[name][0], ()))
                while pending:
                    current = pending.pop()
                    if current in writer_reach:
                        continue
                    writer_reach.add(current)
                    pending.extend(adjacency.get(current, ()))
                if reader not in writer_reach:
                    add(errors, "unreachable_task_binding", where,
                        "%s is not reachable from the node that binds %s" % (reader, name))

        stop_facts = raw.get("stop_facts", [])
        if not stop_facts:
            reachability: Dict[str, Set[str]] = {}
            for start in nodes:
                reached: Set[str] = set()
                pending = list(adjacency.get(start, ()))
                while pending:
                    current = pending.pop()
                    if current in reached:
                        continue
                    reached.add(current)
                    pending.extend(adjacency.get(current, ()))
                reachability[start] = reached
            remaining = set(nodes)
            while remaining:
                seed = min(remaining)
                component = {node for node in remaining
                             if node == seed or (node in reachability[seed] and seed in reachability[node])}
                remaining -= component
                cyclic = len(component) > 1 or seed in adjacency.get(seed, ())
                bounded = any(
                    isinstance(edge, Mapping) and edge.get("kind", "normal") == "repeat" and
                    edge.get("from") in component and edge.get("to") in component
                    for edge in edges
                )
                if cyclic and not bounded:
                    add(errors, "unbounded_cycle", "%s.edges" % prefix, "a cycle has no repeat edge or stop_facts policy")
                    break

        for index, raw_edge in enumerate(edges):
            if not isinstance(raw_edge, Mapping) or raw_edge.get("kind", "normal") != "repeat":
                continue
            source = raw_edge.get("from")
            target = raw_edge.get("to")
            if source not in nodes or target not in nodes:
                continue
            pending = [str(target)]
            reached: Set[str] = set()
            while pending:
                current = pending.pop()
                if current in reached:
                    continue
                reached.add(current)
                pending.extend(adjacency.get(current, ()))
            if source not in reached:
                add(errors, "invalid_repeat_edge", "%s.edges[%d]" % (prefix, index), "repeat edge must close a cycle")

        for node_id, node in nodes.items():
            if node.get("kind") == "terminal":
                continue
            if not outgoing.get(node_id):
                add(warnings, "dead_end", "%s.nodes.%s" % (prefix, node_id), "non-terminal node has no outgoing edge")
                continue
            routed = set()
            for edge in edges:
                if isinstance(edge, Mapping) and edge.get("from") == node_id and isinstance(edge.get("outcomes"), (list, tuple)):
                    routed.update(edge.get("outcomes", []))
            missing = [outcome for outcome in ("failed", "unverified") if outcome not in routed]
            if missing:
                add(warnings, "unrouted_outcome", "%s.nodes.%s" % (prefix, node_id), "unrouted outcomes: %s" % ", ".join(missing))

        for name in sorted(subgraphs):
            child = subgraphs[name]
            if not isinstance(child, Mapping):
                add(errors, "unknown_subgraph", "%s.subgraphs.%s" % (prefix, name), "subgraph must be an object")
            else:
                check_graph(child, "%s.subgraphs.%s" % (prefix, name), schema, True)

    check_graph(spec, "spec", EXEC_SCHEMA, False)
    errors.sort(key=lambda item: (item["where"], item["code"], item["message"]))
    warnings.sort(key=lambda item: (item["where"], item["code"], item["message"]))
    return {"ok": not errors, "errors": errors, "warnings": warnings}
