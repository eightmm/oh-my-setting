"""Stdlib graph analytics: degrees, hubs, components, cycles, paths, communities (W-G).

Every function is pure: it reads only the node and edge sequences it is given,
never the disk, and returns results in a deterministic order. Edges whose
endpoints are not among the supplied nodes are ignored rather than rejected, so
a partial slice of a graph analyses cleanly.
"""

from __future__ import annotations

from collections import deque
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

# Ceilings on DFS expansions in `cycles`, so a dense graph cannot make the walk
# run away before `limit` cycles are found. Both are fixed, so runs stay
# reproducible; the per-anchor share stops one tangled node from spending the
# whole budget and starving every later anchor. A sparse graph explores each
# anchor exhaustively well inside these bounds.
_CYCLE_EXPANSION_BUDGET = 200000
_CYCLE_ANCHOR_BUDGET = 2000
ASSURANCE_BASIS = "structural-evidence"
ASSURANCE_LABELS = {
    "supported": "Test-linked",
    "needs-evidence": "Needs evidence",
    "attention": "Attention",
    "test-evidence": "Test evidence",
}
_ASSURANCE_TEST_PATH_LIMIT = 12


def _node_index(nodes: Sequence[Mapping[str, Any]]) -> Dict[str, Mapping[str, Any]]:
    index = {}  # type: Dict[str, Mapping[str, Any]]
    for node in nodes:
        if not isinstance(node, dict):
            continue
        node_id = node.get("id")
        if isinstance(node_id, str) and node_id and node_id not in index:
            index[node_id] = node
    return index


def _iter_edges(edges: Sequence[Mapping[str, Any]], index: Mapping[str, Any], relations: Sequence[str] = ()) -> List[Tuple[str, str]]:
    wanted = tuple(relations)
    pairs = []  # type: List[Tuple[str, str]]
    for edge in edges:
        if not isinstance(edge, dict):
            continue
        source = edge.get("source")
        target = edge.get("target")
        if not isinstance(source, str) or not isinstance(target, str):
            continue
        if source not in index or target not in index:
            continue
        if wanted and edge.get("relation") not in wanted:
            continue
        pairs.append((source, target))
    return pairs


def _adjacency(pairs: Sequence[Tuple[str, str]], index: Mapping[str, Any], *, undirected: bool) -> Dict[str, List[str]]:
    """Deduplicated neighbour lists, each sorted, for every known node."""
    seen = {node_id: set() for node_id in index}
    for source, target in pairs:
        seen[source].add(target)
        if undirected:
            seen[target].add(source)
    return {node_id: sorted(members) for node_id, members in seen.items()}


def _reverse_adjacency(pairs: Sequence[Tuple[str, str]], index: Mapping[str, Any]) -> Dict[str, List[str]]:
    seen = {node_id: set() for node_id in index}
    for source, target in pairs:
        seen[target].add(source)
    return {node_id: sorted(members) for node_id, members in seen.items()}


def degrees(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]]) -> Dict[str, Dict[str, int]]:
    index = _node_index(nodes)
    counts = {}  # type: Dict[str, Dict[str, int]]
    for node_id in sorted(index):
        counts[node_id] = {"in": 0, "out": 0, "total": 0}
    for source, target in _iter_edges(edges, index):
        counts[source]["out"] += 1
        counts[target]["in"] += 1
    for row in counts.values():
        row["total"] = row["in"] + row["out"]
    return counts


def _uncertain_site_key(edge: Mapping[str, Any], index: Mapping[str, Mapping[str, Any]]) -> Tuple[Any, ...]:
    """One unresolved source occurrence, independent of candidate fan-out."""
    evidence = edge.get("evidence") if isinstance(edge.get("evidence"), Mapping) else {}
    confidence = str(edge.get("confidence", "AMBIGUOUS") or "AMBIGUOUS")
    target = str(edge.get("target", ""))
    # Every candidate edge for one ambiguous name has the same source location
    # and target name. The name keeps two unresolved calls on one source line
    # distinct without copying candidate ids into every edge.
    discriminator = str(index.get(target, {}).get("name", "")) if confidence == "AMBIGUOUS" else target
    return (
        str(edge.get("source", "")), str(edge.get("relation", "")),
        str(evidence.get("path", "")), evidence.get("line"), discriminator,
    )


