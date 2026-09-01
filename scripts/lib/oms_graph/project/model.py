"""Project graph node/edge shapes, ids, and canonical ordering (W2)."""

from __future__ import annotations

import re
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

# A repo-relative path literal: at least one directory separator, no leading
# slash or dot segment. Resolution keeps only paths the working tree contains.
PATH_LITERAL_RE = re.compile(r"(?<![A-Za-z0-9_./$\\-])([A-Za-z0-9_][A-Za-z0-9_.-]*(?:/[A-Za-z0-9_][A-Za-z0-9_.-]*)+)")

def node_id(kind: str, path: str, qualname: str = "") -> str:
    path = path.replace("\\", "/").lstrip("/")
    if kind == "file":
        return "file:%s" % path
    if kind == "module":
        return "module:%s" % path
    if kind in ("class", "function", "method", "symbol"):
        return "symbol:%s::%s" % (path, qualname)
    return "%s:%s" % (kind, path)


def make_node(kind: str, name: str, path: str, language: str, source_digest: str, *, qualname: str = "", line: Optional[int] = None, metadata: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    node: Dict[str, Any] = {"id": node_id(kind, path, qualname), "kind": kind,
                            "name": name, "path": path.replace("\\", "/"),
                            "language": language, "source_digest": source_digest,
                            "summary": None, "crux": None, "metadata": dict(metadata or {})}
    if qualname:
        node["metadata"]["qualname"] = qualname
    if line is not None:
        node["metadata"]["line"] = int(line)
    return node


def make_edge(source: str, target: str, relation: str, confidence: str, *, path: str, source_digest: str, line: Optional[int] = None, candidates: Optional[Sequence[str]] = None) -> Dict[str, Any]:
    evidence: Dict[str, Any] = {"path": path.replace("\\", "/"), "source_digest": source_digest}
    if line is not None:
        evidence["line"] = int(line)
    if candidates:
        evidence["candidates"] = sorted(candidates)
    return {"source": source, "target": target, "relation": relation,
            "confidence": confidence, "evidence": evidence}


def sort_nodes(nodes: Sequence[Mapping[str, Any]]) -> List[Dict[str, Any]]:
    return [dict(item) for item in sorted(nodes, key=lambda item: str(item["id"]))]


def sort_edges(edges: Sequence[Mapping[str, Any]]) -> List[Dict[str, Any]]:
    return [dict(item) for item in sorted(edges,
            key=lambda item: (str(item["source"]), str(item["target"]), str(item["relation"]),
                              str(item.get("confidence", "")), int(item.get("evidence", {}).get("line", 0))))]
