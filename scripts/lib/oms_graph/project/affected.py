"""Fail-open affected-test projection over positive Project Graph evidence."""

from __future__ import annotations

import fnmatch
import subprocess
from pathlib import Path
from typing import Any, Dict, Mapping, Sequence

from oms_graph.errors import GraphError
from oms_runtime.common import run_output

from .blast import blast_radius
from .query import Graph


DEFAULT_BOUNDARIES = (
    ".github/*", ".gitlab/*", "Jenkinsfile", "AGENTS.md", "**/AGENTS.md",
    "CLAUDE.md", "**/CLAUDE.md", "GEMINI.md", "**/GEMINI.md", "PROJECT.md",
    "**/PROJECT.md", "SECURITY.md", "**/SECURITY.md", "VERSION", "Makefile",
    "makefile", "GNUmakefile", "package.json", "pyproject.toml", "setup.py",
    "setup.cfg", "tox.ini", "noxfile.py", "Cargo.toml", "go.mod", "go.sum",
    "pom.xml", "build.gradle", "build.gradle.kts", "*.lock", "*.lock.json",
    "*-lock.json", "*lock.yaml", "*lock.yml", "install.sh", "**/install*.sh",
    "**/uninstall*.sh", "**/check.sh", "**/run-smoke-shard.sh",
)

DOCUMENT_SUFFIXES = (".md", ".mdx", ".rst", ".txt", ".adoc")


def changed_entries(repo: Path, base: str, head: str = "HEAD") -> Sequence[Dict[str, str]]:
    """Return normalized name-status rows for an exact Git tree range."""
    for label, ref in (("base", base), ("head", head)):
        if not ref or not run_output(["git", "-C", str(repo), "rev-parse", "--verify", ref + "^{tree}"]):
            raise GraphError("affected %s is not a Git tree: %s" % (label, ref))
    raw = run_output(["git", "-C", str(repo), "diff", "--name-status", "-z", base, head, "--"])
    if not raw:
        return []
    fields = raw.split("\0")
    rows = []
    position = 0
    while position < len(fields) and fields[position]:
        raw_status = fields[position]
        position += 1
        status = raw_status[:1]
        if status in ("R", "C"):
            if position + 1 >= len(fields):
                raise GraphError("affected Git diff returned a truncated rename row")
            old_path, path = fields[position], fields[position + 1]
            position += 2
            rows.append({"status": status, "path": path.replace("\\", "/"),
                         "old_path": old_path.replace("\\", "/")})
        else:
            if position >= len(fields):
                raise GraphError("affected Git diff returned a truncated path row")
            path = fields[position]
            position += 1
            rows.append({"status": status, "path": path.replace("\\", "/")})
    return sorted(rows, key=lambda item: (item["path"], item["status"], item.get("old_path", "")))


def workspace_matches_head(repo: Path, head: str) -> bool:
    """The cache describes the requested head only when tracked and new files match it."""
    try:
        tracked = subprocess.run(
            ["git", "-C", str(repo), "diff", "--quiet", head, "--"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=20,
            check=False,
        )
        untracked = subprocess.run(
            ["git", "-C", str(repo), "ls-files", "--others", "--exclude-standard"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=20,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return False
    return tracked.returncode == 0 and untracked.returncode == 0 and not untracked.stdout.strip()


def _boundary(path: str, patterns: Sequence[str]) -> bool:
    return any(fnmatch.fnmatch(path, pattern) for pattern in patterns)


def _docs_only(paths: Sequence[str]) -> bool:
    return bool(paths) and all(Path(path).suffix.lower() in DOCUMENT_SUFFIXES for path in paths)


def _runnable_test(path: str) -> bool:
    pure = Path(path)
    return path.endswith(".sh") or (pure.name.startswith("test_") and path.endswith(".py"))


def affected_plan(
    graph: Any,
    paths: Sequence[str],
    *,
    changes: Sequence[Mapping[str, str]] = (),
    depth: int = 0,
    boundary_patterns: Sequence[str] = DEFAULT_BOUNDARIES,
    workspace_exact: bool = True,
) -> Dict[str, Any]:
    """Select positive test evidence or require the broader project gate."""
    if depth < 0:
        raise GraphError("affected depth must be zero or greater")
    index = graph if isinstance(graph, Graph) else Graph(graph)
    normalized = sorted(set(str(path).replace("\\", "/").lstrip("/") for path in paths if path))
    result = blast_radius(index, normalized, depth=None if depth == 0 else depth,
                          confidences=("EXTRACTED",))
    tests = [path for path in result["tests"] if _runnable_test(path)]
    test_paths = set(tests)
    test_cases = [row for row in result["test_cases"] if row["path"] in test_paths]
    reasons = []
    if not workspace_exact:
        reasons.append("workspace-differs-from-head")
    reasons.extend("unmatched:%s" % path for path in result["unmatched"])
    reasons.extend("boundary:%s" % path for path in normalized if _boundary(path, boundary_patterns))
    if result["truncated"]:
        reasons.append("depth-truncated")
    for change in changes:
        path = str(change.get("path", "")).replace("\\", "/").lstrip("/")
        status = str(change.get("status", ""))[:1]
        if status == "D":
            reasons.append("deleted:%s" % path)
        elif status in ("R", "C"):
            reasons.append("renamed:%s" % path)
    confidence_counts: Dict[str, int] = {}
    for row in result["dependents"]:
        confidence = str(row.get("confidence", "AMBIGUOUS"))
        confidence_counts[confidence] = confidence_counts.get(confidence, 0) + 1
    reached = set(result["seeds"]) | {row["id"] for row in result["dependents"]}
    ignored_confidence_counts: Dict[str, int] = {}
    ignored_edges = set()
    for target in reached:
        for edge in index.ins.get(target, []):
            confidence = str(edge.get("confidence", "AMBIGUOUS"))
            key = (edge["source"], edge["target"], edge["relation"], confidence)
            if confidence != "EXTRACTED" and edge["source"] not in reached and key not in ignored_edges:
                ignored_edges.add(key)
                ignored_confidence_counts[confidence] = ignored_confidence_counts.get(confidence, 0) + 1
    if normalized and not tests and not _docs_only(normalized):
        reasons.append("no-tests")
    return {
        "schema": 1,
        "mode": "full" if reasons else "affected",
        "depth": int(depth),
        "paths": normalized,
        "tests": tests,
        "test_cases": test_cases,
        "unsupported_test_count": len(result["tests"]) - len(tests),
        "confidence_counts": dict(sorted(confidence_counts.items())),
        "ignored_confidence_counts": dict(sorted(ignored_confidence_counts.items())),
        "path_coverage": result["path_coverage"],
        "unmatched": result["unmatched"],
        "reasons": sorted(set(reasons)),
    }
