"""Eligibility and write-scope conflicts (W3).

Pure functions. The route evaluator answers "what is legal next?"; this module
answers "which of those may start right now without two writers colliding?".
Both answers stay advisory: `patch-admit` enforces scope and `patch-land`
serializes landing regardless of what is scheduled here.

Overlap is deliberately conservative — an unknown write scope conflicts with
everything — because a false "no overlap" costs a corrupted tree while a false
"overlap" costs only serialization.

Task identity is an input, never derived here: the runner resolves every
agent node to a concrete plan task (a literal id, a binding, or one read-only
peek at the plan's `next`) and passes `resolved_tasks`. Two nodes on the same
task never share a wave — the lifecycle lease is one resource whatever their
path scopes say — and at most one node whose task came from a dynamic
selector runs per wave, because two peeks can return the same task.
"""

from __future__ import annotations

from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

import path_scope

from . import predicates
from .errors import GraphError

GLOB_CHARS = "*?["


def _static_prefix(pattern: str) -> str:
    """Directory text before the first glob character, trimmed to its last '/'.

    ``src/lib/*.py`` -> ``src/lib``; ``src/li*b/x`` -> ``src``; ``*.py`` -> ``''``
    (an empty prefix can select anywhere and therefore overlaps everything).
    """
    cut = len(pattern)
    for char in GLOB_CHARS:
        index = pattern.find(char)
        if 0 <= index < cut:
            cut = index
    head = pattern[:cut]
    slash = head.rfind("/")
    return head[:slash] if slash >= 0 else ""


def _literal_overlap(one: str, two: str) -> bool:
    """Equal, or one literal path is a directory prefix of the other."""
    return path_scope.matches(one, two) or path_scope.matches(two, one)


def _prefix_overlap(one: str, two: str) -> bool:
    """Static-prefix comparison; an empty prefix diverges from nothing."""
    if not one or not two:
        return True
    return _literal_overlap(one, two)


def _patterns_overlap(one: str, two: str) -> bool:
    one_glob = path_scope.has_glob(one)
    two_glob = path_scope.has_glob(two)
    if not one_glob and not two_glob:
        return _literal_overlap(one, two)
    if one_glob and two_glob:
        return _prefix_overlap(_static_prefix(one), _static_prefix(two))
    literal, glob = (two, one) if one_glob else (one, two)
    if path_scope.matches(literal, glob):
        return True
    return _prefix_overlap(literal, _static_prefix(glob))


def scopes_overlap(a: Sequence[str], b: Sequence[str]) -> bool:
    """Whether two write scopes may touch the same path. Unknown means yes."""
    if not a or not b:
        return True
    try:
        left = [path_scope.normalize(str(item)) for item in a]
        right = [path_scope.normalize(str(item)) for item in b]
    except (ValueError, TypeError):
        return True
    for one in left:
        for two in right:
            try:
                if _patterns_overlap(one, two):
                    return True
            except (ValueError, TypeError):
                return True
    return False


def _node_of(spec: Mapping[str, Any], node_id: str) -> Optional[Mapping[str, Any]]:
    nodes = spec.get("nodes")
    if not isinstance(nodes, Mapping):
        return None
    node = nodes.get(node_id)
    return node if isinstance(node, Mapping) else None


def _is_exclusive(node: Mapping[str, Any]) -> bool:
    """Landing and write-capable tools run alone; nothing else joins them."""
    kind = str(node.get("kind", ""))
    effect = str(node.get("effect", "read"))
    if kind == "agent" and str(node.get("mode", "run") or "run") == "land":
        return True
    return kind == "tool" and effect == "write"


def _write_scope(
    node: Mapping[str, Any], task_scopes: Mapping[str, Mapping[str, Sequence[str]]], task_id: str
) -> Tuple[bool, Optional[List[str]]]:
    """Return (is_write, allowed_scope); ``None`` scope means unknown."""
    if str(node.get("effect", "read")) != "write":
        return False, []
    if str(node.get("kind", "")) != "agent":
        # A write-capable tool declares no plan scope; it is exclusive instead.
        return True, None
    entry = task_scopes.get(task_id) if task_id else None
    if not isinstance(entry, Mapping):
        return True, None
    allowed = entry.get("allowed")
    if not isinstance(allowed, (list, tuple)) or not allowed:
        return True, None
    return True, [str(item) for item in allowed]


def _requires_reason(node: Mapping[str, Any], facts: Mapping[str, Any]) -> str:
    declared = node.get("requires") or []
    if not isinstance(declared, (list, tuple)):
        return "requires:invalid"
    try:
        unmet = predicates.missing(list(declared), facts)
    except GraphError:
        return "requires:invalid"
    return "requires:%s" % unmet[0] if unmet else ""


