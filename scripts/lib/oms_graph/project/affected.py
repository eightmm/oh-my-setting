"""Fail-open affected-test projection over positive Project Graph evidence."""

from __future__ import annotations

import fnmatch
import re
import subprocess
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Set

from oms_graph.errors import GraphError
from oms_runtime.common import run_output

from .blast import DEFAULT_RELATIONS, blast_radius
from .parsers import parser_for
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

DOCUMENT_SUFFIXES = (".md", ".mdx", ".rst", ".adoc")


_HUNK_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
_TEST_DEFINITION_RE = re.compile(
    r"^\s*(?:async\s+)?def\s+(test_[A-Za-z0-9_]*)\s*\(|"
    r"^\s*function\s+(test_[A-Za-z0-9_]*)\b|"
    r"^\s*(test_[A-Za-z0-9_]*)\s*\(\s*\)\s*(?:\{|$)"
)


def _test_diff_hunks(repo: Path, base: str, head: str, path: str) -> Sequence[Dict[str, Any]]:
    """Read exact changed lines on both sides; an unreadable diff fails open."""
    try:
        result = subprocess.run(
            ["git", "-C", str(repo), "diff", "--no-ext-diff", "--no-textconv", "--no-color",
             "--unified=0", base, head, "--", ":(literal)" + path],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=30, check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return ()
    if result.returncode != 0:
        return ()
    try:
        diff_text = result.stdout.decode("utf-8")
    except UnicodeError:
        return ()
    hunks: List[Dict[str, Any]] = []
    current: Optional[Dict[str, Any]] = None
    old_line = new_line = 0
    for raw in diff_text.split("\n"):
        if raw.startswith("diff --git "):
            current = None
            continue
        match = _HUNK_RE.match(raw)
        if match:
            old_line, old_count, new_line, new_count = match.groups()
            current = {"old_start": int(old_line), "old_count": int(old_count or "1"),
                       "new_start": int(new_line), "new_count": int(new_count or "1"),
                       "old_lines": [], "new_lines": []}
            hunks.append(current)
            old_line, new_line = int(old_line), int(new_line)
        elif current is not None and raw.startswith("-") and not raw.startswith("---"):
            current["old_lines"].append((old_line, raw[1:]))
            old_line += 1
        elif current is not None and raw.startswith("+") and not raw.startswith("+++"):
            current["new_lines"].append((new_line, raw[1:]))
            new_line += 1
        elif current is not None and raw.startswith(" "):
            old_line += 1
            new_line += 1
    return hunks


def changed_entries(repo: Path, base: str, head: str = "HEAD") -> Sequence[Dict[str, Any]]:
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


def changed_test_hunks(
    repo: Path,
    base: str,
    head: str,
    changes: Sequence[Mapping[str, Any]],
) -> Dict[str, Sequence[Dict[str, Any]]]:
    """Return diff hunks separately from the stable name-status rows."""
    return {
        str(row["path"]): _test_diff_hunks(repo, base, head, str(row["path"]))
        for row in changes
        if str(row.get("status", ""))[:1] == "M" and _runnable_test(str(row.get("path", "")))
    }


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


def _documentation_path(path: str) -> bool:
    pure = Path(path)
    if pure.suffix.lower() not in DOCUMENT_SUFFIXES:
        return False
    return (pure.parts[0] in ("doc", "docs")
            or pure.name.upper().startswith(("README.", "CHANGELOG.", "CONTRIBUTING.")))


def _runnable_test(path: str) -> bool:
    pure = Path(path)
    return path.endswith(".sh") or (pure.name.startswith("test_") and path.endswith(".py"))


def _revision_text(repo: Path, revision: str, path: str) -> Optional[str]:
    try:
        result = subprocess.run(
            ["git", "-C", str(repo), "show", "%s:%s" % (revision, path)],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=30, check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if result.returncode != 0:
        return None
    try:
        return result.stdout.decode("utf-8")
    except UnicodeError:
        return None


def _test_ranges(nodes: Sequence[Mapping[str, Any]], path: str, text: str) -> Optional[Dict[tuple, Dict[str, Any]]]:
    lines = text.split("\n")
    ranges = {}
    for node in nodes:
        name = str(node.get("name", ""))
        if (node.get("path") != path or node.get("kind") not in ("function", "method")
                or not name.startswith("test_") or node.get("language") not in ("python", "shell")):
            continue
        metadata = node.get("metadata") if isinstance(node.get("metadata"), Mapping) else {}
        definition_line, end = metadata.get("line"), metadata.get("end_line")
        if (not isinstance(definition_line, int) or not isinstance(end, int)
                or definition_line < 1 or end < definition_line or end > len(lines)):
            return None
        start = definition_line
        while (start > 1 and lines[start - 2].lstrip().startswith("#")
               and not lines[start - 2].lstrip().startswith("#!")):
            start -= 1
        key = (str(node.get("kind", "")), str(metadata.get("qualname") or name))
        if key in ranges:
            return None
        ranges[key] = {
            "id": node.get("id"), "language": node.get("language", ""), "name": name,
            "path": path, "start": start, "definition_line": definition_line, "end": end,
        }
    return ranges


def _parsed_test_ranges(path: str, text: str) -> Optional[Dict[tuple, Dict[str, Any]]]:
    parser = parser_for(path)
    if parser is None:
        return None
    try:
        parsed = parser.parse(path, text, "")
    except Exception:
        return None
    return _test_ranges(parsed.nodes, path, text)


def _changed_test_cases(
    graph: Graph,
    path: str,
    hunks: Sequence[Mapping[str, Any]],
    old_text: str,
    new_text: str,
) -> Optional[List[Dict[str, str]]]:
    """Narrow only when each hunk maps to one same-named case on both revisions."""
    test_file = any(node.get("kind") == "test" and node.get("path") == path for node in graph.nodes.values())
    if not test_file or not hunks:
        return None
    new_ranges = _test_ranges(
        [node for node in graph.nodes.values() if node.get("path") == path], path, new_text)
    old_ranges = _parsed_test_ranges(path, old_text)
    if new_ranges is None or old_ranges is None or not new_ranges or not old_ranges:
        return None

    def rows_inside(rows: Sequence[Sequence[Any]], case: Mapping[str, Any]) -> bool:
        return bool(rows) and all(case["start"] <= int(number) <= case["end"] for number, _ in rows)

    def empty_side_inside(
        point: int,
        case: Mapping[str, Any],
        other_rows: Sequence[Sequence[Any]],
        other_case: Mapping[str, Any],
    ) -> bool:
        if case["start"] <= point <= case["end"]:
            return True
        return (
            point == case["definition_line"] - 1
            and bool(other_rows)
            and all(str(line).lstrip().startswith("#") for _, line in other_rows)
            and max(int(number) for number, _ in other_rows) == other_case["definition_line"] - 1
        )

    selected = set()
    for hunk in hunks:
        old_definitions = {match.group(1) or match.group(2) or match.group(3)
                           for _, line in hunk.get("old_lines", [])
                           for match in [_TEST_DEFINITION_RE.match(line)] if match}
        new_definitions = {match.group(1) or match.group(2) or match.group(3)
                           for _, line in hunk.get("new_lines", [])
                           for match in [_TEST_DEFINITION_RE.match(line)] if match}
        if old_definitions != new_definitions:
            return None
        old_start, old_count = int(hunk["old_start"]), int(hunk["old_count"])
        new_start, new_count = int(hunk["new_start"]), int(hunk["new_count"])
        old_rows, new_rows = hunk.get("old_lines", []), hunk.get("new_lines", [])
        if old_count != len(old_rows) or new_count != len(new_rows):
            return None
        matches = []
        for key in new_ranges.keys() & old_ranges.keys():
            old_case, new_case = old_ranges[key], new_ranges[key]
            old_inside = (rows_inside(old_rows, old_case) if old_rows else
                          empty_side_inside(old_start, old_case, new_rows, new_case))
            new_inside = (rows_inside(new_rows, new_case) if new_rows else
                          empty_side_inside(new_start, new_case, old_rows, old_case))
            if old_inside and new_inside:
                matches.append(new_case)
        if len(matches) != 1:
            return None
        selected.add(matches[0]["id"])
    return [{key: candidate[key] for key in ("id", "language", "name", "path")}
            for candidate in new_ranges.values() if candidate["id"] in selected]


def _case_containment_seeds(
    graph: Graph,
    path: str,
    cases: Sequence[Mapping[str, Any]],
) -> Optional[Set[str]]:
    """Return edited cases and all containing nodes, or fail open if incomplete."""
    if not cases:
        return None
    seeds: Set[str] = set()
    root_kinds = ("test", "file", "module")
    for case in cases:
        case_id = str(case.get("id", ""))
        case_node = graph.nodes.get(case_id)
        if (not case_node or case_node.get("path") != path
                or case_node.get("kind") not in ("function", "method")):
            return None
        state: Dict[str, int] = {}
        stack = [(case_id, False)]
        reached_root = False
        while stack:
            ident, exiting = stack.pop()
            if exiting:
                state[ident] = 2
                seeds.add(ident)
                continue
            status = state.get(ident, 0)
            if status == 1:
                return None
            if status == 2:
                continue
            node = graph.nodes.get(ident)
            if not node or node.get("path") != path:
                return None
            parents = [edge for edge in graph.ins.get(ident, [])
                       if edge.get("relation") == "contains"]
            if not parents:
                if node.get("kind") not in root_kinds:
                    return None
                state[ident] = 2
                seeds.add(ident)
                reached_root = True
                continue
            state[ident] = 1
            stack.append((ident, True))
            for edge in parents:
                parent_id = str(edge.get("source", ""))
                parent = graph.nodes.get(parent_id)
                if not parent or parent.get("path") != path or state.get(parent_id) == 1:
                    return None
                if state.get(parent_id) != 2:
                    stack.append((parent_id, False))
        if not reached_root:
            return None
    return seeds


def affected_plan(
    graph: Any,
    paths: Sequence[str],
    *,
    changes: Sequence[Mapping[str, str]] = (),
    test_hunks: Optional[Mapping[str, Sequence[Mapping[str, Any]]]] = None,
    repo: Optional[Path] = None,
    base: str = "",
    head: str = "HEAD",
    depth: int = 0,
    boundary_patterns: Sequence[str] = DEFAULT_BOUNDARIES,
    workspace_exact: bool = True,
) -> Dict[str, Any]:
    """Select positive test evidence or require the broader project gate."""
    if depth < 0:
        raise GraphError("affected depth must be zero or greater")
    index = graph if isinstance(graph, Graph) else Graph(graph)
    normalized = sorted(set(str(path).replace("\\", "/").lstrip("/") for path in paths if path))
    documents = [path for path in normalized
                 if _documentation_path(path) and not _boundary(path, boundary_patterns)]
    narrow_cases: Dict[str, List[Dict[str, str]]] = {}
    narrow_seeds: Dict[str, Set[str]] = {}
    for change in changes:
        path = str(change.get("path", "")).replace("\\", "/").lstrip("/")
        if str(change.get("status", ""))[:1] == "M":
            hunks = (test_hunks or {}).get(path, ())
            if hunks and repo is not None and base:
                old_text = _revision_text(repo, base, path)
                new_text = _revision_text(repo, head, path)
                cases = (_changed_test_cases(index, path, hunks, old_text, new_text)
                         if old_text is not None and new_text is not None else None)
            else:
                cases = None
            if cases is not None:
                seeds = _case_containment_seeds(index, path, cases)
                if seeds is not None:
                    narrow_cases[path] = cases
                    narrow_seeds[path] = seeds
    seed_paths = [path for path in normalized if path not in narrow_cases]
    containment_seeds = set().union(*narrow_seeds.values()) if narrow_seeds else set()
    # Variable-prefixed path references (for example "$ROOT/tests/foo.py")
    # resolve to a containing node but carry INFERRED confidence. Seed those
    # direct consumers only; traversal remains restricted to EXTRACTED edges.
    path_consumer_seeds = {
        edge["source"]
        for ident in containment_seeds
        for edge in index.ins.get(ident, [])
        if edge.get("relation") in DEFAULT_RELATIONS and edge.get("confidence") == "INFERRED"
        and index.nodes.get(edge["source"], {}).get("path") not in narrow_cases
    }
    # Some parsers model a Python module as a sibling of its definitions, even
    # though import edges target it. Preserve those external consumers without
    # seeding unrelated nodes from the narrowed file.
    module_consumer_seeds = {
        edge["source"]
        for ident, node in index.nodes.items()
        if node.get("path") in narrow_cases and node.get("kind") == "module"
        for edge in index.ins.get(ident, [])
        if edge.get("relation") in DEFAULT_RELATIONS
        and edge.get("confidence") in ("EXTRACTED", "INFERRED")
        and index.nodes.get(edge["source"], {}).get("path") not in narrow_cases
    }
    result = blast_radius(index, seed_paths, depth=None if depth == 0 else depth,
                          confidences=("EXTRACTED",),
                          seed_ids=sorted(containment_seeds | path_consumer_seeds | module_consumer_seeds))
    tests = [path for path in result["tests"] if _runnable_test(path)]
    test_paths = set(tests)
    test_cases = [row for row in result["test_cases"] if row["path"] in test_paths]
    reasons = []
    if not workspace_exact:
        reasons.append("workspace-differs-from-head")
    reasons.extend("unmatched:%s" % path for path in result["unmatched"] if path not in documents)
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
    if any(path not in documents for path in normalized) and not tests:
        reasons.append("no-tests")
    if tests and not reasons:
        # One covered file cannot certify another changed file. Walk from the
        # selected tests once, not a fresh graph traversal per changed path.
        covered = {ident for ident in reached if index.nodes[ident].get("path") in test_paths}
        pending = list(covered)
        while pending:
            for edge in index.out.get(pending.pop(), []):
                target = edge["target"]
                if (edge["relation"] in DEFAULT_RELATIONS and edge.get("confidence") == "EXTRACTED"
                        and target in reached and target not in covered):
                    covered.add(target)
                    pending.append(target)
        covered_paths = {index.nodes[ident].get("path") for ident in covered}
        reasons.extend("no-tests:%s" % path for path in normalized
                       if path not in documents and path not in covered_paths and path not in narrow_cases)
    return {
        "schema": 1,
        "mode": "full" if reasons else "affected",
        "depth": int(depth),
        "paths": normalized,
        "documentation_paths": documents,
        "tests": tests,
        "test_cases": test_cases,
        "unsupported_test_count": len(result["tests"]) - len(tests),
        "confidence_counts": dict(sorted(confidence_counts.items())),
        "ignored_confidence_counts": dict(sorted(ignored_confidence_counts.items())),
        "path_coverage": result["path_coverage"],
        "unmatched": result["unmatched"],
        "reasons": sorted(set(reasons)),
    }
