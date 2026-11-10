"""Symbols with no incoming graph edge: review candidates, never facts.

The Project Graph only holds what its parsers resolved. A symbol can have no
incoming edge because it is dead, or because it is reached through a dynamic
call, a decorator, a CLI/entrypoint table, a framework hook, a shell `eval`, or
a file the parsers do not read. Every row is therefore a candidate to look at,
and the report says so; an empty result proves nothing either.
"""

from __future__ import annotations

from typing import Any, Dict, List

from .query import Graph

SYMBOL_KINDS = ("function", "method", "class", "symbol")
# Reached by convention rather than by a resolvable reference.
ENTRYPOINT_NAMES = ("main",)
NOTE = ("candidates only: dynamic calls, decorators, entrypoint tables, framework hooks and "
        "unparsed files are invisible to the graph, so none of these is a confirmed dead symbol")


def unreferenced_report(index: Graph, *, limit: int = 100, include_tests: bool = False) -> Dict[str, Any]:
    """Symbols whose only incoming edges are structural `contains` edges."""
    rows: List[Dict[str, Any]] = []
    for node_id, node in index.nodes.items():
        if node.get("kind") not in SYMBOL_KINDS:
            continue
        if not include_tests and index.is_test(node_id):
            continue
        name = str(node.get("name", ""))
        if name in ENTRYPOINT_NAMES or (name.startswith("__") and name.endswith("__")):
            continue
        if any(edge["relation"] != "contains" for edge in index.ins.get(node_id, [])):
            continue
        metadata = node.get("metadata") if isinstance(node.get("metadata"), dict) else {}
        rows.append({"id": node_id, "kind": node["kind"], "path": node.get("path", ""),
                     "line": metadata.get("line")})
    rows.sort(key=lambda row: (str(row["path"]), row["line"] or 0, row["id"]))
    shown = rows[:limit] if limit > 0 else rows
    return {"note": NOTE, "total": len(rows), "truncated": len(shown) < len(rows), "candidates": shown}
