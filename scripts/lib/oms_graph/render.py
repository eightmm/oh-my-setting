"""Text and mermaid renderers for both graphs (W-G).

Pure formatting over plain dicts: an execution GraphSpec plus its optional
events projection and route, or project-graph nodes and edges. Nothing here
reads disk, and every listing is ordered deterministically so two renders of
the same input are byte-identical.
"""

from __future__ import annotations

from collections import deque
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .project.analytics import degrees as _degrees

GLYPH_DONE = "✔"
GLYPH_FAILED = "✖"
GLYPH_ACTIVE = "◇"
GLYPH_GATE = "⏸"
GLYPH_PENDING = "○"
REPEAT_MARK = "↻"

_DONE_OUTCOMES = ("completed", "approved")
_FAILED_OUTCOMES = ("failed", "blocked", "unverified")


def _spec_nodes(spec: Mapping[str, Any]) -> Dict[str, Mapping[str, Any]]:
    raw = spec.get("nodes")
    nodes = {}  # type: Dict[str, Mapping[str, Any]]
    if isinstance(raw, dict):
        for node_id, node in raw.items():
            if isinstance(node_id, str) and node_id:
                nodes[node_id] = node if isinstance(node, dict) else {}
    return nodes


def _spec_edges(spec: Mapping[str, Any], nodes: Mapping[str, Any]) -> List[Dict[str, Any]]:
    """Edges with both endpoints known, ordered by (priority, from, to, outcomes)."""
    raw = spec.get("edges")
    rows = []  # type: List[Dict[str, Any]]
    if not isinstance(raw, list):
        return rows
    for edge in raw:
        if not isinstance(edge, dict):
            continue
        source = edge.get("from")
        target = edge.get("to")
        if not isinstance(source, str) or not isinstance(target, str):
            continue
        if source not in nodes or target not in nodes:
            continue
        outcomes = edge.get("outcomes")
        outcomes = [str(item) for item in outcomes] if isinstance(outcomes, list) else []
        try:
            priority = int(edge.get("priority") or 0)
        except (TypeError, ValueError):
            priority = 0
        rows.append({
            "from": source,
            "to": target,
            "outcomes": outcomes,
            "kind": edge.get("kind") if edge.get("kind") in ("normal", "repeat") else "normal",
            "priority": priority,
        })
    rows.sort(key=lambda row: (row["priority"], row["from"], row["to"], ",".join(row["outcomes"])))
    return rows


def _entry(spec: Mapping[str, Any]) -> str:
    entry = spec.get("entry")
    return entry if isinstance(entry, str) else ""


def _render_order(nodes: Mapping[str, Any], edges: Sequence[Mapping[str, Any]], entry: str) -> List[str]:
    """Topological order from `entry`, ties by id. Cycles are legal in a spec,
    so when no node is free of incoming edges the walk releases the remaining
    node closest to `entry` (ties by id) and continues."""
    ids = sorted(nodes)
    indegree = {node_id: 0 for node_id in ids}
    outgoing = {node_id: [] for node_id in ids}  # type: Dict[str, List[str]]
    for source, target in sorted({(edge["from"], edge["to"]) for edge in edges}):
        outgoing[source].append(target)
        indegree[target] += 1
    distance = {}  # type: Dict[str, int]
    if entry in indegree:
        distance[entry] = 0
        queue = deque([entry])
        while queue:
            node = queue.popleft()
            for target in outgoing[node]:
                if target not in distance:
                    distance[target] = distance[node] + 1
                    queue.append(target)
    unreachable = len(ids) + 1
    remaining = set(ids)
    order = []  # type: List[str]
    while remaining:
        if not order and entry in remaining:
            chosen = entry
        else:
            ready = [node_id for node_id in ids if node_id in remaining and indegree[node_id] == 0]
            if ready:
                chosen = ready[0]
            else:
                chosen = min(remaining, key=lambda node_id: (distance.get(node_id, unreachable), node_id))
        order.append(chosen)
        remaining.discard(chosen)
        for target in outgoing[chosen]:
            indegree[target] -= 1
    return order


def _node_state(projection: Optional[Mapping[str, Any]], node_id: str) -> Dict[str, Any]:
    if not isinstance(projection, dict):
        return {}
    nodes = projection.get("nodes")
    if not isinstance(nodes, dict):
        return {}
    state = nodes.get(node_id)
    return state if isinstance(state, dict) else {}


