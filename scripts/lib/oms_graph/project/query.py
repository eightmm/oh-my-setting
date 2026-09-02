"""Graph index: find, neighbors, trace, map summary (W2)."""

from __future__ import annotations

import hashlib
import math
import re
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_runtime.common import sensitive_text

from ..errors import GraphError
from .analytics import evidence_degrees


def _identifier_words(text: str) -> List[str]:
    expanded = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", " ", str(text or ""))
    return [item for item in re.findall(r"[^\W_]+", expanded.casefold()) if item]


def _normalized_identifier(text: str) -> str:
    return " ".join(_identifier_words(text))


def _token_hits(tokens: Sequence[str], text: str) -> int:
    """Query tokens found in the text, where a 4+ character stem shared in either
    direction counts (``recovery`` finds ``recover_lease``)."""
    words = set(_identifier_words(text))
    hits = 0
    for term in tokens:
        if term in words:
            hits += 1
            continue
        if len(term) >= 4 and any(word.startswith(term) or (len(word) >= 4 and term.startswith(word)) for word in words):
            hits += 1
    return hits


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

    def is_test(self, node_id: str) -> bool:
        node = self.nodes.get(node_id, {})
        return node.get("kind") == "test" or ("test:" + str(node.get("path", ""))) in self.nodes

    def without_tests(self) -> "Graph":
        """The same graph minus test files and everything they define or touch.

        Tests name many paths, so they dominate hubs and communities; the
        overview verbs drop them by default while blast/context keep them.
        """
        keep = {ident for ident in self.nodes if not self.is_test(ident)}
        return Graph({"schema": self.graph.get("schema"), "revision": self.graph.get("revision"),
                      "nodes": [self.nodes[ident] for ident in sorted(keep)],
                      "edges": [edge for edge in self.graph.get("edges", []) if edge["source"] in keep and edge["target"] in keep]})

    def node(self, node_id: str) -> Dict[str, Any]:
        if node_id not in self.nodes:
            raise GraphError("unknown graph node: %s" % node_id)
        return dict(self.nodes[node_id])

    def find(self, query: str, *, kinds: Sequence[str] = (), limit: int = 20, include_tests: bool = False) -> List[Dict[str, Any]]:
        if not include_tests and "test" not in kinds:
            return self.without_tests().find(query, kinds=kinds, limit=limit, include_tests=True)
        needle = query.strip().casefold()
        normalized_needle = _normalized_identifier(query)
        tokens = _identifier_words(query)
        candidates = [node for node in self.nodes.values()
                      if not kinds or node.get("kind") in kinds]
        frequencies = {term: 0 for term in tokens}
        for node in candidates:
            metadata = node.get("metadata") if isinstance(node.get("metadata"), Mapping) else {}
            haystack = " ".join((str(node.get("name", "")), str(node.get("path", "")),
                                 str(metadata.get("qualname", "")), str(node.get("summary") or "")))
            for term in tokens:
                if _token_hits((term,), haystack):
                    frequencies[term] += 1
        weights = {
            term: max(1, min(8, int(round(1 + math.log(
                (len(candidates) + 1.0) / (frequencies[term] + 1.0), 2
            )))))
            for term in tokens
        }
        rows = []
        for node in candidates:
            name = str(node.get("name", "")).casefold()
            path = str(node.get("path", "")).casefold()
            qualname = str(node.get("metadata", {}).get("qualname", "")).casefold()
            summary = str(node.get("summary") or "").casefold()
            normalized_name = _normalized_identifier(name)
            normalized_qualname = _normalized_identifier(qualname)
            normalized_path = _normalized_identifier(path)
            normalized_summary = _normalized_identifier(summary)
            if name == needle or (normalized_needle and normalized_name == normalized_needle):
                score = 100
            elif needle and (needle in qualname or needle in name
                             or (normalized_needle and normalized_needle in normalized_qualname)
                             or (normalized_needle and normalized_needle in normalized_name)):
                score = 70
            elif needle and (needle in path or (normalized_needle and normalized_needle in normalized_path)):
                score = 50
            elif normalized_needle and normalized_needle in normalized_summary:
                score = 45
            else:
                score = sum(max(
                    8 * weights[term] if _token_hits((term,), name + " " + qualname) else 0,
                    3 * weights[term] if _token_hits((term,), path) else 0,
                    2 * weights[term] if _token_hits((term,), summary) else 0,
                ) for term in tokens)
                score = min(90, score)
            if score and node.get("kind") in ("class", "function", "method"):
                score += 2
            if score:
                item = dict(node); item["score"] = score; rows.append(item)
        return sorted(rows, key=lambda item: (-item["score"], item["id"]))[:max(0, limit)]

    def file_api(self, path: str, *, limit: int = 500) -> Dict[str, Any]:
        """Signatures and summaries for one indexed file, never function bodies."""
        normalized = str(path or "").replace("\\", "/")
        if (not normalized or normalized.startswith("/") or normalized.startswith("-")
                or any(part in ("", ".", "..") for part in normalized.split("/"))):
            raise GraphError("project api requires a normalized repo-relative path")
        matching = [node for node in self.nodes.values() if node.get("path") == normalized]
        if not matching:
            raise GraphError("path is not indexed by the project graph: %s" % normalized)
        roots = [node for node in matching if node.get("kind") in ("file", "test", "document", "config", "module")]
        symbols = []
        for node in matching:
            if node.get("kind") not in ("class", "function", "method", "symbol"):
                continue
            metadata = node.get("metadata") if isinstance(node.get("metadata"), Mapping) else {}
            qualname = str(metadata.get("qualname") or node.get("name") or "")
            signature = str(metadata.get("signature") or "")
            if not signature:
                signature = ("class %s" if node.get("kind") == "class" else "def %s(...)") % qualname
            row = {
                "id": node["id"], "kind": node.get("kind", "symbol"),
                "name": node.get("name", ""), "qualname": qualname,
                "line": int(metadata.get("line", 0) or 0),
                "end_line": int(metadata.get("end_line", metadata.get("line", 0)) or 0),
                "signature": signature,
            }
            if node.get("summary"):
                row["summary"] = str(node["summary"])
            symbols.append(row)
        symbols.sort(key=lambda item: (item["line"] or 2 ** 31, item["qualname"], item["id"]))
        total_symbols = len(symbols)
        symbols = symbols[:max(1, min(2000, int(limit)))]
        language = next((str(node.get("language", "")) for node in roots if node.get("language")), "")
        summary = next((str(node.get("summary")) for node in roots if node.get("summary")), "")
        return {"schema": 1, "path": normalized, "language": language,
                "content_trust": "untrusted-source-data",
                "summary": summary or None, "total_symbols": total_symbols,
                "returned_symbols": len(symbols), "truncated": len(symbols) < total_symbols,
                "symbols": symbols}

    def search(self, repo: Path, query: str, *, limit: int = 200) -> Dict[str, Any]:
        """Case-insensitive literal source search grouped by enclosing symbol."""
        needle = str(query or "")
        if not needle or len(needle.encode("utf-8")) > 1024:
            raise GraphError("project search requires a non-empty query of at most 1024 bytes")
        row_limit = max(1, min(500, int(limit)))
        root = Path(repo).resolve()
        file_nodes = {
            str(node.get("path")): node
            for node in self.nodes.values()
            if node.get("kind") in ("file", "test", "document", "config") and node.get("path")
        }
        symbols_by_path: Dict[str, List[Dict[str, Any]]] = {}
        for node in self.nodes.values():
            if node.get("kind") not in ("class", "function", "method", "symbol"):
                continue
            metadata = node.get("metadata") if isinstance(node.get("metadata"), Mapping) else {}
            row = dict(node)
            row["_line"] = int(metadata.get("line", 0) or 0)
            row["_end"] = int(metadata.get("end_line", metadata.get("line", 0)) or 0)
            symbols_by_path.setdefault(str(node.get("path", "")), []).append(row)
        for rows in symbols_by_path.values():
            rows.sort(key=lambda item: (item["_line"], -item["_end"], item["id"]))

        captured: List[Tuple[Dict[str, Any], int, str]] = []
        total_hits = 0
        searched_files = 0
        stale_files = 0
        skipped_files = 0
        capture_limit = max(1000, row_limit * 20)
        folded = needle.casefold()
        for path in sorted(file_nodes):
            target = root / path
            try:
                if target.is_symlink() or not target.is_file() or target.stat().st_size > 2 * 1024 * 1024:
                    skipped_files += 1
                    continue
                target.resolve().relative_to(root)
                data = target.read_bytes()
            except (OSError, ValueError):
                skipped_files += 1
                continue
            if hashlib.sha256(data).hexdigest() != str(file_nodes[path].get("source_digest", "")):
                stale_files += 1
                continue
            lines = data.decode("utf-8", "replace").splitlines()
            searched_files += 1
            symbols = symbols_by_path.get(path, [])
            for number, line in enumerate(lines, 1):
                if folded not in line.casefold():
                    continue
                total_hits += 1
                if len(captured) >= capture_limit:
                    continue
                owners = [row for row in symbols
                          if row["_line"] <= number and (not row["_end"] or number <= row["_end"])]
                owner = max(owners, key=lambda item: (item["_line"], -item["_end"], item["id"])) if owners else file_nodes[path]
                preview = " ".join(line.strip().split())[:240]
                if sensitive_text(preview):
                    preview = "[sensitive-content-omitted]"
                captured.append((owner, number, preview))

        grouped: Dict[str, Dict[str, Any]] = {}
        for owner, number, preview in captured:
            ident = str(owner["id"])
            group = grouped.setdefault(ident, {
                "id": ident, "kind": owner.get("kind", "file"),
                "path": owner.get("path", ""), "name": owner.get("name", ""),
                "incoming": len(self.ins.get(ident, [])), "hits": [],
            })
            if len(group["hits"]) < 8:
                group["hits"].append({"line": number, "text": preview})
        ranked = sorted(grouped.values(), key=lambda item: (
            -int(item["incoming"]), str(item["path"]),
            int(item["hits"][0]["line"]) if item["hits"] else 0, str(item["id"]),
        ))
        returned = 0
        groups = []
        for group in ranked:
            remaining = row_limit - returned
            if remaining <= 0:
                break
            group = dict(group)
            group["hits"] = list(group["hits"][:remaining])
            returned += len(group["hits"])
            groups.append(group)
        return {
            "schema": 1, "query": needle, "match": "literal-case-insensitive",
            "content_trust": "untrusted-source-data", "indexed_files": len(file_nodes),
            "searched_files": searched_files, "stale_files": stale_files,
            "skipped_files": skipped_files, "total_hits": total_hits,
            "returned_hits": returned, "truncated": total_hits > returned,
            "groups": groups,
        }

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

    def trace(
        self,
        node_id: str,
        *,
        direction: str = "out",
        depth: int = 2,
        relations: Sequence[str] = (),
        confidences: Sequence[str] = (),
        max_nodes: int = 128,
        max_edges: int = 160,
    ) -> Dict[str, Any]:
        self.node(node_id)
        node_limit = max(1, int(max_nodes))
        edge_limit = max(0, int(max_edges))
        seen = {node_id: {"id": node_id, "distance": 0, "via": ""}}
        edges: List[Dict[str, Any]] = []
        omitted_edges = 0
        node_limit_reached = False
        queue = [node_id]
        while queue:
            current = queue.pop(0); distance = seen[current]["distance"]
            if distance >= max(0, depth): continue
            candidates = self.out.get(current, []) if direction == "out" else self.ins.get(current, [])
            for edge in candidates:
                if relations and edge["relation"] not in relations: continue
                if confidences and edge.get("confidence", "AMBIGUOUS") not in confidences: continue
                adjacent = edge["target"] if direction == "out" else edge["source"]
                if adjacent not in seen and len(seen) >= node_limit:
                    node_limit_reached = True
                    omitted_edges += 1
                    continue
                if adjacent not in seen:
                    seen[adjacent] = {"id": adjacent, "distance": distance + 1,
                                      "via": edge["relation"], "confidence": edge.get("confidence", "AMBIGUOUS")}
                    queue.append(adjacent)
                if len(edges) >= edge_limit:
                    omitted_edges += 1
                    continue
                evidence = edge.get("evidence") if isinstance(edge.get("evidence"), Mapping) else {}
                projected_evidence = {
                    key: evidence[key] for key in ("path", "line") if key in evidence
                }
                edges.append({"source": edge["source"], "target": edge["target"],
                              "relation": edge["relation"],
                              "confidence": edge.get("confidence", "AMBIGUOUS"),
                              "evidence": projected_evidence})
        return {"schema": 1, "root": node_id, "direction": direction, "depth": depth,
                "nodes": sorted(seen.values(), key=lambda item: (item["distance"], item["id"])),
                "edges": sorted(edges, key=lambda item: (item["source"], item["target"], item["relation"])),
                "limits": {"nodes": node_limit, "edges": edge_limit},
                "truncated": bool(node_limit_reached or omitted_edges),
                "node_limit_reached": node_limit_reached,
                "omitted_edges": omitted_edges}

    def degree(self, node_id: str) -> int:
        return len(self.out.get(node_id, [])) + len(self.ins.get(node_id, []))

    def map_summary(self) -> Dict[str, Any]:
        by_kind: Dict[str, int] = {}
        by_language: Dict[str, int] = {}
        groups: Dict[str, List[str]] = {}
        confidence_counts: Dict[str, int] = {}
        for node in self.nodes.values():
            by_kind[node["kind"]] = by_kind.get(node["kind"], 0) + 1
            language = node.get("language", "") or "unknown"
            by_language[language] = by_language.get(language, 0) + 1
            if node["kind"] == "module":
                path = node.get("path", "")
                groups.setdefault(path.split("/", 1)[0] if "/" in path else "root", []).append(node["id"])
        for edge in self.graph.get("edges", []):
            if not isinstance(edge, Mapping) or edge.get("source") not in self.nodes or edge.get("target") not in self.nodes:
                continue
            confidence = str(edge.get("confidence", "AMBIGUOUS") or "AMBIGUOUS")
            confidence_counts[confidence] = confidence_counts.get(confidence, 0) + 1
        ranked_degree = evidence_degrees(list(self.nodes.values()), self.graph.get("edges", []))
        hubs = sorted(({"id": ident, "kind": node["kind"], "degree": ranked_degree[ident]["total"]} for ident, node in self.nodes.items()),
                      key=lambda item: (-item["degree"], item["id"]))[:10]
        return {"revision": str(self.graph.get("revision", "")), "counts": {"kind": dict(sorted(by_kind.items())), "language": dict(sorted(by_language.items())),
                                                                               "confidence": dict(sorted(confidence_counts.items()))},
                "hubs": hubs, "groups": {key: sorted(value) for key, value in sorted(groups.items())}}
