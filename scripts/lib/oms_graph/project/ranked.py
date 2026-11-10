"""Ranked multi-word search over symbol blocks: BM25, stdlib only, no embedding and no index on disk.

`search` answers "where is this exact text"; this answers "which block is
about these words". Every indexed line belongs to its innermost symbol, or to
its file when no symbol encloses it, and that block is one document. Bodies,
comments and identifiers all count; camelCase and snake_case are split the way
`find` splits them. The same safety rules as `search` apply: a file whose
bytes no longer match the graph is not read as current, notebooks are skipped,
and a secret-shaped preview is withheld.
"""

from __future__ import annotations

import hashlib
import math
from collections import Counter
from pathlib import Path
from typing import Any, Dict, List, Mapping, Tuple

from oms_runtime.common import sensitive_text

from ..errors import GraphError
from .query import Graph, _identifier_words

K1, B = 1.2, 0.75
MAX_TERMS = 16
SYMBOL_KINDS = ("class", "function", "method", "symbol")
FILE_KINDS = ("file", "test", "document", "config")


def ranked_search(repo: Path, index: Graph, query: str, *, limit: int = 20) -> Dict[str, Any]:
    text = str(query or "")
    if not text.strip() or len(text.encode("utf-8")) > 1024:
        raise GraphError("project search --ranked requires a non-empty query of at most 1024 bytes")
    terms = list(dict.fromkeys(_identifier_words(text)))[:MAX_TERMS]
    if not terms:
        raise GraphError("project search --ranked found no word in the query")
    row_limit = max(1, min(500, int(limit)))
    root = Path(repo).resolve()
    file_nodes = {str(node.get("path")): node for node in index.nodes.values()
                  if node.get("kind") in FILE_KINDS and node.get("path")}
    symbols_by_path: Dict[str, List[Tuple[int, int, Dict[str, Any]]]] = {}
    for node in index.nodes.values():
        if node.get("kind") not in SYMBOL_KINDS:
            continue
        metadata = node.get("metadata") if isinstance(node.get("metadata"), Mapping) else {}
        start = int(metadata.get("line", 0) or 0)
        end = int(metadata.get("end_line", start) or start)
        if start:
            symbols_by_path.setdefault(str(node.get("path", "")), []).append((start, end, node))

    wanted = set(terms)
    documents: List[Dict[str, Any]] = []
    searched = stale = skipped = 0
    for path in sorted(file_nodes):
        target = root / path
        if file_nodes[path].get("language") == "notebook":
            skipped += 1
            continue
        try:
            if target.is_symlink() or not target.is_file() or target.stat().st_size > 2 * 1024 * 1024:
                skipped += 1
                continue
            target.resolve().relative_to(root)
            data = target.read_bytes()
        except (OSError, ValueError):
            skipped += 1
            continue
        if hashlib.sha256(data).hexdigest() != str(file_nodes[path].get("source_digest", "")):
            stale += 1
            continue
        searched += 1
        spans = sorted(symbols_by_path.get(path, []), key=lambda item: (item[0], -item[1], item[2]["id"]))
        blocks: Dict[str, Dict[str, Any]] = {}
        for number, line in enumerate(data.decode("utf-8", "replace").splitlines(), 1):
            owners = [item for item in spans if item[0] <= number <= item[1]]
            owner = max(owners, key=lambda item: (item[0], -item[1], item[2]["id"]))[2] if owners else file_nodes[path]
            block = blocks.setdefault(str(owner["id"]), {"node": owner, "length": 0, "counts": Counter(), "best": (0, 0, "")})
            words = _identifier_words(line)
            block["length"] += len(words)
            hits = [word for word in words if word in wanted]
            if hits:
                block["counts"].update(hits)
                distinct = len(set(hits))
                if distinct > block["best"][0]:
                    block["best"] = (distinct, number, line)
        documents.extend(blocks.values())

    total = len(documents)
    average = (sum(doc["length"] for doc in documents) / total) if total else 0.0
    frequency = {term: sum(1 for doc in documents if doc["counts"].get(term)) for term in terms}
    results = []
    for doc in documents:
        if not doc["counts"]:
            continue
        score = 0.0
        for term in terms:
            count = doc["counts"].get(term, 0)
            if not count:
                continue
            idf = math.log(1.0 + (total - frequency[term] + 0.5) / (frequency[term] + 0.5))
            norm = count + K1 * (1.0 - B + B * (doc["length"] / average if average else 1.0))
            score += idf * count * (K1 + 1.0) / norm
        node = doc["node"]
        metadata = node.get("metadata") if isinstance(node.get("metadata"), Mapping) else {}
        preview = " ".join(doc["best"][2].strip().split())[:240]
        if sensitive_text(preview):
            preview = "[sensitive-content-omitted]"
        results.append({"id": node["id"], "kind": node.get("kind", "file"), "path": node.get("path", ""),
                        "line": int(metadata.get("line", 0) or 0),
                        "score": round(score, 4), "matched_terms": [term for term in terms if doc["counts"].get(term)],
                        "preview": {"line": doc["best"][1], "text": preview}})
    results.sort(key=lambda item: (-item["score"], -len(item["matched_terms"]), item["path"], item["line"], item["id"]))
    return {"schema": 1, "kind": "oms-project-ranked-search", "query": text, "terms": terms,
            "match": "bm25-symbol-blocks", "content_trust": "untrusted-source-data",
            "indexed_files": len(file_nodes), "searched_files": searched, "stale_files": stale,
            "skipped_files": skipped, "blocks": total, "total_matches": len(results),
            "returned": min(len(results), row_limit), "truncated": len(results) > row_limit,
            "results": results[:row_limit]}
