"""Task-specific context pack over the project graph (W2)."""

from __future__ import annotations

import hashlib
import math
import re
from collections import Counter
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_runtime.common import atomic_write_json, canonical_json, ensure_private_dir, parse_path_list, relative_path, sha256_bytes
from oms_runtime.context import plan_context

from .blast import blast_radius, changed_paths
from .build import coverage as graph_coverage, state_dir
from . import analytics
from .query import Graph, _identifier_words
from ..errors import GraphError

_QUOTED_SOURCE = re.compile(r"""(['"])(.*?)(?<!\\)\1""", re.S)


def _measure_pack(pack: Dict[str, Any]) -> None:
    """Count canonical JSON payload bytes, including the size field itself."""
    while True:
        size = len(canonical_json(pack))
        if pack["pack_bytes"] == size:
            return
        pack["pack_bytes"] = size


def _rank_entries(repo: Path, index: Graph, tokens: Sequence[str]) -> List[Dict[str, Any]]:
    """BM25 field scores; source is read only when it still matches the graph.

    Quoted fragments supply option/error text absent from short summaries.
    Source text stays local: only node IDs and scores enter the context pack.
    """
    terms = list(dict.fromkeys(tokens))
    if not terms:
        return []
    root = Path(repo).resolve()
    sources: Dict[str, List[str]] = {}
    candidates = [node for node in index.nodes.values() if not index.is_test(node["id"])]
    for node in candidates:
        path = str(node.get("path", ""))
        if path in sources:
            continue
        sources[path] = []
        try:
            target = root / path
            target.resolve().relative_to(root)
            if target.is_symlink() or not target.is_file() or target.stat().st_size > 2 * 1024 * 1024:
                continue
            data = target.read_bytes()
            if hashlib.sha256(data).hexdigest() == node.get("source_digest"):
                sources[path] = data.decode("utf-8", "replace").splitlines()
        except (OSError, ValueError):
            continue

    documents = []
    for node in candidates:
        metadata = node.get("metadata") or {}
        lines = sources.get(str(node.get("path", "")), [])
        start = int(metadata.get("line", 0) or 0)
        end = int(metadata.get("end_line", start) or start)
        body = "\n".join(lines[max(0, start - 1):end] if start else lines)
        quoted = " ".join(match.group(2) for match in _QUOTED_SOURCE.finditer(body))
        fields = (str(node.get("name", "")) + " " + str(metadata.get("qualname", "")),
                  str(node.get("path", "")), str(node.get("summary") or ""), quoted)
        counters = [Counter(_identifier_words(field)) for field in fields]
        documents.append((node, counters, [sum(counter.values()) for counter in counters]))
    count = len(documents)
    averages = [sum(lengths[field] for _, _, lengths in documents) / max(1, count)
                for field in range(4)]
    frequencies = {term: sum(any(term in counter for counter in counters)
                             for _, counters, _ in documents) for term in terms}
    weights = {term: math.log(1 + (count - frequencies[term] + 0.5) / (frequencies[term] + 0.5))
               for term in terms}
    rows = []
    for node, counters, lengths in documents:
        score = 0.0
        for term in terms:
            for field, counter in enumerate(counters):
                frequency = counter[term]
                if frequency:
                    normalizer = 1.2 * (0.25 + 0.75 * lengths[field] / max(1, averages[field]))
                    score += (3.0, 1.0, 1.5, 0.6)[field] * weights[term] * frequency * 2.2 / (frequency + normalizer)
        if score:
            rows.append({"id": node["id"], "score": round(score * 100)})
    # Do not truncate symbols before the caller deduplicates files: a large
    # module's matching symbols must not consume the entire candidate window.
    return sorted(rows, key=lambda row: (-row["score"], not row["id"].startswith("symbol:"), row["id"]))


def select_context(repo: Path, graph: Any, *, task: str, max_files: int = 12, max_nodes: int = 40, depth: int = 2, base: str = "", state: Optional[Path] = None, entry_paths: Sequence[str] = ()) -> Dict[str, Any]:
    """Rank context without writing; exact paths bypass lexical retrieval."""
    entry_paths = [str(path).replace("\\", "/") for path in entry_paths]
    if any(parse_path_list([path]) != [path] for path in entry_paths):
        raise GraphError("entry paths must be normalized repo-relative file paths")
    index = graph if isinstance(graph, Graph) else Graph(graph)
    # Generic task verbs describe the requested operation, not the project
    # concept to retrieve. Letting them compete with identifiers can make a
    # symbol such as ``changed`` outrank the actual subject of "change needle".
    stopwords = {
        "the", "and", "for", "with", "from", "that", "this", "into",
        "add", "change", "fix", "make", "modify", "update", "use",
        "are", "was", "will", "please",
    }
    # Filter whole words before splitting identifiers: `update_cache` is a
    # subject, not the generic verb `update`. Short non-ASCII words are useful.
    tokens = [token for word in re.findall(r"\w+", task)
              if word.casefold() not in stopwords
              for token in _identifier_words(word) if len(token) > 1 or not token.isascii()]
    if entry_paths:
        raw_entries = [{"id": ident, "score": 100} for path in entry_paths
                       for ident in ("file:" + path, "test:" + path) if ident in index.nodes]
    else:
        raw_entries = _rank_entries(Path(repo), index, tokens)
    entries = []
    entry_keys = set()
    entry_limit = min(len(entry_paths) if entry_paths else 8, max(0, max_files))
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
                            "pack_bytes": 0}
    unmatched = [path for path in entry_paths if "file:" + path not in index.nodes
                 and "test:" + path not in index.nodes]
    pack["retrieval"] = {
        "method": "entry-path" if entry_paths else "lexical",
        "status": "no-match" if not entries else "partial" if unmatched else "matched",
        "unmatched_entry_paths": unmatched,
        "hint": "" if entries and not unmatched else (
            "No complete match. Search source identifiers/paths directly, then use --entry-path. "
            "Lexical search does not translate the task; missing matches do not prove missing code."
        ),
    }
    _measure_pack(pack)
    return pack


def context_pack(repo: Path, graph: Any, *, task: str, max_files: int = 12, max_nodes: int = 40, depth: int = 2, base: str = "", state: Optional[Path] = None, entry_paths: Sequence[str] = ()) -> Dict[str, Any]:
    """Write .oms/project-graph/context/<digest>.json and return the pack."""
    pack = select_context(repo, graph, task=task, max_files=max_files,
                          max_nodes=max_nodes, depth=depth, base=base, state=state, entry_paths=entry_paths)
    directory = ensure_private_dir(state_dir(Path(repo).resolve(), state) / "context")
    digest = sha256_bytes(canonical_json(pack))
    path = directory / (digest + ".json")
    pack["pack_digest"] = digest
    pack["pack_path"] = relative_path(path, Path(repo).resolve()) or ("context/" + path.name)
    _measure_pack(pack)
    atomic_write_json(path, pack)
    return pack


def compile_bundle(repo: Path, pack: Mapping[str, Any], *, max_bytes: int = 64 * 1024) -> Dict[str, Any]:
    """Reuse oms_runtime.context.plan_context(explicit=...) for the pack's files."""
    explicit = [(str(path), "project-graph: %s" % item.get("reason", "selected"))
                for item in pack.get("evidence", []) for path in [item.get("path", "")] if path]
    return plan_context(Path(repo).resolve(), explicit=explicit, max_bytes=max_bytes)
