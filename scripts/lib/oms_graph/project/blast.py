"""Changed paths -> transitive dependents (W2)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_runtime.common import run_output
from .model import node_id
from .query import Graph

DEFAULT_RELATIONS = ("imports", "calls", "references", "tests", "uses", "depends_on")


def changed_paths(repo: Path, *, base: str = "") -> Dict[str, List[str]]:
    """{"changed": [...], "untracked": [...]} from git; never writes."""
    ref = base or "HEAD"
    changed = [item for item in run_output(["git", "-C", str(repo), "diff", "--name-only", ref]).splitlines() if item]
    status = run_output(["git", "-C", str(repo), "status", "--porcelain"])
    untracked = sorted(line[3:] for line in status.splitlines() if line.startswith("?? "))
    return {"changed": sorted(set(changed)), "untracked": untracked}


def blast_radius(graph: Any, paths: Sequence[str], *, depth: int = 3, relations: Sequence[str] = DEFAULT_RELATIONS) -> Dict[str, Any]:
    """{"seeds","dependents":[{"id","distance","via"}],"files","tests"}."""
    index = graph if isinstance(graph, Graph) else Graph(graph)
    seeds = []
    for path in paths:
        for ident, node in index.nodes.items():
            if node.get("path") == path and node.get("kind") in ("file", "test", "class", "function", "method", "symbol"):
                seeds.append(ident)
    seeds = sorted(set(seeds))
    seen = {ident: {"id": ident, "distance": 0, "via": ""} for ident in seeds}
    queue = list(seeds)
    while queue:
        current = queue.pop(0); distance = seen[current]["distance"]
        if distance >= depth: continue
        for edge in index.ins.get(current, []):
            if edge["relation"] not in relations: continue
            adjacent = edge["source"]
            if adjacent not in seen:
                seen[adjacent] = {"id": adjacent, "distance": distance + 1, "via": edge["relation"]}; queue.append(adjacent)
    dependents = sorted((row for ident, row in seen.items() if ident not in seeds), key=lambda item: (item["distance"], item["id"]))
    affected = seeds + [row["id"] for row in dependents]
    files = sorted(set(index.nodes[ident].get("path", "") for ident in affected if ident in index.nodes and index.nodes[ident].get("path")))
    tests = sorted(set(index.nodes[ident].get("path", "") for ident in affected if ident in index.nodes and index.nodes[ident].get("kind") == "test"))
    return {"seeds": seeds, "dependents": dependents, "files": files, "tests": tests}
