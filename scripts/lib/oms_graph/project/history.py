"""Co-change coupling from Git history: what changes together, whether or not the parsers can see why.

Temporal coupling (Code Maat's measure): two paths are coupled by the share of
their revisions they spent in the same commit,
``degree = shared_revs / average(revs_a, revs_b) * 100``. It is derived from
`git log` alone, so it catches coupling no syntactic parser extracts — config
to code, fixture to module, shell to Python — and it is never presented as a
dependency: every pair is annotated with whether the Project Graph holds a
structural edge between the two paths, so `structural: false` is exactly the
coupling the parsers cannot see. Nothing here is stored in `graph.json`; the
result is a projection of history at the current HEAD.
"""

from __future__ import annotations

from collections import Counter
from pathlib import Path
from typing import Any, Dict, Iterable, List, Mapping, Optional, Sequence, Set, Tuple

from oms_runtime.common import run_output

DEFAULT_COMMITS = 500
# Code Maat's `--max-changeset-size` idea: a commit that touches this many
# files is a bulk move, rename, or formatting pass, and would couple everything.
DEFAULT_MAX_CHANGESET = 50
DEFAULT_MIN_SHARED = 5
DEFAULT_MIN_DEGREE = 30
LOG_TIMEOUT = 60


def changesets(repo: Path, *, commits: int = DEFAULT_COMMITS, max_changeset: int = DEFAULT_MAX_CHANGESET,
               paths: Optional[Set[str]] = None) -> Dict[str, Any]:
    """Per-commit file sets from `git log`, newest first; bulk commits are counted, not used.

    `paths` restricts every set to the files that exist now (a deleted file
    cannot be coupled to anything), which is what `coupling_report` passes."""
    raw = run_output(["git", "-C", str(repo), "log", "-n", str(max(1, int(commits))), "--no-merges", "--name-only",
                      "--format=%x1e%H%x09%ct", "--", "."], cwd=repo, timeout=LOG_TIMEOUT)
    sets: List[Dict[str, Any]] = []
    skipped_bulk = 0
    for chunk in raw.split("\x1e"):
        lines = [line.strip() for line in chunk.splitlines() if line.strip()]
        if not lines:
            continue
        head, _, stamp = lines[0].partition("\t")
        files = sorted({line.replace("\\", "/") for line in lines[1:]})
        if paths is not None:
            files = [item for item in files if item in paths]
        if not files:
            continue
        if len(files) > max_changeset:
            skipped_bulk += 1
            continue
        try:
            when = int(stamp)
        except ValueError:
            when = 0
        sets.append({"commit": head, "time": when, "files": files})
    return {"commits": len(sets), "skipped_bulk": skipped_bulk, "sets": sets}


def coupling(sets: Sequence[Mapping[str, Any]], *, min_shared: int = DEFAULT_MIN_SHARED, min_degree: float = DEFAULT_MIN_DEGREE,
             focus: Optional[Set[str]] = None) -> List[Dict[str, Any]]:
    """Pure: coupled pairs, strongest first. `focus` keeps pairs touching at least one focus path."""
    revisions: Counter = Counter()
    shared: Counter = Counter()
    for entry in sets:
        files = sorted(set(entry.get("files", [])))
        for path in files:
            revisions[path] += 1
        for index, first in enumerate(files):
            for second in files[index + 1:]:
                if focus and first not in focus and second not in focus:
                    continue
                shared[(first, second)] += 1
    rows: List[Dict[str, Any]] = []
    for (first, second), count in shared.items():
        if count < min_shared:
            continue
        average = (revisions[first] + revisions[second]) / 2.0
        degree = round(100.0 * count / average, 1) if average else 0.0
        if degree < min_degree:
            continue
        rows.append({"a": first, "b": second, "shared_revs": count,
                     "revs": [revisions[first], revisions[second]], "degree": degree})
    rows.sort(key=lambda row: (-row["degree"], -row["shared_revs"], row["a"], row["b"]))
    return rows


def structural_pairs(graph: Mapping[str, Any]) -> Set[Tuple[str, str]]:
    """Path pairs the Project Graph connects by any non-`contains` edge, in either direction."""
    path_of = {node.get("id"): node.get("path", "") for node in graph.get("nodes", [])}
    pairs: Set[Tuple[str, str]] = set()
    for edge in graph.get("edges", []):
        if edge.get("relation") == "contains":
            continue
        first, second = path_of.get(edge.get("source"), ""), path_of.get(edge.get("target"), "")
        if first and second and first != second:
            pairs.add((min(first, second), max(first, second)))
    return pairs


def annotate(rows: Iterable[Mapping[str, Any]], structural: Set[Tuple[str, str]]) -> List[Dict[str, Any]]:
    result = []
    for row in rows:
        item = dict(row)
        item["structural"] = (min(item["a"], item["b"]), max(item["a"], item["b"])) in structural
        result.append(item)
    return result


def coupling_report(repo: Path, graph: Optional[Mapping[str, Any]], *, commits: int = DEFAULT_COMMITS,
                    max_changeset: int = DEFAULT_MAX_CHANGESET, min_shared: int = DEFAULT_MIN_SHARED,
                    min_degree: float = DEFAULT_MIN_DEGREE, focus: Sequence[str] = (), limit: int = 40) -> Dict[str, Any]:
    """The CLI's payload: bounded pairs plus how the history window was read."""
    repo = Path(repo).resolve()
    # Every tracked path, not only what the parsers read: a config or data file
    # that moves with the code is exactly the coupling this report exists for.
    present = {line.replace("\\", "/") for line in run_output(["git", "-C", str(repo), "ls-files"], cwd=repo, timeout=LOG_TIMEOUT).splitlines() if line}
    window = changesets(repo, commits=commits, max_changeset=max_changeset, paths=present)
    focus_set = {item.replace("\\", "/") for item in focus if item}
    rows = coupling(window["sets"], min_shared=min_shared, min_degree=min_degree, focus=focus_set or None)
    rows = annotate(rows, structural_pairs(graph) if graph else set())
    bound = max(1, int(limit)) if limit else len(rows)
    return {
        "schema": 1,
        "commits": window["commits"],
        "skipped_bulk": window["skipped_bulk"],
        "params": {"commits": int(commits), "max_changeset": int(max_changeset), "min_shared": int(min_shared),
                   "min_degree": float(min_degree), "focus": sorted(focus_set)},
        "graph_revision": str(graph.get("revision", "")) if graph else "",
        "pairs": rows[:bound],
        "hidden": sum(1 for row in rows if not row["structural"]),
        "truncated": len(rows) > bound,
        "omitted": max(0, len(rows) - bound),
    }