def eligible(
    spec: Mapping[str, Any],
    state: Mapping[str, Any],
    facts: Mapping[str, Any],
    *,
    route: Mapping[str, Any],
    task_scopes: Mapping[str, Mapping[str, Sequence[str]]],
    capacity: int = 1,
    active: Sequence[str] = (),
    resolved_tasks: Optional[Mapping[str, str]] = None,
    selectors: Sequence[str] = (),
) -> Dict[str, Any]:
    """Which routed candidates may start now, and why the others may not.

    `resolved_tasks` maps node id -> concrete plan task for candidates and
    active nodes; `selectors` lists the candidates whose task came from a
    dynamic selector this wave.
    """
    alternatives = route.get("alternatives") or []
    if not isinstance(alternatives, (list, tuple)):
        alternatives = []
    candidates: List[str] = []
    for item in [route.get("primary")] + list(alternatives):
        name = str(item or "")
        if name and name not in candidates:
            candidates.append(name)

    eligible_nodes: List[str] = []
    deferred: List[Dict[str, str]] = []
    conflicts: List[Dict[str, str]] = []

    status = str(route.get("status", ""))
    if status != "actionable":
        reason = "route:%s" % (status or "unknown")
        for name in candidates or [""]:
            deferred.append({"node": name, "reason": reason})
        return {"eligible": eligible_nodes, "deferred": deferred, "conflicts": conflicts}

    tasks = {str(key): str(value) for key, value in (resolved_tasks or {}).items() if str(value or "")}
    selector_nodes = {str(item) for item in selectors}

    def task_of(name: str, node: Mapping[str, Any]) -> str:
        if name in tasks:
            return tasks[name]
        literal = str(node.get("plan_task", "") or "")
        return "" if not literal or literal == "next" else literal

    active_ids = [str(item) for item in active if str(item)]
    # An active write node constrains every later selection; an active node the
    # spec does not describe is treated as an unknown writer, not as absent.
    held: List[Tuple[str, Optional[List[str]]]] = []
    held_tasks: Dict[str, str] = {}
    for name in active_ids:
        node = _node_of(spec, name)
        if node is None:
            held.append((name, None))
            continue
        task_id = task_of(name, node)
        if task_id:
            held_tasks[task_id] = name
        is_write, scope = _write_scope(node, task_scopes, task_id)
        if is_write:
            held.append((name, scope))

    limit = capacity if isinstance(capacity, int) and capacity > 0 else 1
    exclusive_selected = False
    selector_selected = any(name in selector_nodes for name in active_ids)
    selected_scopes: List[Tuple[str, Optional[List[str]]]] = []
    selected_tasks: Dict[str, str] = {}

    for name in candidates:
        node = _node_of(spec, name)
        if node is None:
            deferred.append({"node": name, "reason": "unknown-node"})
            continue
        if str(node.get("kind", "")) == "gate":
            deferred.append({"node": name, "reason": "gate"})
            continue
        unmet = _requires_reason(node, facts)
        if unmet:
            deferred.append({"node": name, "reason": unmet})
            continue
        if exclusive_selected:
            deferred.append({"node": name, "reason": "exclusive"})
            continue

        task_id = task_of(name, node)
        is_agent = str(node.get("kind", "")) == "agent"
        if is_agent and task_id:
            other = held_tasks.get(task_id) or selected_tasks.get(task_id)
            if other:
                conflicts.append({"node": name, "with": other, "reason": "same-task"})
                continue
        if name in selector_nodes and selector_selected:
            deferred.append({"node": name, "reason": "dynamic-selector-exclusive"})
            continue

        is_write, scope = _write_scope(node, task_scopes, task_id)
        exclusive = _is_exclusive(node)
        # A write-capable tool declares no plan scope by design and is fenced by
        # exclusivity instead. An agent node without a scope is a real unknown.
        if is_write and scope is None and is_agent:
            conflicts.append({"node": name, "with": "", "reason": "unknown-scope"})
            continue
        if exclusive and (active_ids or eligible_nodes):
            deferred.append({"node": name, "reason": "exclusive"})
            continue

        if is_write and scope is not None:
            blocker = ""
            for other, other_scope in held + selected_scopes:
                if scopes_overlap(scope, other_scope if other_scope is not None else []):
                    blocker = other
                    break
            if blocker:
                conflicts.append({"node": name, "with": blocker, "reason": "scope-overlap"})
                continue

        if len(eligible_nodes) >= limit:
            deferred.append({"node": name, "reason": "capacity"})
            continue

        eligible_nodes.append(name)
        if is_agent and task_id:
            selected_tasks[task_id] = name
        if name in selector_nodes:
            selector_selected = True
        if is_write:
            selected_scopes.append((name, scope))
        if exclusive:
            exclusive_selected = True

    return {"eligible": eligible_nodes, "deferred": deferred, "conflicts": conflicts}
