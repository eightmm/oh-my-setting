"""Changed paths -> transitive dependents (W2)."""

from __future__ import annotations

from collections import deque
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence

from oms_graph.errors import GraphError
from oms_runtime.common import run_output
from .query import Graph

DEFAULT_RELATIONS = ("imports", "calls", "references", "tests", "uses", "depends_on")


def changed_paths(repo: Path, *, base: str = "") -> Dict[str, List[str]]:
    """{"changed": [...], "untracked": [...]} from git; never writes."""
    repo = Path(repo)
    if base.startswith("-") or "\x00" in base:
        raise GraphError("invalid base ref: %s" % base)
    head = run_output(["git", "-C", str(repo), "rev-parse", "--verify", "HEAD"])
    if base:
        base_commit = run_output(["git", "-C", str(repo), "rev-parse", "--verify", base + "^{commit}"])
        if not base_commit or not head:
            raise GraphError("cannot resolve base and HEAD: %s" % base)
        ref = run_output(["git", "-C", str(repo), "merge-base", base_commit, head])
        if not ref:
            raise GraphError("base has no merge-base with HEAD: %s" % base)
        changed_text = run_output(["git", "-C", str(repo), "diff", "--name-only", ref, "--"])
    elif head:
        changed_text = run_output(["git", "-C", str(repo), "diff", "--name-only", "HEAD", "--"])
    else:
        # Before the first commit there is no HEAD tree. Both index and
        # worktree deltas matter: a staged new file must not disappear from a
        # blast/context change projection.
        cached = run_output(["git", "-C", str(repo), "diff", "--name-only", "--cached", "--"])
        unstaged = run_output(["git", "-C", str(repo), "diff", "--name-only", "--"])
        changed_text = "\n".join(item for item in (cached, unstaged) if item)
    changed = [item for item in changed_text.splitlines() if item]
    # `--untracked-files=all` names each new file; the default collapses a new
    # directory to its own name, which no graph node ever matches.
    status = run_output(["git", "-C", str(repo), "status", "--porcelain", "--untracked-files=all"])
    untracked = sorted(line[3:] for line in status.splitlines() if line.startswith("?? "))
    return {"changed": sorted(set(changed)), "untracked": untracked}


def blast_radius(
    graph: Any,
    paths: Sequence[str],
    *,
    depth: Optional[int] = 3,
    relations: Sequence[str] = DEFAULT_RELATIONS,
    confidences: Sequence[str] = (),
) -> Dict[str, Any]:
    """Reverse dependents plus path coverage for the requested seeds."""
    if depth is not None and depth < 0:
        raise GraphError("blast depth must be zero or greater")
    index = graph if isinstance(graph, Graph) else Graph(graph)
    seeds = []
    matched_paths = set()
    for path in paths:
        for ident, node in index.nodes.items():
            if node.get("path") == path:
                seeds.append(ident)
                matched_paths.add(path)
    seeds = sorted(set(seeds))
    seen = {ident: {"id": ident, "distance": 0, "via": ""} for ident in seeds}
    queue = deque(seeds)
    while queue:
        current = queue.popleft()
        distance = seen[current]["distance"]
        if depth is not None and distance >= depth:
            continue
        for edge in index.ins.get(current, []):
            if edge["relation"] not in relations:
                continue
            if confidences and edge.get("confidence", "AMBIGUOUS") not in confidences:
                continue
            adjacent = edge["source"]
            if adjacent not in seen:
                seen[adjacent] = {"id": adjacent, "distance": distance + 1,
                                  "via": edge["relation"], "confidence": edge.get("confidence", "AMBIGUOUS")}
                queue.append(adjacent)
    truncated = bool(depth is not None and any(
        row["distance"] == depth
        and any(edge["relation"] in relations
                and (not confidences or edge.get("confidence", "AMBIGUOUS") in confidences)
                and edge["source"] not in seen
                for edge in index.ins.get(ident, []))
        for ident, row in seen.items()
    ))
    dependents = sorted((row for ident, row in seen.items() if ident not in seeds), key=lambda item: (item["distance"], item["id"]))
    affected = seeds + [row["id"] for row in dependents]
    files = sorted(set(index.nodes[ident].get("path", "") for ident in affected if ident in index.nodes and index.nodes[ident].get("path")))
    test_paths = set(node.get("path", "") for node in index.nodes.values() if node.get("kind") == "test")
    tests = sorted(path for path in set(files) if path in test_paths)
    test_cases = sorted(({
        "id": ident,
        "language": index.nodes[ident].get("language", ""),
        "name": index.nodes[ident].get("name", ""),
        "path": index.nodes[ident].get("path", ""),
    } for ident in affected
        if ident in index.nodes
        and index.nodes[ident].get("path") in test_paths
        and index.nodes[ident].get("kind") in ("function", "method")
        and str(index.nodes[ident].get("name", "")).startswith("test_")), key=lambda item: item["id"])
    unmatched = sorted(set(paths) - matched_paths)
    return {"seeds": seeds, "dependents": dependents, "files": files, "tests": tests, "test_cases": test_cases,
            "unmatched": unmatched, "path_coverage": "partial" if unmatched else "complete",
            "truncated": truncated}