def evidence_degrees(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]]) -> Dict[str, Dict[str, int]]:
    """Degree used for ranking evidence, with ambiguous fan-out counted once.

    Candidate targets remain individually reachable and each keeps its incoming
    edge. Only the unresolved source site's outgoing count is collapsed, so a
    common symbol name cannot turn its caller into an artificial hub.
    """
    index = _node_index(nodes)
    counts = {node_id: {"in": 0, "out": 0, "total": 0} for node_id in sorted(index)}
    outgoing_seen = set()
    incoming_seen = set()
    for edge in edges:
        if not isinstance(edge, Mapping):
            continue
        source, target = edge.get("source"), edge.get("target")
        if source not in index or target not in index:
            continue
        confidence = str(edge.get("confidence", "AMBIGUOUS") or "AMBIGUOUS")
        evidence = edge.get("evidence") if isinstance(edge.get("evidence"), Mapping) else {}
        occurrence_key = (
            source, target, edge.get("relation"), confidence,
            evidence.get("path"), evidence.get("line"),
        )
        if confidence == "AMBIGUOUS":
            out_key = _uncertain_site_key(edge, index)
        else:
            out_key = occurrence_key
        in_key = occurrence_key
        if out_key not in outgoing_seen:
            outgoing_seen.add(out_key)
            counts[str(source)]["out"] += 1
        if in_key not in incoming_seen:
            incoming_seen.add(in_key)
            counts[str(target)]["in"] += 1
    for row in counts.values():
        row["total"] = row["in"] + row["out"]
    return counts


def structural_assurance(
    nodes: Sequence[Mapping[str, Any]],
    edges: Sequence[Mapping[str, Any]],
    *,
    include_tests: bool = False,
) -> Dict[str, Any]:
    """Canonical advisory assurance shared by CLI JSON and visual renderers."""
    index = _node_index(nodes)
    test_paths = {
        str(node.get("path", "")) for node in index.values()
        if str(node.get("kind", "")) == "test" and str(node.get("path", ""))
    }
    test_ids = {
        node_id for node_id, node in index.items()
        if str(node.get("kind", "")) == "test" or str(node.get("path", "")) in test_paths
    }
    path_tests = {}  # type: Dict[str, set]
    uncertain = {}  # type: Dict[str, Dict[str, set]]
    for edge in edges:
        if not isinstance(edge, Mapping):
            continue
        source, target = edge.get("source"), edge.get("target")
        if source not in index or target not in index:
            continue
        source = str(source); target = str(target)
        source_test, target_test = source in test_ids, target in test_ids
        confidence = str(edge.get("confidence", "AMBIGUOUS") or "AMBIGUOUS")
        if source_test != target_test and confidence == "EXTRACTED":
            production_id, test_id = (target, source) if source_test else (source, target)
            production_path = str(index[production_id].get("path", ""))
            test_path = str(index[test_id].get("path", ""))
            if production_path and test_path:
                path_tests.setdefault(production_path, set()).add(test_path)
        if not source_test and not target_test and confidence in ("INFERRED", "AMBIGUOUS"):
            bucket = uncertain.setdefault(source, {"INFERRED": set(), "AMBIGUOUS": set()})
            bucket[confidence].add(_uncertain_site_key(edge, index))

    projected = {}  # type: Dict[str, Dict[str, Any]]
    counts = {}  # type: Dict[str, int]
    for node_id in sorted(index):
        node = index[node_id]
        if node_id in test_ids:
            if not include_tests:
                continue
            row = {
                "assurance": "test-evidence",
                "assurance_label": ASSURANCE_LABELS["test-evidence"],
                "assurance_reason": "Explicitly included test node",
                "signals": {"extracted_test_links": 0, "extracted_test_paths": [],
                            "omitted_test_paths": 0, "inferred_sites": 0, "ambiguous_sites": 0},
            }
        else:
            path = str(node.get("path", ""))
            linked_tests = sorted(path_tests.get(path, set()))
            site_sets = uncertain.get(node_id, {"INFERRED": set(), "AMBIGUOUS": set()})
            inferred_sites = len(site_sets["INFERRED"])
            ambiguous_sites = len(site_sets["AMBIGUOUS"])
            signals = {
                "extracted_test_links": len(linked_tests),
                "extracted_test_paths": linked_tests[:_ASSURANCE_TEST_PATH_LIMIT],
                "omitted_test_paths": max(0, len(linked_tests) - _ASSURANCE_TEST_PATH_LIMIT),
                "inferred_sites": inferred_sites,
                "ambiguous_sites": ambiguous_sites,
            }
            if linked_tests:
                uncertain_count = inferred_sites + ambiguous_sites
                suffix = "; %d unresolved site(s) remain" % uncertain_count if uncertain_count else ""
                state = "supported"
                reason = "%d extracted test-file link(s); pass status is not asserted%s" % (len(linked_tests), suffix)
            elif ambiguous_sites:
                state = "attention"
                reason = "No extracted test-file link; %d ambiguous source site(s)" % ambiguous_sites
            else:
                state = "needs-evidence"
                suffix = "; %d inferred source site(s) remain" % inferred_sites if inferred_sites else ""
                reason = "No extracted test-file link in this graph%s" % suffix
            row = {"assurance": state, "assurance_label": ASSURANCE_LABELS[state],
                   "assurance_reason": reason, "signals": signals}
        projected[node_id] = row
        state = str(row["assurance"])
        counts[state] = counts.get(state, 0) + 1
    return {
        "schema": 1,
        "basis": ASSURANCE_BASIS,
        "note": "Structural evidence only; no test-pass or completion claim.",
        "nodes": projected,
        "counts": dict(sorted(counts.items())),
    }


