"""Task-specific context pack over the project graph (W2)."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_runtime.common import atomic_write_json, canonical_json, ensure_private_dir, relative_path, sha256_bytes
from oms_runtime.context import plan_context

from .blast import blast_radius, changed_paths
from .build import coverage as graph_coverage, state_dir
from . import analytics
from .query import Graph


def context_pack(repo: Path, graph: Any, *, task: str, max_files: int = 12, max_nodes: int = 40, depth: int = 2, base: str = "", state: Optional[Path] = None) -> Dict[str, Any]:
    """Write .oms/project-graph/context/<digest>.json and return the pack."""
    index = graph if isinstance(graph, Graph) else Graph(graph)
    # Generic task verbs describe the requested operation, not the project
    # concept to retrieve. Letting them compete with identifiers can make a
    # symbol such as ``changed`` outrank the actual subject of "change needle".
    stopwords = {
        "the", "and", "for", "with", "from", "that", "this", "into",
        "add", "change", "fix", "make", "modify", "update", "use",
        "are", "was", "will", "please",
    }
    tokens = [item for item in re.findall(r"[a-z0-9_]+", task.lower()) if len(item) >= 3 and item not in stopwords]
    raw_entries = index.find(" ".join(tokens), limit=max(24, max_files * 4)) if tokens else []
    entries = []
    entry_keys = set()
    entry_limit = min(8, max(0, max_files))
    for entry in raw_entries:
        if not entry_limit:
            break
        path = index.nodes[entry["id"]].get("path", "")
        key = ("path", path) if path else ("id", entry["id"])
        if key in entry_keys:
            continue
        entry_keys.add(key)
        entries.append(entry)
        if len(entries) >= entry_limit:
            break
    # Entry files always outrank expansion: a lexical hit is the strongest
    # signal, neighbor and blast contributions are capped so a hub test file
    # cannot crowd the pack, and tests are reported in their own list.
    score: Dict[str, int] = {}
    reasons: Dict[str, str] = {}
    neighbor_hits: Dict[str, int] = {}
    selected_nodes = set()
    entry_files: List[str] = []
    for entry in entries:
        selected_nodes.add(entry["id"])
        node = index.nodes[entry["id"]]
        path = node.get("path", "")
        if path:
            score[path] = score.get(path, 0) + 1000 + entry["score"]
            reasons[path] = "query:%s" % entry["id"]
            if path not in entry_files:
                entry_files.append(path)
    change_paths = {"changed": [], "untracked": []}  # type: Dict[str, List[str]]
    if base:
        change_paths = changed_paths(Path(repo), base=base)
        known_paths = {node.get("path", "") for node in index.nodes.values()}
        for path in sorted(set(change_paths["changed"]) | set(change_paths["untracked"])):
            if path not in known_paths:
                continue
            score[path] = score.get(path, 0) + 800
            reasons.setdefault(path, "changed:%s" % base)
            if path not in entry_files:
                entry_files.append(path)

    confidence_rank = {"EXTRACTED": 0, "INFERRED": 1, "AMBIGUOUS": 2}
    neighbor_candidates: Dict[str, Tuple[int, int, int, str]] = {}
    for entry_rank, entry in enumerate(entries):
        for direction in ("out", "in"):
            traced = index.trace(entry["id"], direction=direction, depth=depth,
                                 confidences=("EXTRACTED", "INFERRED"))
            for item in traced["nodes"]:
                if item["distance"] == 0:
                    continue
                candidate = (confidence_rank.get(item.get("confidence", "AMBIGUOUS"), 3),
                             item["distance"], entry_rank, entry["id"])
                if item["id"] not in neighbor_candidates or candidate < neighbor_candidates[item["id"]]:
                    neighbor_candidates[item["id"]] = candidate
    for ident, (_confidence, distance, _entry_rank, entry_id) in sorted(
            neighbor_candidates.items(), key=lambda item: (item[1], item[0])):
        if len(selected_nodes) >= max(0, max_nodes):
            break
        selected_nodes.add(ident)
        path = index.nodes.get(ident, {}).get("path", "")
        if not path or path in entry_files or neighbor_hits.get(path, 0) >= 3:
            continue
        neighbor_hits[path] = neighbor_hits.get(path, 0) + 1
        score[path] = score.get(path, 0) + max(10, 40 - distance * 10)
        reasons.setdefault(path, "neighbor:%s" % entry_id)

    blast = blast_radius(index, entry_files, depth=depth, confidences=("EXTRACTED", "INFERRED"))
    blast_paths: Dict[str, int] = {}
    test_quality: Dict[str, Tuple[int, int]] = {}
    for item in blast["dependents"]:
        path = index.nodes.get(item["id"], {}).get("path", "")
        if not path:
            continue
        blast_paths[path] = min(item["distance"], blast_paths.get(path, item["distance"]))
        if index.is_test(item["id"]):
            quality = (confidence_rank.get(item.get("confidence", "AMBIGUOUS"), 3), item["distance"])
            test_quality[path] = min(quality, test_quality.get(path, quality))
    for path, distance in sorted(blast_paths.items(), key=lambda item: (item[1], item[0]))[:max(0, max_nodes)]:
        if path not in entry_files:
            score[path] = score.get(path, 0) + max(5, 25 - distance * 5)
            reasons.setdefault(path, "blast:%d" % distance)

    is_test = lambda path: index.nodes.get("test:" + path, {}).get("kind") == "test"
    ordered = sorted(score, key=lambda path: (is_test(path) and path not in entry_files, -score[path], path))[:max(0, max_files)]
    tests = sorted(set(blast["tests"]) | set(path for path in score if is_test(path)),
                   key=lambda path: (test_quality.get(path, (4, depth + 1)), -score.get(path, 0), path))[:max(0, max_files)]
    test_order = {path: position for position, path in enumerate(tests)}
    max_cases = min(40, max(0, max_files) * 4)
    dependent_quality = {
        item["id"]: (confidence_rank.get(item.get("confidence", "AMBIGUOUS"), 3), item["distance"])
        for item in blast["dependents"]
    }
    case_tokens = [token for token in tokens if token not in ("test", "tests")]
    case_candidates = sorted(
        (row for row in blast["test_cases"] if row["path"] in test_order),
        key=lambda row: (-sum(token in row["name"].lower() for token in case_tokens),
                         test_order[row["path"]], dependent_quality.get(row["id"], (4, depth + 1)), row["id"]),
    )
    case_counts: Dict[str, int] = {}
    test_cases = []
    for row in case_candidates:
        if case_counts.get(row["path"], 0) >= 4:
            continue
        case_counts[row["path"]] = case_counts.get(row["path"], 0) + 1
        test_cases.append(row)
        if len(test_cases) >= max_cases:
            break
    evidence_degree = analytics.evidence_degrees(list(index.nodes.values()), index.graph.get("edges", []))
    hubs = sorted(({"id": ident, "degree": evidence_degree[ident]["total"]} for ident in selected_nodes if ident in index.nodes), key=lambda item: (-item["degree"], item["id"]))[:10]
    raw_bytes = sum((Path(repo) / path).stat().st_size for path in score if (Path(repo) / path).is_file())
    evidence = [{"path": path, "reason": reasons[path], "score": score[path]} for path in ordered]
    blast_view = {
        "seeds": blast["seeds"][:max(0, max_nodes)],
        "dependents": blast["dependents"][:max(0, max_nodes)],
        "files": blast["files"][:max(0, max_files)],
        "tests": tests,
        "test_cases": test_cases,
        "unmatched": blast["unmatched"][:max(0, max_files)],
        "path_coverage": blast["path_coverage"],
        "truncated": blast["truncated"],
        "counts": {"seeds": len(blast["seeds"]), "dependents": len(blast["dependents"]),
                   "files": len(blast["files"]), "tests": len(blast["tests"]),
                   "test_cases": len(blast["test_cases"])},
    }
    assurance_all = analytics.structural_assurance(
        list(index.nodes.values()), index.graph.get("edges", []),
    )
    path_order = {path: position for position, path in enumerate(ordered)}
    assurance_ids = set(selected_nodes)
    assurance_ids.update(
        ident for ident, node in index.nodes.items()
        if node.get("path") in path_order and node.get("kind") in ("file", "module")
    )
    entry_order = {entry["id"]: position for position, entry in enumerate(entries)}
    assurance_ids = sorted(
        (ident for ident in assurance_ids if ident in assurance_all["nodes"]),
        key=lambda ident: (
            0 if ident in entry_order else 1,
            entry_order.get(ident, len(entry_order)),
            path_order.get(str(index.nodes[ident].get("path", "")), len(path_order)),
            ident,
        ),
    )[:max(0, max_nodes)]
    assurance_rows = []
    assurance_counts: Dict[str, int] = {}
    for ident in assurance_ids:
        row = {"id": ident, "path": str(index.nodes[ident].get("path", "")),
               "kind": str(index.nodes[ident].get("kind", "node"))}
        row.update(assurance_all["nodes"][ident])
        assurance_rows.append(row)
        assurance_counts[row["assurance"]] = assurance_counts.get(row["assurance"], 0) + 1
    assurance_view = {
        "schema": assurance_all["schema"], "basis": assurance_all["basis"],
        "note": assurance_all["note"], "counts": dict(sorted(assurance_counts.items())),
        "nodes": assurance_rows,
    }
    change_seeds = sorted(set(change_paths["changed"]) | set(change_paths["untracked"]))
    change_blast = blast_radius(index, change_seeds, depth=depth, confidences=("EXTRACTED",))
    test_paths = set(change_blast["tests"])
    changed_set = set(change_seeds)
    impacted = sorted(set(
        str(index.nodes.get(row["id"], {}).get("path", ""))
        for row in change_blast["dependents"]
        if str(index.nodes.get(row["id"], {}).get("path", ""))
        and str(index.nodes.get(row["id"], {}).get("path", "")) not in changed_set
        and str(index.nodes.get(row["id"], {}).get("path", "")) not in test_paths
    ))
    change_view = {
        "schema": 1, "enabled": bool(base), "base": base,
        "impact_basis": "EXTRACTED reverse dependencies",
        "changed": list(change_paths["changed"]), "untracked": list(change_paths["untracked"]),
        "impacted": impacted, "unmatched": change_blast["unmatched"],
        "truncated": change_blast["truncated"],
    }
    pack: Dict[str, Any] = {"task": task, "project_graph_revision": str(index.graph.get("revision", "")),
                            "entries": [item["id"] for item in entries], "files": ordered,
                            "tests": tests, "test_cases": test_cases,
                            "blast": blast_view, "hubs": hubs, "evidence": evidence,
                            "assurance": assurance_view, "change": change_view,
                            "coverage": graph_coverage(Path(repo), state=state),
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
