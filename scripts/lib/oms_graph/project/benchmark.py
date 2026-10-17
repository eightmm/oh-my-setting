"""Opt-in history recall check: python3 -m oms_graph.project.benchmark --repo .

Current retrieval runs over each fix's parent tree, never the fixed source.
The 60-commit corpus is intentionally outside the default test lane.
"""

from __future__ import annotations

import argparse
import gzip
import io
import json
import os
import statistics
import subprocess
import tarfile
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any, Dict, List

from oms_runtime.common import canonical_json

from . import context
from .build import build, load_graph
from .extract import is_test_path
from .query import Graph


def _git(repo: Path, *args: str) -> bytes:
    return subprocess.check_output(["git", "-C", str(repo)] + list(args))


def gold_path(path: str) -> bool:
    """Exclude tests and prose, retaining executable templates and config."""
    return (not is_test_path(path)
            and PurePosixPath(path).suffix.lower() not in (".md", ".rst", ".txt", ".adoc")
            and PurePosixPath(path).parts[0] not in ("doc", "docs"))


def history_cases(repo: Path, count: int = 60, revision: str = "HEAD") -> List[Dict[str, Any]]:
    rows = _git(repo, "log", revision, "--no-merges", "--format=%H%x00%P%x00%s").decode("utf-8").splitlines()
    cases = []
    for row in rows:
        commit, parents, subject = row.split("\0", 2)
        if not subject.startswith("fix:") or not parents:
            continue
        parent = parents.split()[0]
        paths = _git(repo, "diff", "--name-only", "--no-renames", "-z", parent, commit).decode("utf-8").split("\0")
        gold = sorted(path for path in paths if path and gold_path(path))
        if gold:
            cases.append({"commit": commit, "parent": parent, "task": subject[4:].strip(), "gold": gold})
        if len(cases) >= count:
            break
    return cases


def export_parent(repo: Path, revision: str, target: Path) -> None:
    """Export regular Git archive members without following archive links."""
    data = _git(repo, "archive", "--format=tar", revision)
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        for member in archive:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts:
                raise ValueError("unsafe archive member")
            destination = target.joinpath(*path.parts)
            if member.isfile():
                destination.parent.mkdir(parents=True, exist_ok=True)
                source = archive.extractfile(member)
                if source is not None:
                    destination.write_bytes(source.read())


def summarize(rows: List[Dict[str, Any]]) -> Dict[str, Any]:
    return {"cases": len(rows),
            "recall_at_8": statistics.mean(row["recall_at_8"] for row in rows) if rows else 0,
            "hit_at_8": statistics.mean(bool(row["hits"]) for row in rows) if rows else 0,
            "median_pack_bytes": statistics.median(row["pack_bytes"] for row in rows) if rows else 0}


def run(repo: Path, *, count: int = 60, revision: str = "HEAD", cache: Path) -> Dict[str, Any]:
    cases = history_cases(repo, count, revision)
    if len(cases) != count:
        raise ValueError("requested %d qualifying fixes, found %d" % (count, len(cases)))
    cache.mkdir(parents=True, exist_ok=True)
    rows = []
    for number, case in enumerate(cases, 1):
        with tempfile.TemporaryDirectory(prefix="oms-context-benchmark.") as temporary:
            root = Path(temporary)
            tree, state = root / "tree", root / "state"
            tree.mkdir()
            export_parent(repo, case["parent"], tree)
            cached = cache / (case["parent"] + ".json.gz")
            previous = os.environ.get("OMS_PROJECT_GRAPH_STATE")
            os.environ["OMS_PROJECT_GRAPH_STATE"] = str(state)
            try:
                if cached.exists():
                    with gzip.open(cached, "rt", encoding="utf-8") as handle:
                        saved = json.load(handle)
                    graph = saved["graph"]
                    state.mkdir()
                    (state / "manifest.json").write_text(json.dumps(saved["manifest"]), encoding="utf-8")
                else:
                    extraction = cache / "extraction"
                    manifest = build(tree, state=extraction)
                    graph = load_graph(tree, state=extraction)
                    state.mkdir()
                    (state / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
                    pending = cached.with_suffix(".pending")
                    with gzip.open(pending, "wt", encoding="utf-8") as handle:
                        json.dump({"graph": graph, "manifest": manifest}, handle)
                    pending.replace(cached)
                index = Graph(graph)
                pack = context.select_context(tree, index, task=case["task"], state=state)
            finally:
                if previous is None:
                    os.environ.pop("OMS_PROJECT_GRAPH_STATE", None)
                else:
                    os.environ["OMS_PROJECT_GRAPH_STATE"] = previous
            entries = list(dict.fromkeys(index.nodes[ident]["path"] for ident in pack["entries"][:8]))
            hits = sorted(set(entries) & set(case["gold"]))
            row = dict(case, entries=entries, hits=hits, recall_at_8=len(hits) / len(case["gold"]),
                       pack_bytes=len(canonical_json(pack)))
            rows.append(row)
            print("%d/%d %s hits=%d/%d" % (number, count, case["commit"][:10], len(hits), len(case["gold"])), flush=True)
    return {"revision": _git(repo, "rev-parse", revision).decode().strip(),
            "definition": "macro file recall@8; all changed non-test/non-doc paths, including additions/deletions",
            "metrics": summarize(rows), "rows": rows}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--count", type=int, default=60)
    parser.add_argument("--revision", default="HEAD")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.count < 1:
        parser.error("--count must be positive")
    with tempfile.TemporaryDirectory(prefix="oms-context-benchmark-cache.") as temporary:
        result = run(args.repo.resolve(), count=args.count, revision=args.revision, cache=Path(temporary))
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result["metrics"], sort_keys=True))


if __name__ == "__main__":
    main()