def hubs(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], *, limit: int = 10, kinds: Sequence[str] = ()) -> List[Dict[str, Any]]:
    """Highest evidence degree first; `kinds` only restricts listed nodes."""
    index = _node_index(nodes)
    counts = evidence_degrees(nodes, edges)
    wanted = tuple(kinds)
    rows = []  # type: List[Dict[str, Any]]
    for node_id in sorted(index):
        kind = index[node_id].get("kind")
        kind = kind if isinstance(kind, str) else ""
        if wanted and kind not in wanted:
            continue
        rows.append({"id": node_id, "kind": kind, "degree": counts[node_id]["total"]})
    rows.sort(key=lambda row: (-row["degree"], row["id"]))
    if limit >= 0:
        rows = rows[:limit]
    return rows


def _finish_order(order: Sequence[str], adjacency: Mapping[str, Sequence[str]]) -> List[str]:
    """Iterative post-order over out-edges; no recursion limit on deep graphs."""
    seen = set()
    finished = []  # type: List[str]
    for start in order:
        if start in seen:
            continue
        seen.add(start)
        stack = [(start, 0)]
        while stack:
            node, position = stack[-1]
            neighbours = adjacency.get(node, ())
            if position < len(neighbours):
                stack[-1] = (node, position + 1)
                nxt = neighbours[position]
                if nxt not in seen:
                    seen.add(nxt)
                    stack.append((nxt, 0))
            else:
                stack.pop()
                finished.append(node)
    return finished


def connected_components(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], *, undirected: bool = True) -> List[List[str]]:
    """Weakly connected components by default; strongly connected components
    (iterative Kosaraju) when `undirected` is false."""
    index = _node_index(nodes)
    pairs = _iter_edges(edges, index)
    order = sorted(index)
    groups = []  # type: List[List[str]]
    if undirected:
        adjacency = _adjacency(pairs, index, undirected=True)
        seen = set()
        for start in order:
            if start in seen:
                continue
            seen.add(start)
            members = []
            queue = deque([start])
            while queue:
                node = queue.popleft()
                members.append(node)
                for nxt in adjacency.get(node, ()):
                    if nxt not in seen:
                        seen.add(nxt)
                        queue.append(nxt)
            groups.append(members)
    else:
        forward = _adjacency(pairs, index, undirected=False)
        backward = _reverse_adjacency(pairs, index)
        assigned = set()
        for node in reversed(_finish_order(order, forward)):
            if node in assigned:
                continue
            assigned.add(node)
            members = []
            stack = [node]
            while stack:
                current = stack.pop()
                members.append(current)
                for previous in backward.get(current, ()):
                    if previous not in assigned:
                        assigned.add(previous)
                        stack.append(previous)
            groups.append(members)
    result = [sorted(members) for members in groups]
    result.sort(key=lambda members: (-len(members), members[0]))
    return result


