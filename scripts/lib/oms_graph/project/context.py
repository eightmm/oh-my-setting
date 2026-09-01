"""Task-specific context pack over the project graph (W2)."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_runtime.common import atomic_write_json, canonical_json, ensure_private_dir, relative_path, sha256_bytes
from oms_runtime.context import plan_context

from .blast import blast_radius
from .build import state_dir
from .query import Graph


def context_pack(repo: Path, graph: Any, *, task: str, max_files: int = 12, max_nodes: int = 40, depth: int = 2, base: str = "", state: Optional[Path] = None) -> Dict[str, Any]:
    """Write .oms/project-graph/context/<digest>.json and return the pack."""
    index = graph if isinstance(graph, Graph) else Graph(graph)
    stopwords = {"the", "and", "for", "with", "from", "that", "this", "into", "add", "fix", "use", "are", "was", "will", "please"}
    tokens = [item for item in re.findall(r"[a-z0-9_]+", task.lower()) if len(item) >= 3 and item not in stopwords]
    entries = index.find(" ".join(tokens), limit=6) if tokens else []
    score: Dict[str, int] = {}; reasons: Dict[str, str] = {}
    selected_nodes = set()
    for entry in entries:
        selected_nodes.add(entry["id"])
        node = index.nodes[entry["id"]]
        if node.get("path"):
            score[node["path"]] = score.get(node["path"], 0) + entry["score"]; reasons[node["path"]] = "query:%s" % entry["id"]
        # An undirected expansion makes call/import neighbors useful whichever
        # side lexical matching found; Graph.trace intentionally has in/out API.
        for direction in ("out", "in"):
            for item in index.trace(entry["id"], direction=direction, depth=depth)["nodes"]:
                if len(selected_nodes) >= max_nodes: break
                selected_nodes.add(item["id"])
                node = index.nodes.get(item["id"], {})
                path = node.get("path", "")
                if path:
                    score[path] = score.get(path, 0) + max(1, entry["score"] - item["distance"] * 5)
                    reasons.setdefault(path, "neighbor:%s" % entry["id"])
    entry_files = sorted(score)
    blast = blast_radius(index, entry_files, depth=depth)
    for path in blast["files"]:
        score[path] = score.get(path, 0) + 3; reasons.setdefault(path, "blast")
    selected_paths = set(score)
    for edge in index.graph.get("edges", []):
        source = index.nodes.get(edge["source"], {})
        target = index.nodes.get(edge["target"], {})
        if source.get("kind") == "test" and target.get("path") in selected_paths:
            path = source.get("path", "")
            if path:
                score[path] = score.get(path, 0) + 4; reasons.setdefault(path, "connected-test")
    ordered = sorted(score, key=lambda path: (-score[path], path))[:max(0, max_files)]
    tests = sorted(path for path in blast["tests"] if path in ordered)
    degree = lambda ident: len(index.out.get(ident, [])) + len(index.ins.get(ident, []))
    hubs = sorted(({"id": ident, "degree": degree(ident)} for ident in selected_nodes if ident in index.nodes), key=lambda item: (-item["degree"], item["id"]))[:10]
    raw_bytes = sum((Path(repo) / path).stat().st_size for path in score if (Path(repo) / path).is_file())
    evidence = [{"path": path, "reason": reasons[path], "score": score[path]} for path in ordered]
    pack: Dict[str, Any] = {"task": task, "entries": [item["id"] for item in entries], "files": ordered, "tests": tests,
                            "blast": blast, "hubs": hubs, "evidence": evidence,
                            "byte_estimate": {"raw_candidate_files": raw_bytes, "pack": 0}}
    pack["byte_estimate"]["pack"] = len(canonical_json(pack))
    directory = ensure_private_dir(state_dir(Path(repo).resolve(), state) / "context")
    digest = sha256_bytes(canonical_json(pack))
    path = directory / (digest + ".json")
    pack["pack_digest"] = digest
    pack["pack_path"] = relative_path(path, Path(repo).resolve()) or ("context/" + path.name)
    atomic_write_json(path, pack)
    return pack


def compile_bundle(repo: Path, pack: Mapping[str, Any], *, max_bytes: int = 64 * 1024) -> Dict[str, Any]:
    """Reuse oms_runtime.context.plan_context(explicit=...) for the pack's files."""
    explicit = [(str(path), "project-graph: %s" % item.get("reason", "selected"))
                for item in pack.get("evidence", []) for path in [item.get("path", "")] if path]
    return plan_context(Path(repo).resolve(), explicit=explicit, max_bytes=max_bytes)