def _glyph(node_id: str, node: Mapping[str, Any], projection: Optional[Mapping[str, Any]], route: Optional[Mapping[str, Any]]) -> str:
    """Precedence: a finished node shows its outcome, an undecided gate that the
    run has reached shows the pause, then the active or routed node, then
    pending. A finished node whose outcome is neither a success nor a failure
    (`partial`, `skipped`) falls through to the later rules."""
    state = _node_state(projection, node_id)
    status = state.get("status")
    outcome = state.get("outcome")
    if status == "finished":
        if outcome in _DONE_OUTCOMES:
            return GLYPH_DONE
        if outcome in _FAILED_OUTCOMES:
            return GLYPH_FAILED
    gates = projection.get("gates") if isinstance(projection, dict) else None
    decided = isinstance(gates, dict) and node_id in gates
    is_primary = isinstance(route, dict) and route.get("primary") == node_id
    if node.get("kind") == "gate" and not decided and (status == "active" or is_primary):
        return GLYPH_GATE
    if status == "active" or is_primary:
        return GLYPH_ACTIVE
    return GLYPH_PENDING


def render_exec_text(spec: Mapping[str, Any], projection: Optional[Mapping[str, Any]] = None, route: Optional[Mapping[str, Any]] = None) -> str:
    nodes = _spec_nodes(spec)
    edges = _spec_edges(spec, nodes)
    entry = _entry(spec)
    outgoing = {}  # type: Dict[str, List[Mapping[str, Any]]]
    for edge in edges:
        outgoing.setdefault(edge["from"], []).append(edge)
    spec_id = spec.get("id")
    lines = ["graph %s (entry %s)" % (spec_id if isinstance(spec_id, str) and spec_id else "-", entry or "-")]
    if isinstance(route, dict) and route.get("status"):
        reason = route.get("reason")
        header = "route %s" % route.get("status")
        if isinstance(reason, str) and reason:
            header += ": %s" % reason
        lines.append(header)
    for node_id in _render_order(nodes, edges, entry):
        node = nodes[node_id]
        kind = node.get("kind")
        line = "%s %s (%s)" % (_glyph(node_id, node, projection, route), node_id, kind if isinstance(kind, str) and kind else "node")
        outcome = _node_state(projection, node_id).get("outcome")
        if isinstance(outcome, str) and outcome:
            line += " [%s]" % outcome
        title = node.get("title")
        if isinstance(title, str) and title and title != node_id:
            line += " — %s" % title
        if isinstance(route, dict) and route.get("primary") == node_id:
            line += "  ← next"
        lines.append(line)
        rows = outgoing.get(node_id, [])
        for position, edge in enumerate(rows):
            connector = "└──" if position == len(rows) - 1 else "├──"
            mark = " %s" % REPEAT_MARK if edge["kind"] == "repeat" else ""
            lines.append("  %s %s → %s%s" % (connector, ", ".join(edge["outcomes"]) or "-", edge["to"], mark))
    return "\n".join(lines) + "\n"


def _mermaid_ids(node_ids: Sequence[str]) -> Dict[str, str]:
    """Stable mermaid-safe identifiers; collisions after sanitising are suffixed
    in sorted order so the mapping never depends on input order."""
    mapping = {}  # type: Dict[str, str]
    taken = set()
    for node_id in sorted(node_ids):
        safe = "".join(char if (char.isascii() and (char.isalnum() or char == "_")) else "_" for char in node_id)
        if not safe or not (safe[0].isalpha() or safe[0] == "_"):
            safe = "n_" + safe
        candidate = safe
        suffix = 2
        while candidate in taken:
            candidate = "%s_%d" % (safe, suffix)
            suffix += 1
        taken.add(candidate)
        mapping[node_id] = candidate
    return mapping


def _label(text: str) -> str:
    return text.replace("\\", "/").replace('"', "'").replace("\n", " ")


def render_exec_mermaid(spec: Mapping[str, Any], projection: Optional[Mapping[str, Any]] = None) -> str:
    nodes = _spec_nodes(spec)
    edges = _spec_edges(spec, nodes)
    ids = _mermaid_ids(list(nodes))
    lines = ["flowchart TD"]
    for node_id in sorted(nodes):
        kind = nodes[node_id].get("kind")
        kind = kind if isinstance(kind, str) and kind else "node"
        lines.append('    %s["%s\\n(%s)"]' % (ids[node_id], _label(node_id), _label(kind)))
    for edge in edges:
        arrow = "-.->" if edge["kind"] == "repeat" else "-->"
        label = ", ".join(edge["outcomes"])
        source = ids[edge["from"]]
        target = ids[edge["to"]]
        if label:
            lines.append('    %s %s|"%s"| %s' % (source, arrow, _label(label), target))
        else:
            lines.append("    %s %s %s" % (source, arrow, target))
    if isinstance(projection, dict):
        buckets = {"done": [], "active": [], "failed": []}  # type: Dict[str, List[str]]
        for node_id in sorted(nodes):
            state = _node_state(projection, node_id)
            status = state.get("status")
            outcome = state.get("outcome")
            if status == "finished" and outcome in _DONE_OUTCOMES:
                buckets["done"].append(ids[node_id])
            elif status == "finished" and outcome in _FAILED_OUTCOMES:
                buckets["failed"].append(ids[node_id])
            elif status == "active":
                buckets["active"].append(ids[node_id])
        lines.append("    classDef done fill:#dff0d8,stroke:#3c763d,color:#1b1b1b;")
        lines.append("    classDef active fill:#fcf8e3,stroke:#8a6d3b,color:#1b1b1b;")
        lines.append("    classDef failed fill:#f2dede,stroke:#a94442,color:#1b1b1b;")
        for name in ("done", "active", "failed"):
            if buckets[name]:
                lines.append("    class %s %s;" % (",".join(buckets[name]), name))
    return "\n".join(lines) + "\n"