def cycles(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], *, relations: Sequence[str] = ("imports", "calls"), limit: int = 20) -> List[List[str]]:
    """Simple directed cycles, each listed once as the rotation starting at its
    smallest id. The walk anchors on that smallest id and never descends to a
    smaller one, which is what makes the rotation canonical. Collection stops at
    `limit` cycles; the returned list is sorted by (length, ids). Discovery is
    exhaustive for any graph the expansion budgets can cover and best-effort
    beyond it, never unbounded."""
    index = _node_index(nodes)
    pairs = _iter_edges(edges, index, relations)
    adjacency = _adjacency(pairs, index, undirected=False)
    found = []  # type: List[List[str]]
    seen = set()
    budget = _CYCLE_EXPANSION_BUDGET
    for start in sorted(index):
        if len(found) >= limit or budget <= 0:
            break
        anchor_budget = _CYCLE_ANCHOR_BUDGET
        stack = [(start, 0)]
        path = [start]
        on_path = set(path)
        while stack:
            if len(found) >= limit or budget <= 0 or anchor_budget <= 0:
                break
            node, position = stack[-1]
            neighbours = adjacency.get(node, ())
            if position >= len(neighbours):
                stack.pop()
                on_path.discard(path.pop())
                continue
            stack[-1] = (node, position + 1)
            nxt = neighbours[position]
            if nxt < start:
                continue
            budget -= 1
            anchor_budget -= 1
            if nxt == start:
                key = tuple(path)
                if key not in seen:
                    seen.add(key)
                    found.append(list(path))
                continue
            if nxt in on_path:
                continue
            path.append(nxt)
            on_path.add(nxt)
            stack.append((nxt, 0))
    found.sort(key=lambda cycle: (len(cycle), cycle))
    return found[:limit]


def shortest_path(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], source: str, target: str, *, undirected: bool = True) -> Optional[List[str]]:
    index = _node_index(nodes)
    if source not in index or target not in index:
        return None
    if source == target:
        return [source]
    adjacency = _adjacency(_iter_edges(edges, index), index, undirected=undirected)
    previous = {source: source}
    queue = deque([source])
    while queue:
        node = queue.popleft()
        for nxt in adjacency.get(node, ()):
            if nxt in previous:
                continue
            previous[nxt] = node
            if nxt == target:
                path = [target]
                while path[-1] != source:
                    path.append(previous[path[-1]])
                path.reverse()
                return path
            queue.append(nxt)
    return None


def _dominant_directory(members: Sequence[str], index: Mapping[str, Any]) -> str:
    counts = {}  # type: Dict[str, int]
    for node_id in members:
        path = index[node_id].get("path")
        if not isinstance(path, str) or not path:
            continue
        parts = path.replace("\\", "/").lstrip("/").split("/", 1)
        top = parts[0] if len(parts) > 1 and parts[0] else "root"
        counts[top] = counts.get(top, 0) + 1
    if not counts:
        return "root"
    return min(counts.items(), key=lambda item: (-item[1], item[0]))[0]


def communities(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], *, max_rounds: int = 20) -> List[Dict[str, Any]]:
    """Label propagation: every node starts labelled with its own id, nodes are
    visited in sorted order and adopt the commonest neighbour label with ties
    broken by the smallest label, so the partition is reproducible."""
    index = _node_index(nodes)
    adjacency = _adjacency(_iter_edges(edges, index), index, undirected=True)
    order = sorted(index)
    labels = {node_id: node_id for node_id in order}
    for _round in range(max(0, max_rounds)):
        changed = False
        for node_id in order:
            neighbours = adjacency.get(node_id, ())
            if not neighbours:
                continue
            tally = {}  # type: Dict[str, int]
            for nxt in neighbours:
                label = labels[nxt]
                tally[label] = tally.get(label, 0) + 1
            best = min(tally.items(), key=lambda item: (-item[1], item[0]))[0]
            if best != labels[node_id]:
                labels[node_id] = best
                changed = True
        if not changed:
            break
    grouped = {}  # type: Dict[str, List[str]]
    for node_id in order:
        grouped.setdefault(labels[node_id], []).append(node_id)
    ranked = sorted(grouped.values(), key=lambda members: (-len(members), members[0]))
    result = []  # type: List[Dict[str, Any]]
    for position, members in enumerate(ranked, 1):
        result.append({
            "id": "c%d" % position,
            "label": _dominant_directory(members, index),
            "members": members,
        })
    return result
