"""Graph index: find, neighbors, trace, map summary (W2)."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from ..errors import GraphError

class Graph:
    """In-memory index over a loaded graph.json."""

    def __init__(self, graph: Mapping[str, Any]) -> None:
        self.graph = dict(graph)
        self.nodes = {item["id"]: dict(item) for item in graph.get("nodes", [])}
        self.out: Dict[str, List[Dict[str, Any]]] = {}
        self.ins: Dict[str, List[Dict[str, Any]]] = {}
        for edge in graph.get("edges", []):
            self.out.setdefault(edge["source"], []).append(dict(edge))
            self.ins.setdefault(edge["target"], []).append(dict(edge))
        for mapping in (self.out, self.ins):
            for rows in mapping.values():
                rows.sort(key=lambda item: (item["source"], item["target"], item["relation"]))

    def node(self, node_id: str) -> Dict[str, Any]:
        if node_id not in self.nodes:
            raise GraphError("unknown graph node: %s" % node_id)
        return dict(self.nodes[node_id])

    def find(self, query: str, *, kinds: Sequence[str] = (), limit: int = 20) -> List[Dict[str, Any]]:
        needle = query.strip().lower()
        tokens = set(re.findall(r"[a-z0-9_]+", needle))
        rows = []
        for node in self.nodes.values():
            if kinds and node.get("kind") not in kinds:
                continue
            name = str(node.get("name", "")).lower()
            path = str(node.get("path", "")).lower()
            qualname = str(node.get("metadata", {}).get("qualname", "")).lower()
            corpus = " ".join((name, path, qualname, str(node.get("summary") or "").lower()))
            if name == needle:
                score = 100
            elif needle and (needle in path or needle in qualname or needle in name):
                score = 60
            else:
                overlap = len(tokens & set(re.findall(r"[a-z0-9_]+", corpus)))
                score = overlap * 10
            if score:
                item = dict(node); item["score"] = score; rows.append(item)
        return sorted(rows, key=lambda item: (-item["score"], item["id"]))[:max(0, limit)]

    def neighbors(self, node_id: str, *, relation: str = "", direction: str = "both") -> List[Dict[str, Any]]:
        self.node(node_id)
        rows = []
        if direction in ("out", "both"):
            for edge in self.out.get(node_id, []):
                if not relation or edge["relation"] == relation:
                    rows.append({"id": edge["target"], "relation": edge["relation"], "direction": "out", "confidence": edge["confidence"]})
        if direction in ("in", "both"):
            for edge in self.ins.get(node_id, []):
                if not relation or edge["relation"] == relation:
                    rows.append({"id": edge["source"], "relation": edge["relation"], "direction": "in", "confidence": edge["confidence"]})
        return sorted(rows, key=lambda item: (item["id"], item["relation"], item["direction"]))

    def trace(self, node_id: str, *, direction: str = "out", depth: int = 2, relations: Sequence[str] = ()) -> Dict[str, Any]:
        self.node(node_id)
        seen = {node_id: {"id": node_id, "distance": 0, "via": ""}}
        edges: List[Dict[str, Any]] = []
        queue = [node_id]
        while queue and len(seen) < 500:
            current = queue.pop(0); distance = seen[current]["distance"]
            if distance >= max(0, depth): continue
            candidates = self.out.get(current, []) if direction == "out" else self.ins.get(current, [])
            for edge in candidates:
                if relations and edge["relation"] not in relations: continue
                adjacent = edge["target"] if direction == "out" else edge["source"]
                edges.append(dict(edge))
                if adjacent not in seen and len(seen) < 500:
                    seen[adjacent] = {"id": adjacent, "distance": distance + 1, "via": edge["relation"]}
                    queue.append(adjacent)
        return {"root": node_id, "direction": direction, "depth": depth,
                "nodes": sorted(seen.values(), key=lambda item: (item["distance"], item["id"])),
                "edges": sorted(edges, key=lambda item: (item["source"], item["target"], item["relation"]))}

    def map_summary(self) -> Dict[str, Any]:
        by_kind: Dict[str, int] = {}; by_language: Dict[str, int] = {}; modules: Dict[str, List[str]] = {}
        for node in self.nodes.values():
            by_kind[node["kind"]] = by_kind.get(node["kind"], 0) + 1
            by_language[node.get("language", "")] = by_language.get(node.get("language", ""), 0) + 1
            if node["kind"] == "module": modules.setdefault(node.get("path", "").split("/", 1)[0], []).append(node["id"])
        degree = lambda ident: len(self.out.get(ident, [])) + len(self.ins.get(ident, []))
        hubs = sorted(({"id": ident, "degree": degree(ident)} for ident in self.nodes), key=lambda item: (-item["degree"], item["id"]))[:10]
        return {"kinds": dict(sorted(by_kind.items())), "languages": dict(sorted(by_language.items())), "hubs": hubs,
                "modules": {key: sorted(value) for key, value in sorted(modules.items())}}
