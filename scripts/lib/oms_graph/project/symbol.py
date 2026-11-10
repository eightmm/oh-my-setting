"""One view of a symbol: where it is defined, who the graph says refers to it, and where its name appears unlinked.

The three zones never mix: `extracted` is what a parser read from source,
`inferred` is a resolver's guess (INFERRED or AMBIGUOUS edges), and
`text_only` is a literal match the graph has no edge for. A reader can tell at
a glance how much of the answer the graph vouches for. Orientation only: an
empty zone does not prove the code is unused.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Mapping, Set, Tuple

from ..errors import GraphError
from .query import Graph

SYMBOL_KINDS = ("class", "function", "method", "symbol")
MAX_DEFINITIONS = 20


def _metadata(node: Mapping[str, Any]) -> Mapping[str, Any]:
    value = node.get("metadata")
    return value if isinstance(value, Mapping) else {}


def _definitions(index: Graph, name: str) -> Tuple[List[Dict[str, Any]], str]:
    """Symbols the name selects, and how: a node id, an exact name or qualified name, else a case-folded one."""
    if name in index.nodes and index.nodes[name].get("kind") in SYMBOL_KINDS:
        return [dict(index.nodes[name])], "id"
    symbols = [node for node in index.nodes.values() if node.get("kind") in SYMBOL_KINDS]
    for label, same in (("exact", lambda left, right: left == right),
                        ("case-insensitive", lambda left, right: left.casefold() == right.casefold())):
        rows = [dict(node) for node in symbols
                if same(str(node.get("name", "")), name) or same(str(_metadata(node).get("qualname", "")), name)]
        if rows:
            return sorted(rows, key=lambda item: (str(item.get("path", "")), str(item["id"]))), label
    return [], "none"


def symbol_view(repo: Path, index: Graph, name: str, *, limit: int = 40) -> Dict[str, Any]:
    name = str(name or "").strip()
    if not name or len(name.encode("utf-8")) > 512:
        raise GraphError("project symbol requires a non-empty name of at most 512 bytes")
    row_limit = max(1, min(500, int(limit)))
    found, match = _definitions(index, name)
    omitted = {"definitions": max(0, len(found) - MAX_DEFINITIONS)}
    found = found[:MAX_DEFINITIONS]

    definitions = []
    spans: Dict[str, List[Tuple[int, int]]] = {}
    for node in found:
        metadata = _metadata(node)
        line = int(metadata.get("line", 0) or 0)
        end = int(metadata.get("end_line", line) or line)
        qualname = str(metadata.get("qualname") or node.get("name") or "")
        signature = str(metadata.get("signature") or "") or (
            ("class %s" if node.get("kind") == "class" else "def %s(...)") % qualname)
        definitions.append({"id": node["id"], "kind": node.get("kind", "symbol"), "path": node.get("path", ""),
                            "line": line, "end_line": end, "qualname": qualname, "signature": signature})
        spans.setdefault(str(node.get("path", "")), []).append((line, end))

    zones: Dict[str, List[Dict[str, Any]]] = {"callers": [], "tests": [], "inferred": []}
    linked: Set[Tuple[str, int]] = set()
    seen: Set[Tuple[str, str, str, str, int]] = set()
    for node in found:
        for edge in index.ins.get(node["id"], []):
            if edge.get("relation") in ("defines", "contains"):
                continue
            evidence = edge.get("evidence") if isinstance(edge.get("evidence"), Mapping) else {}
            path, line = str(evidence.get("path", "")), int(evidence.get("line", 0) or 0)
            confidence = str(edge.get("confidence", "AMBIGUOUS") or "AMBIGUOUS")
            key = (edge["source"], node["id"], str(edge["relation"]), confidence, line)
            if key in seen:
                continue
            seen.add(key)
            if path and line:
                linked.add((path, line))
            row = {"source": edge["source"], "target": node["id"], "relation": edge["relation"],
                   "confidence": confidence, "path": path, "line": line}
            if confidence != "EXTRACTED":
                zones["inferred"].append(row)
            elif index.is_test(edge["source"]):
                zones["tests"].append(row)
            else:
                zones["callers"].append(row)
    for key, rows in zones.items():
        rows.sort(key=lambda item: (item["path"], item["line"], item["source"], item["relation"], item["target"]))
        omitted[key] = max(0, len(rows) - row_limit)
        del rows[row_limit:]

    # The bare name is what source text carries: `Lease.recover` is written `recover` at a call site.
    bare = (str(_metadata(found[0]).get("qualname") or found[0].get("name") or name) if found else name).split(".")[-1]
    word = re.compile(r"(?<![A-Za-z0-9_])%s(?![A-Za-z0-9_])" % re.escape(bare), re.IGNORECASE)
    scan = index.search(repo, bare, limit=500)
    text_only: List[Dict[str, Any]] = []
    for group in scan["groups"]:
        path = str(group.get("path", ""))
        for hit in group["hits"]:
            line = int(hit["line"])
            if (path, line) in linked or any(start <= line <= end for start, end in spans.get(path, [])):
                continue
            # A withheld preview cannot be checked for a whole-word match; it is listed rather than dropped.
            if hit["text"] != "[sensitive-content-omitted]" and not word.search(hit["text"]):
                continue
            text_only.append({"path": path, "line": line, "within": group["id"], "text": hit["text"]})
    text_only.sort(key=lambda item: (item["path"], item["line"]))
    omitted["text_only"] = max(0, len(text_only) - row_limit)
    del text_only[row_limit:]

    return {
        "schema": 1, "kind": "oms-project-symbol", "query": name, "match": match, "text_query": bare,
        "content_trust": "untrusted-source-data",
        "extracted": {"definitions": definitions, "callers": zones["callers"], "tests": zones["tests"]},
        "inferred": zones["inferred"],
        "text_only": text_only,
        "omitted": omitted,
        "text_scan": {"searched_files": scan["searched_files"], "stale_files": scan["stale_files"],
                      "skipped_files": scan["skipped_files"], "truncated": bool(scan["truncated"])},
    }


def view_text(view: Mapping[str, Any]) -> str:
    lines = ["symbol %s: %d definition(s) (%s match); orientation, not proof of use or disuse"
             % (view["query"], len(view["extracted"]["definitions"]), view["match"])]
    lines.append("extracted (read from source by a parser):")
    for row in view["extracted"]["definitions"]:
        lines.append("  def     %s:%d  %s  [%s]" % (row["path"], row["line"], row["signature"], row["id"]))
    for label, key in (("caller", "callers"), ("test", "tests")):
        for row in view["extracted"][key]:
            lines.append("  %-7s %s:%d  %s  %s" % (label, row["path"], row["line"], row["relation"], row["source"]))
    if not (view["extracted"]["definitions"] or view["extracted"]["callers"] or view["extracted"]["tests"]):
        lines.append("  none")
    lines.append("inferred (a resolver's guess; check before relying on it):")
    for row in view["inferred"]:
        lines.append("  %-9s %s:%d  %s  %s" % (row["confidence"].lower(), row["path"], row["line"], row["relation"], row["source"]))
    if not view["inferred"]:
        lines.append("  none")
    lines.append("text-only (the name %r appears here and the graph has no edge for it):" % view["text_query"])
    for row in view["text_only"]:
        lines.append("  %s:%d  %s" % (row["path"], row["line"], row["text"]))
    if not view["text_only"]:
        lines.append("  none")
    cut = ", ".join("%s +%d" % (key, count) for key, count in sorted(view["omitted"].items()) if count)
    if cut:
        lines.append("omitted: %s (raise --limit)" % cut)
    scan = view["text_scan"]
    if scan["stale_files"] or scan["skipped_files"] or scan["truncated"]:
        lines.append("text scan incomplete: %d stale, %d skipped file(s)%s"
                     % (scan["stale_files"], scan["skipped_files"], "; hit list truncated" if scan["truncated"] else ""))
    return "\n".join(lines) + "\n"