def _counts_block(title: str, counts: Mapping[str, Any]) -> List[str]:
    rows = []
    for key, value in counts.items():
        try:
            rows.append((str(key), int(value)))
        except (TypeError, ValueError):
            continue
    if not rows:
        return ["%s: (none)" % title]
    rows.sort(key=lambda row: (-row[1], row[0]))
    width = max(len(row[0]) for row in rows)
    lines = ["%s:" % title]
    for name, value in rows:
        lines.append("  %-*s  %d" % (width, name, value))
    return lines


def render_project_map_text(summary: Mapping[str, Any]) -> str:
    counts = summary.get("counts")
    counts = counts if isinstance(counts, dict) else {}
    revision = summary.get("revision")
    revision = revision if isinstance(revision, str) and revision else "unknown"
    lines = ["project graph (revision %s)" % revision]
    for label, key in (("nodes by kind", "kind"), ("nodes by language", "language")):
        block = counts.get(key)
        lines.extend(_counts_block(label, block if isinstance(block, dict) else {}))
    hub_rows = summary.get("hubs")
    hub_rows = hub_rows if isinstance(hub_rows, list) else []
    lines.append("top hubs:" if hub_rows else "top hubs: (none)")
    for position, row in enumerate(hub_rows, 1):
        if not isinstance(row, dict):
            continue
        lines.append("  %2d. %s  (%s)  degree %s" % (position, row.get("id", "?"), row.get("kind", "?"), row.get("degree", 0)))
    groups = summary.get("groups")
    groups = groups if isinstance(groups, dict) else {}
    ranked = []
    for name, members in groups.items():
        members = [str(item) for item in members] if isinstance(members, list) else []
        ranked.append((str(name), sorted(members)))
    ranked.sort(key=lambda row: (-len(row[1]), row[0]))
    lines.append("module groups:" if ranked else "module groups: (none)")
    for name, members in ranked:
        shown = ", ".join(members[:5])
        if len(members) > 5:
            shown += ", +%d more" % (len(members) - 5)
        lines.append("  %s (%d): %s" % (name, len(members), shown))
    return "\n".join(lines) + "\n"


def render_project_mermaid(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], *, limit: int = 200) -> str:
    """At most `limit` nodes, highest degree first with ties by id; only edges
    whose endpoints both survive that cut are drawn."""
    counts = _degrees(nodes, edges)
    index = {}  # type: Dict[str, Mapping[str, Any]]
    for node in nodes:
        if isinstance(node, dict) and isinstance(node.get("id"), str) and node["id"] not in index:
            index[node["id"]] = node
    ranked = sorted(index, key=lambda node_id: (-counts[node_id]["total"], node_id))
    selected = ranked[:limit] if limit >= 0 else ranked
    kept = set(selected)
    ids = _mermaid_ids(selected)
    lines = ["flowchart LR"]
    for node_id in sorted(kept):
        node = index[node_id]
        name = node.get("name")
        name = name if isinstance(name, str) and name else node_id
        lines.append('    %s["%s"]' % (ids[node_id], _label(name)))
    drawn = set()
    rows = []
    for edge in edges:
        if not isinstance(edge, dict):
            continue
        source = edge.get("source")
        target = edge.get("target")
        if source not in kept or target not in kept:
            continue
        relation = edge.get("relation")
        relation = relation if isinstance(relation, str) and relation else "related"
        key = (source, target, relation)
        if key in drawn:
            continue
        drawn.add(key)
        rows.append(key)
    for source, target, relation in sorted(rows):
        lines.append('    %s -->|"%s"| %s' % (ids[source], _label(relation), ids[target]))
    return "\n".join(lines) + "\n"
