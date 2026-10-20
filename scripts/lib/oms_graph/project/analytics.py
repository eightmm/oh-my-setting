"""Stdlib graph analytics: evidence degrees and structural assurance (W-G).

Every function is pure: it reads only the node and edge sequences it is given,
never the disk. Edges whose endpoints are not among the supplied nodes are
ignored rather than rejected, so a partial slice of a graph analyses cleanly.
"""

from __future__ import annotations

from typing import Any, Dict, Mapping, Sequence, Tuple

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
