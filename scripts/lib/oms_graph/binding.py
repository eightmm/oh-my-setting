"""Run-scoped task bindings: selector versus identity (Graph Runtime v2).

`plan_task: "next"` is a *selector*: it names a rule for choosing a task, not a
task. The moment an attempt resolves it, the concrete id is recorded on the
`node_started` row (`task_id`, and `binding` when the node declares
`bind_task`), and every downstream node that says `plan_task_from: <name>`
executes exactly that task — even after the plan's own `next` has moved on.

`events.project()` folds the rows into `projection["bindings"]`; nothing here
persists a second copy. The helpers are pure so the validator, the route
evaluator, the scheduler, and the runner all read one definition of the
`binding.<name>.*` fact namespace and of what a concrete node looks like.
"""

from __future__ import annotations

import re
from typing import Any, Dict, List, Mapping, Optional

from .errors import GraphError

BINDING_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_-]{0,63}$")
SELECTOR_NEXT = "next"
FACT_PREFIX = "binding."


def is_selector(task: Any) -> bool:
    return str(task or "") == SELECTOR_NEXT


def valid_name(name: Any) -> bool:
    return isinstance(name, str) and bool(BINDING_RE.fullmatch(name))


def writers(spec: Mapping[str, Any]) -> Dict[str, List[str]]:
    """Binding name -> agent node ids that declare `bind_task` for it."""
    result: Dict[str, List[str]] = {}
    nodes = spec.get("nodes", {})
    if not isinstance(nodes, Mapping):
        return result
    for node_id in sorted(nodes, key=str):
        node = nodes[node_id]
        if not isinstance(node, Mapping) or node.get("kind") != "agent":
            continue
        name = node.get("bind_task")
        if isinstance(name, str) and name:
            result.setdefault(name, []).append(str(node_id))
    return result


def readers(spec: Mapping[str, Any]) -> Dict[str, List[str]]:
    """Binding name -> agent node ids that resolve their task through it."""
    result: Dict[str, List[str]] = {}
    nodes = spec.get("nodes", {})
    if not isinstance(nodes, Mapping):
        return result
    for node_id in sorted(nodes, key=str):
        node = nodes[node_id]
        if not isinstance(node, Mapping) or node.get("kind") != "agent":
            continue
        name = node.get("plan_task_from")
        if isinstance(name, str) and name:
            result.setdefault(name, []).append(str(node_id))
    return result


def bound_task(projection: Mapping[str, Any], name: str) -> str:
    """The concrete task a binding currently holds, or '' when unbound."""
    bindings = projection.get("bindings", {}) if isinstance(projection, Mapping) else {}
    entry = bindings.get(name) if isinstance(bindings, Mapping) else None
    if not isinstance(entry, Mapping):
        return ""
    return str(entry.get("task_id", "") or "")


def concrete_key(key: str, name: str, task_id: str) -> str:
    """`binding.<name>.X` -> the plan/receipt fact key it stands for.

    `binding.<name>.receipt.<kind>.<rest>` is `receipt.<kind>.<task>.<rest>`;
    every other field is a `plan.task.<task>.<field>` projection. Keys that
    name another binding, or no binding, are returned unchanged.
    """
    prefix = FACT_PREFIX + name + "."
    if not key.startswith(prefix):
        return key
    rest = key[len(prefix):]
    if rest.startswith("receipt."):
        parts = rest.split(".", 2)
        if len(parts) == 3:
            return "receipt.%s.%s.%s" % (parts[1], task_id, parts[2])
        return key
    if rest == "task_id":
        return key
    return "plan.task.%s.%s" % (task_id, rest)


def concrete_predicate(predicate: str, name: str, task_id: str) -> str:
    """Rewrite one proof/requires predicate onto the concrete task's keys."""
    text = str(predicate or "")
    for operator in ("!=", "="):
        if operator in text:
            key, value = text.split(operator, 1)
            return concrete_key(key.strip(), name, task_id) + operator + value
    if text.startswith("!"):
        return "!" + concrete_key(text[1:].strip(), name, task_id)
    return concrete_key(text.strip(), name, task_id)


def augment_binding_facts(spec: Mapping[str, Any], projection: Mapping[str, Any], facts: Mapping[str, Any]) -> Dict[str, Any]:
    """Derive `binding.<name>.*` from the projection and the current facts.

    Not a store: every evaluation recomputes it, so a binding can never carry
    a fact the control plane no longer holds.
    """
    result = dict(facts)
    bindings = projection.get("bindings", {}) if isinstance(projection, Mapping) else {}
    if not isinstance(bindings, Mapping):
        return result
    for name in sorted(bindings, key=str):
        entry = bindings[name]
        if not isinstance(entry, Mapping):
            continue
        task_id = str(entry.get("task_id", "") or "")
        if not task_id:
            continue
        result[FACT_PREFIX + name + ".task_id"] = task_id
        plan_prefix = "plan.task.%s." % task_id
        for key, value in facts.items():
            if key.startswith(plan_prefix):
                result[FACT_PREFIX + name + "." + key[len(plan_prefix):]] = value
                continue
            parts = key.split(".")
            if len(parts) >= 4 and parts[0] == "receipt" and parts[2] == task_id:
                result[".".join([FACT_PREFIX + name, "receipt", parts[1]] + parts[3:])] = value
    return result


def effective_node(node: Mapping[str, Any], task_id: str) -> Dict[str, Any]:
    """The concrete node the adapter sees: `plan_task` is the resolved id and
    the proof predicates name that id, so the adapter never learns what a
    binding or a selector is."""
    if not task_id or is_selector(task_id):
        raise GraphError("an agent node needs a concrete plan task before it runs")
    concrete = dict(node)
    concrete["plan_task"] = task_id
    name = str(node.get("bind_task") or node.get("plan_task_from") or "")
    concrete.pop("plan_task_from", None)
    if name:
        for field in ("proof", "requires"):
            declared = node.get(field) or []
            concrete[field] = [concrete_predicate(str(item), name, task_id) for item in declared]
    return concrete


def resolve_static(node: Mapping[str, Any], projection: Mapping[str, Any]) -> Optional[str]:
    """A node's concrete task when no selector is involved: the literal
    `plan_task`, or the task its `plan_task_from` binding holds. `None` for a
    selector or an unbound reference."""
    source = node.get("plan_task_from")
    if isinstance(source, str) and source:
        return bound_task(projection, source) or None
    task = str(node.get("plan_task", "") or "")
    if not task or is_selector(task):
        return None
    return task
